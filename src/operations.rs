use crate::get_env_or_secret;
use crate::google::GoogleSheetsService;
use crate::parser::Action;
use crate::telegram::TelegramService;
use worker::*;

pub struct OperationsService;

impl OperationsService {
    /// Dispatches and executes parsed actions against Google Sheets and notifies the user
    pub async fn execute(
        env: &Env,
        token: &str,
        bot_token: &str,
        chat_id: i64,
        action: Action,
    ) -> Result<()> {
        let sheet_id = env.secret("SHEET_ID")?.to_string();
        let tz_str = get_env_or_secret(env, "TIMEZONE", "Europe/Kiev");

        match action {
            Action::Pressure { sys, dia, pulse } => {
                let pressure_sheet = get_env_or_secret(env, "PRESSURE_SHEET", "pressure");
                let pressure_sheet_id: i64 = get_env_or_secret(env, "PRESSURE_SHEET_ID", "0")
                    .parse()
                    .unwrap_or(0);

                if let Err(e) = Self::add_pressure(
                    token,
                    &sheet_id,
                    pressure_sheet_id,
                    &pressure_sheet,
                    &tz_str,
                    sys,
                    dia,
                    pulse,
                )
                .await
                {
                    crate::log_event!("error", "sheets.pressure.save_failed", "error={:?}", e);
                    TelegramService::send_message(
                        bot_token,
                        chat_id,
                        &format!("❌ Error saving pressure: {}", e),
                        Some(TelegramService::remove_keyboard()),
                    )
                    .await?;
                } else {
                    let mut msg = format!("✅ Pressure saved: {}/{}", sys, dia);
                    if let Some(p) = pulse {
                        msg.push_str(&format!(" pulse {}", p));
                    }
                    TelegramService::send_message(
                        bot_token,
                        chat_id,
                        &msg,
                        Some(TelegramService::remove_keyboard()),
                    )
                    .await?;
                }
            }
            Action::Cost { amount, comment } => {
                let costs_sheet = get_env_or_secret(env, "COSTS_SHEET", "costs");
                let costs_sheet_id: i64 = get_env_or_secret(env, "COSTS_SHEET_ID", "0")
                    .parse()
                    .unwrap_or(0);

                if let Err(e) = Self::add_cost(
                    token,
                    &sheet_id,
                    &costs_sheet,
                    costs_sheet_id,
                    &tz_str,
                    amount,
                    &comment,
                )
                .await
                {
                    crate::log_event!("error", "sheets.cost.save_failed", "error={:?}", e);
                    TelegramService::send_message(
                        bot_token,
                        chat_id,
                        &format!("❌ Error saving cost: {}", e),
                        Some(TelegramService::remove_keyboard()),
                    )
                    .await?;
                } else {
                    let mut msg = format!("✅ Cost saved: {}", amount);
                    if !comment.is_empty() {
                        msg.push_str(&format!(" {}", comment));
                    }
                    TelegramService::send_message(
                        bot_token,
                        chat_id,
                        &msg,
                        Some(TelegramService::remove_keyboard()),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn add_pressure(
        token: &str,
        sheet_id: &str,
        pressure_sheet_id: i64,
        pressure_sheet: &str,
        tz_str: &str,
        sys: i32,
        dia: i32,
        pulse: Option<i32>,
    ) -> Result<()> {
        let tz: chrono_tz::Tz = tz_str.parse().unwrap_or(chrono_tz::Europe::Kiev);
        let local_time = chrono::Utc::now().with_timezone(&tz);
        let timestamp = local_time.format("%d.%m.%Y %H:%M:%S").to_string();

        // 1. BatchUpdate to insert a row
        let batch_update_url = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}:batchUpdate",
            sheet_id
        );
        let batch_update_payload = serde_json::json!({
            "requests": [
                {
                    "insertDimension": {
                        "range": {
                            "sheetId": pressure_sheet_id,
                            "dimension": "ROWS",
                            "startIndex": 1,
                            "endIndex": 2
                        }
                    }
                }
            ]
        });

        let mut resp = GoogleSheetsService::request(
            token,
            &batch_update_url,
            Method::Post,
            Some(batch_update_payload),
        )
        .await?;
        if resp.status_code() != 200 {
            let err_text = resp.text().await?;
            return Err(worker::Error::from(format!(
                "Prepend insert row failed: {}",
                err_text
            )));
        }

        // 2. Update values at A3 (replicating Go's original logic).
        let range_raw = format!("'{}'!A3", pressure_sheet);
        let range_encoded = urlencoding::encode(&range_raw);

        let update_url = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}?valueInputOption=USER_ENTERED",
            sheet_id, range_encoded
        );
        let pulse_val = pulse.map(|p| p.to_string()).unwrap_or_default();
        let values_payload = serde_json::json!({
            "values": [
                [timestamp, sys.to_string(), dia.to_string(), pulse_val]
            ]
        });

        let mut resp2 =
            GoogleSheetsService::request(token, &update_url, Method::Put, Some(values_payload))
                .await?;
        if resp2.status_code() != 200 {
            let err_text = resp2.text().await?;
            return Err(worker::Error::from(format!(
                "Prepend update values failed: {}",
                err_text
            )));
        }

        Ok(())
    }

    async fn add_cost(
        token: &str,
        sheet_id: &str,
        costs_sheet: &str,
        _costs_sheet_id: i64,
        tz_str: &str,
        amount: i32,
        comment: &str,
    ) -> Result<()> {
        let tz: chrono_tz::Tz = tz_str.parse().unwrap_or(chrono_tz::Europe::Kiev);
        let local_time = chrono::Utc::now().with_timezone(&tz);
        let timestamp = local_time.format("%d.%m").to_string();

        let range_raw = format!("'{}'!A2", costs_sheet);
        let range_encoded = urlencoding::encode(&range_raw);

        let append_url = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}:append?valueInputOption=USER_ENTERED&insertDataOption=INSERT_ROWS",
            sheet_id, range_encoded
        );
        let append_payload = serde_json::json!({
            "values": [
                [timestamp, amount.to_string(), comment]
            ]
        });

        let mut resp =
            GoogleSheetsService::request(token, &append_url, Method::Post, Some(append_payload))
                .await?;
        if resp.status_code() != 200 {
            let err_text = resp.text().await?;
            return Err(worker::Error::from(format!(
                "Append cost failed: {}",
                err_text
            )));
        }

        Ok(())
    }
}
