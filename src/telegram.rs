use worker::*;

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
            payload.as_object_mut().unwrap().insert("reply_markup".to_string(), kb);
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
            console_log!("Telegram Send Error: {}", err_text);
        }
        Ok(())
    }

    /// Keyboard with options for ambiguous entries
    pub fn choose_keyboard() -> serde_json::Value {
        serde_json::json!({
            "keyboard": [
                [
                    {"text": "🩺 Pressure"},
                    {"text": "💸 Cost"},
                    {"text": "❌ Cancel"}
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
}
