use worker::*;
use serde::{Deserialize, Serialize};

mod parser;
mod google;
mod telegram;
mod operations;
mod ai_vision;

use parser::ParserService;
use google::GoogleSheetsService;
use telegram::{Update, TelegramService, CallbackQuery};
use operations::OperationsService;
use ai_vision::AiVisionService;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PendingPressure {
    pub sys: i32,
    pub dia: i32,
    pub pulse: Option<i32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum UserState {
    None,
    AwaitingClassification { raw_text: String },
    AwaitingPressureConfirmation(PendingPressure),
}

pub(crate) fn get_env_or_secret(env: &Env, name: &str, default: &str) -> String {
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
            return err_res.try_into();
        }
    };

    let path = req.path();
    let path_clean = path.trim_end_matches('/');
    let method = req.method().to_string();

    // Friendly GET landing check to confirm bot status in web browsers
    if method == "GET" && (path_clean == "/webhook" || path_clean.is_empty()) {
        let res = Response::ok("🤖 Pressure Bot is running! Please send POST requests via Telegram webhooks.")?;
        return res.try_into();
    }

    if method != "POST" || path_clean != "/webhook" {
        let err_res = Response::error("Not Found", 404)?;
        return err_res.try_into();
    }

    // 2. Parse Telegram Update
    let update_raw = req.text().await?;
    console_log!("RAW UPDATE: {}", update_raw);
    
    // Re-parse from raw text
    let update: Update = match serde_json::from_str(&update_raw) {
        Ok(upd) => upd,
        Err(err) => {
            console_log!("JSON Parse Error: {:?}, raw: {}", err, update_raw);
            // Always return OK 200 to Telegram so it doesn't keep retrying failed webhook payloads
            let res = Response::empty()?;
            return res.try_into();
        }
    };

    let allowed_username = get_env_or_secret(&env, "ALLOWED_USERNAME", "");
    if allowed_username.is_empty() {
        console_log!("ALLOWED_USERNAME binding is missing!");
        let res = Response::empty()?;
        return res.try_into();
    }

    // Access control helper
    let check_access = |username: Option<&String>| -> bool {
        username.map(|u| u.as_str()).unwrap_or("") == allowed_username
    };

    // 3. Handle callback_query
    if let Some(cq) = update.callback_query {
        if !check_access(cq.from.username.as_ref()) {
            console_log!("ACCESS DENIED: user '{:?}' is not allowed", cq.from.username);
            let res = Response::empty()?;
            return res.try_into();
        }

        let env_clone = env.clone();
        ctx.wait_until(async move {
            if let Err(e) = handle_callback_query(env_clone, cq).await {
                console_log!("Callback query error: {:?}", e);
            }
        });

        let res = Response::empty()?;
        return res.try_into();
    }

    // 4. Handle regular message
    if let Some(msg) = update.message {
        // Access Control
        let sender_username = msg.from.as_ref()
            .and_then(|u| u.username.as_ref())
            .cloned()
            .unwrap_or_default();

        if sender_username != allowed_username {
            console_log!("ACCESS DENIED: user '{}' is not allowed", sender_username);
            let res = Response::empty()?;
            return res.try_into();
        }

        let chat_id = msg.chat.id;
        let has_photo = msg.photo.as_ref().map(|p| !p.is_empty()).unwrap_or(false);

        // 4a. Handle photo message FIRST (even if text is empty)
        if has_photo {
            let env_clone = env.clone();
            ctx.wait_until(async move {
                if let Err(e) = handle_photo(env_clone, chat_id, msg).await {
                    console_log!("Photo processing error: {:?}", e);
                }
            });

            let res = Response::empty()?;
            return res.try_into();
        }

        // 4b. Handle text
        let text = msg.text.clone().unwrap_or_default().trim().to_string();
        if text.is_empty() {
            let res = Response::empty()?;
            return res.try_into();
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
    res.try_into()
}

/// Helper to process regular text flow when there is no pending state or a state is overridden/cancelled
async fn process_text_flow(
    env: &Env,
    token: &str,
    bot_token: &str,
    kv: &kv::KvStore,
    state_key: &str,
    chat_id: i64,
    text: &str,
) -> Result<()> {
    if let Some(action) = ParserService::detect_action(text) {
        console_log!("DETECTED: {:?}", action);
        OperationsService::execute(env, token, bot_token, chat_id, action).await?;
    } else {
        console_log!("DETECTED: unknown");
        // Unknown action: save to KV and offer menu
        let state = UserState::AwaitingClassification { raw_text: text.to_string() };
        let state_json = serde_json::to_string(&state)?;
        console_log!("Saving unknown text state: key={}, val={}", state_key, state_json);
        kv.put(state_key, &state_json)?
            .expiration_ttl(600)
            .execute()
            .await?;
        TelegramService::send_message(bot_token, chat_id, "Where to save?", Some(TelegramService::choose_keyboard())).await?;
    }
    Ok(())
}

/// Orchestrates the state processing and operations flow in a background task
async fn handle_webhook_background(env: Env, chat_id: i64, text: String) -> Result<()> {
    let bot_token = env.secret("BOT_TOKEN")?.to_string();
    let kv = env.kv("STATE_STORE")?;
    let state_key = format!("{}_state", chat_id);

    console_log!("Background webhook task: chat_id={}, text='{}'", chat_id, text);

    // Cancel from keyboard is universal and handled first
    if text == telegram::BTN_CANCEL {
        console_log!("Handling universal CANCEL, deleting state key: {}", state_key);
        kv.delete(&state_key).await?;
        return Ok(());
    }

    // Fetch Google Token safely with visual error reporting
    let token = match GoogleSheetsService::get_token(&env).await {
        Ok(t) => t,
        Err(e) => {
            console_log!("get_google_token failed: {:?}", e);
            TelegramService::send_message(&bot_token, chat_id, &format!("❌ Google Auth Error: {}", e), Some(TelegramService::remove_keyboard())).await?;
            return Err(e);
        }
    };

    // Retrieve and manually deserialize the typed state from KV
    let state: UserState = if let Some(state_str) = kv.get(&state_key).text().await? {
        console_log!("Retrieved state string: {}", state_str);
        serde_json::from_str(&state_str).unwrap_or(UserState::None)
    } else {
        UserState::None
    };
    console_log!("Active parsed state: {:?}", state);

    match state {
        UserState::AwaitingPressureConfirmation(pending) => {
            if text == telegram::BTN_SAVE {
                console_log!("Saving pending pressure: {:?}", pending);
                kv.delete(&state_key).await?;
                let action = parser::Action::Pressure {
                    sys: pending.sys,
                    dia: pending.dia,
                    pulse: pending.pulse,
                };
                OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
            } else {
                // Any other input: discard state and parse this input fresh
                console_log!("Input is not Save, discarding pending pressure and processing new text.");
                kv.delete(&state_key).await?;
                process_text_flow(&env, &token, &bot_token, &kv, &state_key, chat_id, &text).await?;
            }
        }
        UserState::AwaitingClassification { raw_text } => {
            if text == telegram::BTN_PRESSURE {
                console_log!("Forced pressure classification for raw text: {}", raw_text);
                kv.delete(&state_key).await?;
                if let Some(action) = ParserService::parse_manual_pressure(&raw_text) {
                    OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
                } else {
                    TelegramService::send_message(&bot_token, chat_id, "❌ Need at least 2 numbers: sys dia", Some(TelegramService::remove_keyboard())).await?;
                }
            } else if text == telegram::BTN_COST {
                console_log!("Forced cost classification for raw text: {}", raw_text);
                kv.delete(&state_key).await?;
                if let Some(action) = ParserService::parse_manual_cost(&raw_text) {
                    OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
                } else {
                    TelegramService::send_message(&bot_token, chat_id, "❌ Invalid cost format", Some(TelegramService::remove_keyboard())).await?;
                }
            } else {
                // Discard state and process this input fresh
                console_log!("Input is not a menu choice, discarding state and processing new text.");
                kv.delete(&state_key).await?;
                process_text_flow(&env, &token, &bot_token, &kv, &state_key, chat_id, &text).await?;
            }
        }
        UserState::None => {
            process_text_flow(&env, &token, &bot_token, &kv, &state_key, chat_id, &text).await?;
        }
    }

    Ok(())
}

/// Handles a callback_query (from inline keyboard buttons)
async fn handle_callback_query(env: Env, cq: CallbackQuery) -> Result<()> {
    let bot_token = env.secret("BOT_TOKEN")?.to_string();
    let kv = env.kv("STATE_STORE")?;

    // Answer callback query immediately to stop loading indicator
    TelegramService::answer_callback_query(&bot_token, &cq.id, None).await?;

    let chat_id = cq.message.as_ref().map(|m| m.chat.id).unwrap_or(0);
    let data = cq.data.unwrap_or_default();
    let state_key = format!("{}_state", chat_id);

    console_log!("Callback query: chat_id={}, data='{}'", chat_id, data);

    // Retrieve and manually deserialize the typed state from KV
    let state: UserState = if let Some(state_str) = kv.get(&state_key).text().await? {
        console_log!("Callback query retrieved state string: {}", state_str);
        serde_json::from_str(&state_str).unwrap_or(UserState::None)
    } else {
        UserState::None
    };
    console_log!("Callback query active parsed state: {:?}", state);

    if data == "confirm_pressure" {
        if let UserState::AwaitingPressureConfirmation(pending) = state {
            kv.delete(&state_key).await?;
            let token = GoogleSheetsService::get_token(&env).await?;
            let action = parser::Action::Pressure {
                sys: pending.sys,
                dia: pending.dia,
                pulse: pending.pulse,
            };
            OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
        } else {
            TelegramService::send_message(&bot_token, chat_id, "❌ No pending pressure data found.", Some(TelegramService::remove_keyboard())).await?;
        }
    } else if data == "cancel_pressure" {
        kv.delete(&state_key).await?;
    }

    Ok(())
}

/// Handles photo messages: download → AI recognition → show result → confirm
async fn handle_photo(env: Env, chat_id: i64, msg: telegram::Message) -> Result<()> {
    let bot_token = env.secret("BOT_TOKEN")?.to_string();
    let kv = env.kv("STATE_STORE")?;
    let state_key = format!("{}_state", chat_id);

    console_log!("Photo processing task for chat_id={}", chat_id);

    // 1. Get the highest resolution photo for precise OCR of thin 7-segment display digits
    let photos = msg.photo.unwrap_or_default();
    let photo = photos.last().ok_or_else(|| worker::Error::from("No photos found"))?;

    console_log!("Processing photo: file_id={} size={:?}", photo.file_id, photo.file_size);

    // 2. Get file path and download
    let file_path = TelegramService::get_file_path(&bot_token, &photo.file_id).await?;
    let image_bytes = TelegramService::download_file(&bot_token, &file_path).await?;

    console_log!("Downloaded photo: {} bytes", image_bytes.len());

    // 3. Call Workers AI via AiVisionService
    let ai_text = match AiVisionService::recognize_pressure(&env, &image_bytes).await {
        Ok(t) => t,
        Err(e) => {
            console_log!("AI recognition error: {:?}", e);
            TelegramService::send_message(&bot_token, chat_id, "❌ AI recognition failed. Please try again or enter pressure manually.", Some(TelegramService::remove_keyboard())).await?;
            return Err(e);
        }
    };

    // 4. Parse pressure from AI response
    if let Some((sys, dia, pulse)) = ParserService::parse_ai_pressure_response(&ai_text) {
        let mut msg = format!("📊 Recognized: {}/{}", sys, dia);
        if let Some(p) = pulse {
            msg.push_str(&format!(" pulse {}", p));
        }
        msg.push_str("\n\nSave?");

        // Save pending pressure state as UserState
        let state = UserState::AwaitingPressureConfirmation(PendingPressure { sys, dia, pulse });
        let state_json = serde_json::to_string(&state)?;
        console_log!("Saving pending pressure state: key={}, val={}", state_key, state_json);
        kv.put(&state_key, &state_json)?
            .expiration_ttl(600)
            .execute()
            .await?;

        TelegramService::send_message(&bot_token, chat_id, &msg, Some(TelegramService::confirm_keyboard())).await?;
    } else {
        console_log!("Could not parse pressure from AI response: {}", ai_text);
        TelegramService::send_message(&bot_token, chat_id,
            &format!("❌ Could not recognize pressure numbers in image.\nAI said: {}\n\nPlease enter pressure manually.", ai_text),
            Some(TelegramService::remove_keyboard()),
        ).await?;
    }

    Ok(())
}
