use worker::*;

use crate::ai_vision::AiVisionService;
use crate::google::auth::GoogleAuthService;
use crate::operations::OperationsService;
use crate::parser::ParserService;
use crate::state::{PendingPressure, TextTransition, UserState};
use crate::telegram::{self, CallbackQuery, TelegramService, Update};

const STATE_TTL_SECONDS: u64 = 600;

pub async fn handle_update(env: Env, ctx: Context, update_raw: String) -> Result<()> {
    let update: Update = match serde_json::from_str(&update_raw) {
        Ok(update) => update,
        Err(err) => {
            crate::log_event!("warn", "telegram.update.invalid_json", "error={}", err);
            return Ok(());
        }
    };

    let allowed_username = crate::get_env_or_secret(&env, "ALLOWED_USERNAME", "");
    if allowed_username.is_empty() {
        crate::log_event!("error", "config.allowed_username_missing");
        return Ok(());
    }

    if let Some(cq) = update.callback_query {
        if !username_is_allowed(cq.from.username.as_ref(), &allowed_username) {
            crate::log_event!(
                "warn",
                "telegram.access_denied",
                "kind=callback user_id={}",
                cq.from.id
            );
            return Ok(());
        }

        let env_clone = env.clone();
        ctx.wait_until(async move {
            if let Err(e) = handle_callback_query(env_clone, cq).await {
                crate::log_event!("error", "telegram.callback.failed", "error={:?}", e);
            }
        });
        return Ok(());
    }

    if let Some(msg) = update.message {
        let sender = msg.from.as_ref();
        if !username_is_allowed(sender.and_then(|u| u.username.as_ref()), &allowed_username) {
            let user_id = sender.map(|u| u.id).unwrap_or_default();
            crate::log_event!(
                "warn",
                "telegram.access_denied",
                "kind=message user_id={}",
                user_id
            );
            return Ok(());
        }

        let chat_id = msg.chat.id;
        let has_photo = msg.photo.as_ref().is_some_and(|p| !p.is_empty());
        if has_photo {
            crate::log_event!("info", "telegram.photo.received", "chat_id={}", chat_id);
            let env_clone = env.clone();
            ctx.wait_until(async move {
                if let Err(e) = handle_photo(env_clone, chat_id, msg).await {
                    crate::log_event!("error", "telegram.photo.failed", "error={:?}", e);
                }
            });
            return Ok(());
        }

        let text = msg.text.clone().unwrap_or_default().trim().to_string();
        if text.is_empty() {
            crate::log_event!(
                "info",
                "telegram.message.ignored_empty",
                "chat_id={}",
                chat_id
            );
            return Ok(());
        }

        crate::log_event!(
            "info",
            "telegram.text.received",
            "chat_id={} chars={}",
            chat_id,
            text.chars().count()
        );
        let env_clone = env.clone();
        ctx.wait_until(async move {
            if let Err(e) = handle_text(env_clone, chat_id, text).await {
                crate::log_event!("error", "telegram.text.failed", "error={:?}", e);
            }
        });
    }

    Ok(())
}

fn username_is_allowed(username: Option<&String>, allowed_username: &str) -> bool {
    username.map(|u| u.as_str()).unwrap_or_default() == allowed_username
}

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
        crate::log_event!(
            "info",
            "parser.action.detected",
            "chat_id={} action={}",
            chat_id,
            action.kind()
        );
        OperationsService::execute(env, token, bot_token, chat_id, action).await?;
    } else {
        crate::log_event!("info", "parser.action.unknown", "chat_id={}", chat_id);
        let state = UserState::AwaitingClassification {
            raw_text: text.to_string(),
        };
        save_state(kv, state_key, &state).await?;
        TelegramService::send_message(
            bot_token,
            chat_id,
            "Where to save?",
            Some(TelegramService::choose_keyboard()),
        )
        .await?;
    }
    Ok(())
}

async fn handle_text(env: Env, chat_id: i64, text: String) -> Result<()> {
    let bot_token = env.secret("BOT_TOKEN")?.to_string();
    let kv = env.kv("STATE_STORE")?;
    let state_key = state_key(chat_id);

    let state = load_state(&kv, &state_key).await?;
    crate::log_event!(
        "info",
        "state.loaded",
        "chat_id={} state={}",
        chat_id,
        state_name(&state)
    );

    let transition = state.text_transition(&text);
    if transition == TextTransition::Cancel {
        delete_state(&kv, &state_key, chat_id).await?;
        TelegramService::remove_keyboard_silently(&bot_token, chat_id).await?;
        return Ok(());
    }

    let token = match GoogleAuthService::get_token(&env).await {
        Ok(token) => token,
        Err(e) => {
            crate::log_event!(
                "error",
                "google.auth.failed",
                "chat_id={} error={}",
                chat_id,
                e
            );
            TelegramService::send_message(
                &bot_token,
                chat_id,
                &format!("❌ Google Auth Error: {}", e),
                Some(TelegramService::remove_keyboard()),
            )
            .await?;
            return Err(e);
        }
    };

    match transition {
        TextTransition::Cancel => unreachable!("cancel transition is handled before auth"),
        TextTransition::SavePressure(pending) => {
            delete_state(&kv, &state_key, chat_id).await?;
            OperationsService::execute(&env, &token, &bot_token, chat_id, pending.into()).await?;
        }
        TextTransition::ForcePressure { raw_text } => {
            delete_state(&kv, &state_key, chat_id).await?;
            if let Some(action) = ParserService::parse_manual_pressure(&raw_text) {
                OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
            } else {
                TelegramService::send_message(
                    &bot_token,
                    chat_id,
                    "❌ Need at least 2 numbers: sys dia",
                    Some(TelegramService::remove_keyboard()),
                )
                .await?;
            }
        }
        TextTransition::ForceCost { raw_text } => {
            delete_state(&kv, &state_key, chat_id).await?;
            if let Some(action) = ParserService::parse_manual_cost(&raw_text) {
                OperationsService::execute(&env, &token, &bot_token, chat_id, action).await?;
            } else {
                TelegramService::send_message(
                    &bot_token,
                    chat_id,
                    "❌ Invalid cost format",
                    Some(TelegramService::remove_keyboard()),
                )
                .await?;
            }
        }
        TextTransition::ProcessFresh { discard_existing } => {
            if discard_existing {
                delete_state(&kv, &state_key, chat_id).await?;
            }
            process_text_flow(&env, &token, &bot_token, &kv, &state_key, chat_id, &text).await?;
        }
    }

    Ok(())
}

async fn handle_callback_query(env: Env, cq: CallbackQuery) -> Result<()> {
    let bot_token = env.secret("BOT_TOKEN")?.to_string();
    TelegramService::answer_callback_query(&bot_token, &cq.id, None).await?;

    let Some(message) = cq.message.as_ref() else {
        crate::log_event!("warn", "telegram.callback.missing_message");
        return Ok(());
    };

    let chat_id = message.chat.id;
    let data = cq.data.unwrap_or_default();
    let kv = env.kv("STATE_STORE")?;
    let state_key = state_key(chat_id);
    let state = load_state(&kv, &state_key).await?;

    crate::log_event!(
        "info",
        "telegram.callback.received",
        "chat_id={} data={}",
        chat_id,
        data
    );

    if data == "confirm_pressure" {
        if let UserState::AwaitingPressureConfirmation(pending) = state {
            save_pressure_from_callback(&env, &bot_token, &kv, &state_key, chat_id, pending)
                .await?;
        } else {
            TelegramService::send_message(
                &bot_token,
                chat_id,
                "❌ No pending pressure data found.",
                Some(TelegramService::remove_keyboard()),
            )
            .await?;
        }
    } else if data == "cancel_pressure" || data == "cancel_option" {
        delete_state(&kv, &state_key, chat_id).await?;
        TelegramService::send_message(
            &bot_token,
            chat_id,
            "❌ Cancelled.",
            Some(TelegramService::remove_keyboard()),
        )
        .await?;
    } else if let Some(option_index) = data.strip_prefix("select_option_") {
        if let UserState::AwaitingMultipleChoice { options } = state {
            if let Ok(index) = option_index.parse::<usize>() {
                if let Some(pending) = options.get(index) {
                    save_pressure_from_callback(
                        &env,
                        &bot_token,
                        &kv,
                        &state_key,
                        chat_id,
                        pending.clone(),
                    )
                    .await?;
                } else {
                    TelegramService::send_message(
                        &bot_token,
                        chat_id,
                        "❌ Invalid option selected.",
                        Some(TelegramService::remove_keyboard()),
                    )
                    .await?;
                }
            } else {
                TelegramService::send_message(
                    &bot_token,
                    chat_id,
                    "❌ Invalid option format.",
                    Some(TelegramService::remove_keyboard()),
                )
                .await?;
            }
        } else {
            TelegramService::send_message(
                &bot_token,
                chat_id,
                "❌ No multiple choice data found.",
                Some(TelegramService::remove_keyboard()),
            )
            .await?;
        }
    }

    Ok(())
}

async fn save_pressure_from_callback(
    env: &Env,
    bot_token: &str,
    kv: &kv::KvStore,
    state_key: &str,
    chat_id: i64,
    pending: PendingPressure,
) -> Result<()> {
    delete_state(kv, state_key, chat_id).await?;
    let token = GoogleAuthService::get_token(env).await?;
    OperationsService::execute(env, &token, bot_token, chat_id, pending.into()).await
}

async fn handle_photo(env: Env, chat_id: i64, msg: telegram::Message) -> Result<()> {
    let bot_token = env.secret("BOT_TOKEN")?.to_string();
    let kv = env.kv("STATE_STORE")?;
    let state_key = state_key(chat_id);

    let photos = msg.photo.unwrap_or_default();
    let photo = photos
        .last()
        .ok_or_else(|| worker::Error::from("No photos found"))?;

    crate::log_event!(
        "info",
        "telegram.photo.processing",
        "chat_id={} width={} height={} size_bytes={}",
        chat_id,
        photo.width,
        photo.height,
        photo.file_size.unwrap_or_default()
    );

    let file_path = match TelegramService::get_file_path(&bot_token, &photo.file_id).await {
        Ok(file_path) => file_path,
        Err(e) => {
            crate::log_event!(
                "error",
                "telegram.photo.file_path_failed",
                "chat_id={} error={:?}",
                chat_id,
                e
            );
            TelegramService::send_message(
                &bot_token,
                chat_id,
                "❌ Failed to retrieve photo from Telegram. Please try again.",
                Some(TelegramService::remove_keyboard()),
            )
            .await?;
            return Err(e);
        }
    };

    let image_bytes = match TelegramService::download_file(&bot_token, &file_path).await {
        Ok(image_bytes) => image_bytes,
        Err(e) => {
            crate::log_event!(
                "error",
                "telegram.photo.download_failed",
                "chat_id={} error={:?}",
                chat_id,
                e
            );
            TelegramService::send_message(
                &bot_token,
                chat_id,
                "❌ Failed to download photo from Telegram. Please try again.",
                Some(TelegramService::remove_keyboard()),
            )
            .await?;
            return Err(e);
        }
    };

    crate::log_event!(
        "info",
        "telegram.photo.downloaded",
        "chat_id={} bytes={}",
        chat_id,
        image_bytes.len()
    );

    let ai_responses = match AiVisionService::recognize_pressure_batch(&env, &image_bytes).await {
        Ok(responses) => responses,
        Err(e) => {
            crate::log_event!(
                "error",
                "ai_vision.recognition.failed",
                "chat_id={} error={:?}",
                chat_id,
                e
            );
            TelegramService::send_message(
                &bot_token,
                chat_id,
                "❌ AI recognition failed. Please try again or enter pressure manually.",
                Some(TelegramService::remove_keyboard()),
            )
            .await?;
            return Err(e);
        }
    };

    let unique_options = unique_pressure_options(&ai_responses);
    crate::log_event!(
        "info",
        "ai_vision.recognition.parsed",
        "chat_id={} responses={} unique_options={}",
        chat_id,
        ai_responses.len(),
        unique_options.len()
    );

    match unique_options.len() {
        0 => {
            TelegramService::send_message(
                &bot_token,
                chat_id,
                "❌ Could not recognize pressure numbers in image. Please enter pressure manually.",
                Some(TelegramService::remove_keyboard()),
            )
            .await?;
        }
        1 => {
            let pending = &unique_options[0];
            let mut msg = format!("📊 Recognized: {}/{}", pending.sys, pending.dia);
            if let Some(p) = pending.pulse {
                msg.push_str(&format!(" pulse {}", p));
            }
            msg.push_str("\n\nSave?");

            save_state(
                &kv,
                &state_key,
                &UserState::AwaitingPressureConfirmation(pending.clone()),
            )
            .await?;

            TelegramService::send_message(
                &bot_token,
                chat_id,
                &msg,
                Some(TelegramService::confirm_keyboard()),
            )
            .await?;
        }
        _ => {
            let msg = multiple_choice_message(&unique_options);
            save_state(
                &kv,
                &state_key,
                &UserState::AwaitingMultipleChoice {
                    options: unique_options.clone(),
                },
            )
            .await?;

            let inline_kb = TelegramService::choice_keyboard(unique_options.len());
            TelegramService::send_inline_message(&bot_token, chat_id, &msg, inline_kb).await?;
        }
    }

    Ok(())
}

fn unique_pressure_options(ai_responses: &[String]) -> Vec<PendingPressure> {
    let mut unique_options = Vec::new();
    for response_text in ai_responses {
        if let Some((sys, dia, pulse)) = ParserService::parse_ai_pressure_response(response_text) {
            let pending = PendingPressure { sys, dia, pulse };
            if !unique_options.contains(&pending) {
                unique_options.push(pending);
            }
        }
    }
    unique_options
}

fn multiple_choice_message(options: &[PendingPressure]) -> String {
    let mut msg_parts = vec!["📊 Multiple options found:".to_string()];
    for (i, opt) in options.iter().enumerate() {
        let mut opt_str = format!("{}️⃣  {}/{}", i + 1, opt.sys, opt.dia);
        if let Some(p) = opt.pulse {
            opt_str.push_str(&format!(" pulse {}", p));
        }
        msg_parts.push(opt_str);
    }
    msg_parts.push("\nChoose the correct one:".to_string());
    msg_parts.join("\n")
}

fn state_key(chat_id: i64) -> String {
    format!("{}_state", chat_id)
}

fn state_name(state: &UserState) -> &'static str {
    match state {
        UserState::None => "none",
        UserState::AwaitingClassification { .. } => "awaiting_classification",
        UserState::AwaitingPressureConfirmation(_) => "awaiting_pressure_confirmation",
        UserState::AwaitingMultipleChoice { .. } => "awaiting_multiple_choice",
    }
}

async fn load_state(kv: &kv::KvStore, state_key: &str) -> Result<UserState> {
    let Some(state_str) = kv.get(state_key).text().await? else {
        return Ok(UserState::None);
    };

    Ok(UserState::parse_or_none(&state_str))
}

async fn save_state(kv: &kv::KvStore, state_key: &str, state: &UserState) -> Result<()> {
    let state_json = serde_json::to_string(state)?;
    kv.put(state_key, &state_json)?
        .expiration_ttl(STATE_TTL_SECONDS)
        .execute()
        .await?;
    crate::log_event!("info", "state.saved", "state={}", state_name(state));
    Ok(())
}

async fn delete_state(kv: &kv::KvStore, state_key: &str, chat_id: i64) -> Result<()> {
    kv.delete(state_key).await?;
    crate::log_event!("info", "state.deleted", "chat_id={}", chat_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_pressure_options_should_deduplicate_valid_ai_responses() {
        let responses = vec![
            r#"{"sys": 120, "dia": 80, "pulse": 70}"#.to_string(),
            r#"{"sys": 120, "dia": 80, "pulse": 70}"#.to_string(),
            r#"{"sys": 121, "dia": 81, "pulse": 71}"#.to_string(),
        ];

        assert_eq!(
            unique_pressure_options(&responses),
            vec![
                PendingPressure {
                    sys: 120,
                    dia: 80,
                    pulse: Some(70)
                },
                PendingPressure {
                    sys: 121,
                    dia: 81,
                    pulse: Some(71)
                }
            ]
        );
    }

    #[test]
    fn unique_pressure_options_should_ignore_invalid_ranges() {
        let responses = vec![r#"{"sys": 300, "dia": 80, "pulse": 70}"#.to_string()];

        assert!(unique_pressure_options(&responses).is_empty());
    }

    #[test]
    fn multiple_choice_message_should_include_all_options() {
        let options = vec![
            PendingPressure {
                sys: 120,
                dia: 80,
                pulse: None,
            },
            PendingPressure {
                sys: 121,
                dia: 81,
                pulse: Some(71),
            },
        ];

        let message = multiple_choice_message(&options);

        assert!(message.contains("120/80"));
        assert!(message.contains("121/81 pulse 71"));
    }

    #[test]
    fn state_key_should_be_scoped_by_chat_id() {
        assert_eq!(state_key(42), "42_state");
    }
}
