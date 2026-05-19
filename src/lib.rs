use worker::*;

mod parser;
mod google;
mod telegram;
mod operations;

use parser::ParserService;
use google::GoogleSheetsService;
use telegram::{Update, TelegramService};
use operations::OperationsService;

fn get_env_or_secret(env: &Env, name: &str, default: &str) -> String {
    env.secret(name)
        .map(|v| v.to_string())
        .or_else(|_| env.var(name).map(|v| v.to_string()))
        .unwrap_or_else(|_| default.to_string())
}

#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    ctx: Context,
) -> Result<HttpResponse> {
    // 1. Convert standard HttpRequest to worker::Request
    let mut req = match worker::Request::try_from(req) {
        Ok(r) => r,
        Err(e) => {
            console_log!("Request conversion error: {:?}", e);
            let err_res = Response::error("Bad Request", 400)?;
            return Ok(err_res.try_into()?);
        }
    };

    let path = req.path();
    let path_clean = path.trim_end_matches('/');
    let method = req.method().to_string();

    // Friendly GET landing check to confirm bot status in web browsers
    if method == "GET" && (path_clean == "/webhook" || path_clean == "") {
        let res = Response::ok("🤖 Pressure Bot is running! Please send POST requests via Telegram webhooks.")?;
        return Ok(res.try_into()?);
    }

    if method != "POST" || path_clean != "/webhook" {
        let err_res = Response::error("Not Found", 404)?;
        return Ok(err_res.try_into()?);
    }

    // 2. Parse Telegram Update
    let update: Update = match req.json::<Update>().await {
        Ok(upd) => upd,
        Err(err) => {
            console_log!("JSON Parse Error: {:?}", err);
            // Always return OK 200 to Telegram so it doesn't keep retrying failed webhook payloads
            let res = Response::empty()?;
            return Ok(res.try_into()?);
        }
    };

    if let Some(msg) = update.message {
        let allowed_username = get_env_or_secret(&env, "ALLOWED_USERNAME", "");
        if allowed_username.is_empty() {
            console_log!("ALLOWED_USERNAME binding is missing!");
            let res = Response::empty()?;
            return Ok(res.try_into()?);
        }

        // Access Control
        let sender_username = msg.from.as_ref()
            .and_then(|u| u.username.as_ref())
            .cloned()
            .unwrap_or_default();

        if sender_username != allowed_username {
            console_log!("ACCESS DENIED: user '{}' is not allowed", sender_username);
            let res = Response::empty()?;
            return Ok(res.try_into()?);
        }

        let chat_id = msg.chat.id;
        let text = msg.text.clone().unwrap_or_default().trim().to_string();
        if text.is_empty() {
            let res = Response::empty()?;
            return Ok(res.try_into()?);
        }

        console_log!("INPUT TEXT: {}", text);

        // Spin up asynchronous background task using wait_until
        // and return 200 OK immediately!
        let env_clone = env.clone();
        ctx.wait_until(async move {
            if let Err(e) = handle_webhook_background(env_clone, chat_id, text).await {
                console_log!("Background task error: {:?}", e);
            }
        });
    }

    let res = Response::empty()?;
    Ok(res.try_into()?)
}

/// Orchestrates the state processing and operations flow in a background task
async fn handle_webhook_background(env: Env, chat_id: i64, text: String) -> Result<()> {
    let bot_token = env.secret("BOT_TOKEN")?.to_string();
    let kv = env.kv("STATE_STORE")?;
    let chat_key = chat_id.to_string();

    // Fetch Google Token safely with visual error reporting
    let token = match GoogleSheetsService::get_token(&env).await {
        Ok(t) => t,
        Err(e) => {
            console_log!("get_google_token failed: {:?}", e);
            TelegramService::send_message(&bot_token, chat_id, &format!("❌ Google Auth Error: {}", e), Some(TelegramService::remove_keyboard())).await?;
            return Err(e);
        }
    };

    // 1. Process Pending Action from KV store
    if let Some(pending_text) = kv.get(&chat_key).text().await? {
        match text.as_str() {
            "🩺 Pressure" => {
                kv.delete(&chat_key).await?;
                if let Some(action) = ParserService::parse_manual_pressure(&pending_text) {
                    OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
                } else {
                    TelegramService::send_message(&bot_token, chat_id, "❌ Need at least 2 numbers: sys dia", Some(TelegramService::remove_keyboard())).await?;
                }
                return Ok(());
            }
            "💸 Cost" => {
                kv.delete(&chat_key).await?;
                if let Some(action) = ParserService::parse_manual_cost(&pending_text) {
                    OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
                } else {
                    TelegramService::send_message(&bot_token, chat_id, "❌ Invalid cost format", Some(TelegramService::remove_keyboard())).await?;
                }
                return Ok(());
            }
            "❌ Cancel" => {
                kv.delete(&chat_key).await?;
                TelegramService::send_message(&bot_token, chat_id, "❌ Canceled", Some(TelegramService::remove_keyboard())).await?;
                return Ok(());
            }
            _ => {}
        }
    }

    // 2. Default classification flow
    if let Some(action) = ParserService::detect_action(&text) {
        console_log!("DETECTED: {:?}", action);
        OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
    } else {
        console_log!("DETECTED: unknown");
        // Unknown action: save to KV and offer menu
        kv.put(&chat_key, &text)?.expiration_ttl(600).execute().await?;
        TelegramService::send_message(&bot_token, chat_id, "Where to save?", Some(TelegramService::choose_keyboard())).await?;
    }

    Ok(())
}