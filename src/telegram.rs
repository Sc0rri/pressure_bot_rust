use serde::{Deserialize, Serialize};
use worker::*;

pub const BTN_SAVE: &str = "✅ Save";
pub const BTN_CANCEL: &str = "❌ Cancel";
pub const BTN_PRESSURE: &str = "🩺 Pressure";
pub const BTN_COST: &str = "💸 Cost";

// Telegram Update models
#[derive(Deserialize, Serialize, Debug)]
pub struct Update {
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Chat {
    pub id: i64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PhotoSize {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: i64,
    pub height: i64,
    pub file_size: Option<i64>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Message {
    pub text: Option<String>,
    pub chat: Chat,
    pub from: Option<User>,
    pub photo: Option<Vec<PhotoSize>>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub message: Option<Message>,
    pub data: Option<String>,
}

#[derive(Deserialize)]
struct GetFileResponse {
    #[allow(dead_code)]
    ok: bool,
    result: Option<GetFileResult>,
}

#[derive(Deserialize)]
struct GetFileResult {
    file_path: Option<String>,
}

pub struct TelegramService;

impl TelegramService {
    /// Sends a text message to a specific Telegram chat, optionally displaying a reply keyboard
    pub async fn send_message(
        bot_token: &str,
        chat_id: i64,
        text: &str,
        keyboard: Option<serde_json::Value>,
    ) -> Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });
        if let Some(kb) = keyboard {
            payload
                .as_object_mut()
                .unwrap()
                .insert("reply_markup".to_string(), kb);
        }

        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;

        let mut req_init = RequestInit::new();
        req_init.with_method(Method::Post);
        req_init.with_headers(headers);
        req_init.with_body(Some(serde_json::to_string(&payload)?.into()));

        let req = Request::new_with_init(&url, &req_init)?;
        let mut resp = Fetch::Request(req).send().await?;
        if resp.status_code() != 200 {
            let err_text = resp.text().await?;
            crate::log_event!(
                "warn",
                "telegram.send_message.failed",
                "body_chars={}",
                err_text.chars().count()
            );
        }
        Ok(())
    }

    /// Sends a message with an inline keyboard markup (not a reply keyboard)
    pub async fn send_inline_message(
        bot_token: &str,
        chat_id: i64,
        text: &str,
        inline_markup: serde_json::Value,
    ) -> Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "reply_markup": inline_markup,
        });

        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;

        let mut req_init = RequestInit::new();
        req_init.with_method(Method::Post);
        req_init.with_headers(headers);
        req_init.with_body(Some(serde_json::to_string(&payload)?.into()));

        let req = Request::new_with_init(&url, &req_init)?;
        let mut resp = Fetch::Request(req).send().await?;
        if resp.status_code() != 200 {
            let err_text = resp.text().await?;
            crate::log_event!(
                "warn",
                "telegram.send_inline_message.failed",
                "body_chars={}",
                err_text.chars().count()
            );
        }
        Ok(())
    }

    pub fn choose_keyboard() -> serde_json::Value {
        serde_json::json!({
            "keyboard": [
                [
                    {"text": BTN_PRESSURE},
                    {"text": BTN_COST},
                    {"text": BTN_CANCEL}
                ]
            ],
            "one_time_keyboard": true,
            "resize_keyboard": true
        })
    }

    /// Resilient keyboard removal payload
    pub fn remove_keyboard() -> serde_json::Value {
        serde_json::json!({
            "remove_keyboard": true
        })
    }

    pub fn confirm_keyboard() -> serde_json::Value {
        serde_json::json!({
            "keyboard": [
                [
                    {"text": BTN_SAVE},
                    {"text": BTN_CANCEL}
                ]
            ],
            "one_time_keyboard": true,
            "resize_keyboard": true
        })
    }

    /// Generates an inline keyboard for choosing between multiple pressure options.
    /// Each option gets a button, plus a final Cancel button.
    pub fn choice_keyboard(options_count: usize) -> serde_json::Value {
        let mut rows: Vec<Vec<serde_json::Value>> = Vec::new();

        for i in 0..options_count {
            let label = format!("Вариант {}", i + 1);
            let data = format!("select_option_{}", i);
            rows.push(vec![serde_json::json!({
                "text": label,
                "callback_data": data
            })]);
        }

        // Cancel button
        rows.push(vec![serde_json::json!({
            "text": BTN_CANCEL,
            "callback_data": "cancel_option"
        })]);

        serde_json::json!({
            "inline_keyboard": rows
        })
    }

    /// Gets file_path for a given file_id from Telegram Bot API
    pub async fn get_file_path(bot_token: &str, file_id: &str) -> Result<String> {
        let url = format!("https://api.telegram.org/bot{}/getFile", bot_token);
        let payload = serde_json::json!({
            "file_id": file_id
        });

        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;

        let mut req_init = RequestInit::new();
        req_init.with_method(Method::Post);
        req_init.with_headers(headers);
        req_init.with_body(Some(serde_json::to_string(&payload)?.into()));

        let req = Request::new_with_init(&url, &req_init)?;
        let mut resp = Fetch::Request(req).send().await?;
        let status = resp.status_code();
        let body = resp.text().await?;

        if status != 200 {
            return Err(worker::Error::from(format!("getFile failed: {}", body)));
        }

        let get_file_resp: GetFileResponse = serde_json::from_str(&body)
            .map_err(|e| worker::Error::from(format!("getFile parse error: {}", e)))?;

        let file_path = get_file_resp
            .result
            .and_then(|r| r.file_path)
            .ok_or_else(|| worker::Error::from("No file_path in getFile response"))?;

        Ok(file_path)
    }

    /// Answers a callback query (removes the "loading" state on the button)
    pub async fn answer_callback_query(
        bot_token: &str,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<()> {
        let url = format!(
            "https://api.telegram.org/bot{}/answerCallbackQuery",
            bot_token
        );
        let mut payload = serde_json::json!({
            "callback_query_id": callback_query_id,
        });
        if let Some(t) = text {
            payload
                .as_object_mut()
                .unwrap()
                .insert("text".to_string(), serde_json::Value::String(t.to_string()));
        }

        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;

        let mut req_init = RequestInit::new();
        req_init.with_method(Method::Post);
        req_init.with_headers(headers);
        req_init.with_body(Some(serde_json::to_string(&payload)?.into()));

        let req = Request::new_with_init(&url, &req_init)?;
        let mut resp = Fetch::Request(req).send().await?;
        if resp.status_code() != 200 {
            let err_text = resp.text().await?;
            crate::log_event!(
                "warn",
                "telegram.answer_callback_query.failed",
                "body_chars={}",
                err_text.chars().count()
            );
        }
        Ok(())
    }

    /// Downloads a file from Telegram file server and returns its bytes
    pub async fn download_file(bot_token: &str, file_path: &str) -> Result<Vec<u8>> {
        let url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            bot_token, file_path
        );

        let req = Request::new(&url, Method::Get)?;
        let mut resp = Fetch::Request(req).send().await?;

        if resp.status_code() != 200 {
            let err_text = resp.text().await?;
            return Err(worker::Error::from(format!(
                "download file failed: {}",
                err_text
            )));
        }

        let bytes = resp.bytes().await?;
        Ok(bytes)
    }
}
