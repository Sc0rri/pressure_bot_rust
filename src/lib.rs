use worker::*;
use serde::{Deserialize, Serialize};
use jwt_simple::prelude::*;
use std::collections::HashMap;

// We need to parse raw Google credentials
#[derive(Deserialize)]
struct GoogleCredentials {
    private_key: String,
    client_email: String,
    token_uri: String,
}

#[derive(Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    expires_in: u64,
}

// Telegram Update models
#[derive(Deserialize, Serialize, Debug)]
struct Update {
    message: Option<Message>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct User {
    id: i64,
    username: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct Chat {
    id: i64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct Message {
    text: Option<String>,
    chat: Chat,
    from: Option<User>,
}

#[derive(Debug, Clone)]
struct DetectResult {
    result_type: String,
    data: HashMap<String, String>,
    raw_text: String,
}

impl DetectResult {
    fn unknown(raw_text: &str) -> Self {
        Self {
            result_type: "unknown".to_string(),
            data: HashMap::new(),
            raw_text: raw_text.to_string(),
        }
    }
}

// Custom URL encoder for Wasm target, supporting special characters and Cyrillic
fn url_encode(s: &str) -> String {
    let mut encoded = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            b' ' => {
                encoded.push_str("%20");
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}

// Resilient helper to read from Cloudflare secrets, standard vars, or fall back to defaults
fn get_env_or_secret(env: &Env, name: &str, default: &str) -> String {
    env.secret(name)
        .map(|v| v.to_string())
        .or_else(|_| env.var(name).map(|v| v.to_string()))
        .unwrap_or_else(|_| default.to_string())
}

fn detect_type(text: &str) -> DetectResult {
    let clean = text.trim();
    let parts: Vec<&str> = clean
        .split(|c: char| c.is_whitespace() || c == '\\' || c == '/' || c == '|')
        .filter(|s| !s.is_empty())
        .collect();

    let mut nums = Vec::new();
    let mut words = Vec::new();
    for p in parts {
        if p.parse::<i32>().is_ok() {
            nums.push(p.to_string());
        } else {
            words.push(p.to_string());
        }
    }

    // PRESSURE
    if words.is_empty() && (nums.len() == 2 || nums.len() == 3) {
        let sys = nums[0].parse::<i32>().unwrap_or(0);
        let dia = nums[1].parse::<i32>().unwrap_or(0);
        if sys >= 80 && sys <= 250 && dia >= 40 && dia <= 150 {
            let mut pulse = String::new();
            if nums.len() == 3 {
                let p = nums[2].parse::<i32>().unwrap_or(0);
                if p >= 40 && p <= 200 {
                    pulse = nums[2].clone();
                } else {
                    return DetectResult::unknown(text);
                }
            }
            let mut data = HashMap::new();
            data.insert("sys".to_string(), nums[0].clone());
            data.insert("dia".to_string(), nums[1].clone());
            data.insert("pulse".to_string(), pulse);
            return DetectResult {
                result_type: "pressure".to_string(),
                data,
                raw_text: text.to_string(),
            };
        }
    }

    // COST
    if nums.len() == 1 {
        let mut data = HashMap::new();
        data.insert("amount".to_string(), nums[0].clone());
        data.insert("comment".to_string(), words.join(" "));
        return DetectResult {
            result_type: "cost".to_string(),
            data,
            raw_text: text.to_string(),
        };
    }

    DetectResult::unknown(text)
}

async fn get_google_token(env: &Env) -> Result<String> {
    let kv = env.kv("STATE_STORE")?;
    
    // 1. Check cached token
    if let Some(cached_token) = kv.get("google_oauth_token").text().await? {
        console_log!("Using cached Google OAuth token");
        return Ok(cached_token);
    }
    
    console_log!("Generating new Google OAuth token...");
    
    let creds_str = env.secret("GOOGLE_CREDENTIALS_JSON")?.to_string();
    let creds: GoogleCredentials = serde_json::from_str(&creds_str)
        .map_err(|e| worker::Error::from(e.to_string()))?;
        
    #[derive(Serialize, Deserialize)]
    struct CustomClaims {
        scope: String,
    }
    
    let claims = Claims::with_custom_claims(
        CustomClaims {
            scope: "https://www.googleapis.com/auth/spreadsheets".to_string(),
        },
        jwt_simple::prelude::Duration::from_secs(3600),
    )
    .with_issuer(&creds.client_email)
    .with_audience(&creds.token_uri);
    
    let key_pair = RS256KeyPair::from_pem(&creds.private_key)
        .map_err(|e| worker::Error::from(e.to_string()))?;
        
    let assertion = key_pair.sign(claims)
        .map_err(|e| worker::Error::from(e.to_string()))?;
        
    let headers = Headers::new();
    headers.set("Content-Type", "application/x-www-form-urlencoded")?;
    
    let payload = format!(
        "grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion={}",
        assertion
    );
    
    let mut req_init = RequestInit::new();
    req_init.with_method(Method::Post);
    req_init.with_headers(headers);
    req_init.with_body(Some(payload.into()));
    
    let req = Request::new_with_init(&creds.token_uri, &req_init)?;
    let mut resp = Fetch::Request(req).send().await?;
    
    if resp.status_code() != 200 {
        let err_text = resp.text().await?;
        return Err(worker::Error::from(format!("Google Auth Error: {}", err_text)));
    }
    
    let token_res: GoogleTokenResponse = resp.json().await?;
    
    // Cache token with a 5-minute safety margin
    let cache_ttl = if token_res.expires_in > 300 {
        token_res.expires_in - 300
    } else {
        token_res.expires_in
    };
    
    kv.put("google_oauth_token", &token_res.access_token)?
        .expiration_ttl(cache_ttl)
        .execute()
        .await?;
        
    Ok(token_res.access_token)
}

async fn google_sheets_request(
    token: &str,
    url: &str,
    method: Method,
    body: Option<serde_json::Value>,
) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {}", token))?;
    headers.set("Content-Type", "application/json")?;

    let mut req_init = RequestInit::new();
    req_init.with_method(method);
    req_init.with_headers(headers);

    if let Some(json_body) = body {
        req_init.with_body(Some(serde_json::to_string(&json_body)?.into()));
    }

    let req = Request::new_with_init(url, &req_init)?;
    Fetch::Request(req).send().await
}

async fn add_pressure(
    _env: &Env,
    token: &str,
    sheet_id: &str,
    pressure_sheet_id: i64,
    pressure_sheet: &str,
    tz_str: &str,
    sys: &str,
    dia: &str,
    pulse: &str,
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
    
    let mut resp = google_sheets_request(token, &batch_update_url, Method::Post, Some(batch_update_payload)).await?;
    if resp.status_code() != 200 {
        let err_text = resp.text().await?;
        return Err(worker::Error::from(format!("Prepend insert row failed: {}", err_text)));
    }

    // 2. Update values at A3 (replicating Go's original logic). 
    // We wrap sheet name in single quotes and URL encode it to avoid spaces and parentheses issues.
    let range_raw = format!("'{}'!A3", pressure_sheet);
    let range_encoded = url_encode(&range_raw);
    
    let update_url = format!(
        "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}?valueInputOption=USER_ENTERED",
        sheet_id,
        range_encoded
    );
    let values_payload = serde_json::json!({
        "values": [
            [timestamp, sys, dia, pulse]
        ]
    });
    
    let mut resp2 = google_sheets_request(token, &update_url, Method::Put, Some(values_payload)).await?;
    if resp2.status_code() != 200 {
        let err_text = resp2.text().await?;
        return Err(worker::Error::from(format!("Prepend update values failed: {}", err_text)));
    }

    Ok(())
}

async fn add_cost(
    _env: &Env,
    token: &str,
    sheet_id: &str,
    costs_sheet: &str,
    _costs_sheet_id: i64, // kept for exact GID config matching
    tz_str: &str,
    amount: &str,
    comment: &str,
) -> Result<()> {
    let tz: chrono_tz::Tz = tz_str.parse().unwrap_or(chrono_tz::Europe::Kiev);
    let local_time = chrono::Utc::now().with_timezone(&tz);
    let timestamp = local_time.format("%d.%m").to_string();

    // We wrap sheet name in single quotes and URL encode it to avoid spaces and parentheses issues.
    let range_raw = format!("'{}'!A2", costs_sheet);
    let range_encoded = url_encode(&range_raw);

    let append_url = format!(
        "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}:append?valueInputOption=USER_ENTERED&insertDataOption=INSERT_ROWS",
        sheet_id,
        range_encoded
    );
    let append_payload = serde_json::json!({
        "values": [
            [timestamp, amount, comment]
        ]
    });
    
    let mut resp = google_sheets_request(token, &append_url, Method::Post, Some(append_payload)).await?;
    if resp.status_code() != 200 {
        let err_text = resp.text().await?;
        return Err(worker::Error::from(format!("Append cost failed: {}", err_text)));
    }

    Ok(())
}

async fn send_telegram_message(
    token: &str,
    chat_id: i64,
    text: &str,
    keyboard: Option<serde_json::Value>,
) -> Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
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

fn choose_keyboard() -> serde_json::Value {
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

fn remove_keyboard() -> serde_json::Value {
    serde_json::json!({
        "remove_keyboard": true
    })
}

#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    _ctx: Context,
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
        let bot_token = env.secret("BOT_TOKEN")?.to_string();
        let kv = env.kv("STATE_STORE")?;
        let chat_key = chat_id.to_string();

        let text = msg.text.clone().unwrap_or_default().trim().to_string();
        if text.is_empty() {
            let res = Response::empty()?;
            return Ok(res.try_into()?);
        }

        console_log!("INPUT TEXT: {}", text);

        // Fetch Google Token safely with visual error reporting
        let token = match get_google_token(&env).await {
            Ok(t) => t,
            Err(e) => {
                console_log!("get_google_token failed: {:?}", e);
                send_telegram_message(&bot_token, chat_id, &format!("❌ Google Auth Error: {}", e), Some(remove_keyboard())).await?;
                let res = Response::empty()?;
                return Ok(res.try_into()?);
            }
        };

        // 3. Process Pending Action
        if let Some(pending_text) = kv.get(&chat_key).text().await? {
            match text.as_str() {
                "🩺 Pressure" => {
                    kv.delete(&chat_key).await?;
                    let nums: Vec<&str> = pending_text.split_whitespace().collect();
                    if nums.len() < 2 {
                        send_telegram_message(&bot_token, chat_id, "❌ Need at least 2 numbers: sys dia", Some(remove_keyboard())).await?;
                        let res = Response::empty()?;
                        return Ok(res.try_into()?);
                    }
                    let pulse = if nums.len() >= 3 { nums[2] } else { "" };
                    
                    let sheet_id = env.secret("SHEET_ID")?.to_string();
                    let pressure_sheet = get_env_or_secret(&env, "PRESSURE_SHEET", "pressure");
                    let pressure_sheet_id: i64 = get_env_or_secret(&env, "PRESSURE_SHEET_ID", "0")
                        .parse()
                        .unwrap_or(0);
                    let tz_str = get_env_or_secret(&env, "TIMEZONE", "Europe/Kiev");
                    
                    if let Err(e) = add_pressure(&env, &token, &sheet_id, pressure_sheet_id, &pressure_sheet, &tz_str, nums[0], nums[1], pulse).await {
                        console_log!("add_pressure failed: {:?}", e);
                        send_telegram_message(&bot_token, chat_id, &format!("❌ Error saving pressure: {}", e), Some(remove_keyboard())).await?;
                    } else {
                        let mut msg = format!("✅ Pressure saved: {}/{}", nums[0], nums[1]);
                        if !pulse.is_empty() {
                            msg.push_str(&format!(" pulse {}", pulse));
                        }
                        send_telegram_message(&bot_token, chat_id, &msg, Some(remove_keyboard())).await?;
                    }
                    
                    let res = Response::empty()?;
                    return Ok(res.try_into()?);
                }
                "💸 Cost" => {
                    kv.delete(&chat_key).await?;
                    let parts: Vec<&str> = pending_text.split_whitespace().collect();
                    let mut amount = String::new();
                    let mut comment_parts = Vec::new();
                    
                    for p in parts {
                        if p.parse::<i32>().is_ok() && amount.is_empty() {
                            amount = p.to_string();
                        } else {
                            comment_parts.push(p);
                        }
                    }
                    let comment = comment_parts.join(" ");
                    
                    let sheet_id = env.secret("SHEET_ID")?.to_string();
                    let costs_sheet = get_env_or_secret(&env, "COSTS_SHEET", "costs");
                    let costs_sheet_id: i64 = get_env_or_secret(&env, "COSTS_SHEET_ID", "0")
                        .parse()
                        .unwrap_or(0);
                    let tz_str = get_env_or_secret(&env, "TIMEZONE", "Europe/Kiev");
                    
                    if let Err(e) = add_cost(&env, &token, &sheet_id, &costs_sheet, costs_sheet_id, &tz_str, &amount, &comment).await {
                        console_log!("add_cost failed: {:?}", e);
                        send_telegram_message(&bot_token, chat_id, &format!("❌ Error saving cost: {}", e), Some(remove_keyboard())).await?;
                    } else {
                        let mut msg = format!("✅ Cost saved: {}", amount);
                        if !comment.is_empty() {
                            msg.push_str(&format!(" {}", comment));
                        }
                        send_telegram_message(&bot_token, chat_id, &msg, Some(remove_keyboard())).await?;
                    }
                    
                    let res = Response::empty()?;
                    return Ok(res.try_into()?);
                }
                "❌ Cancel" => {
                    kv.delete(&chat_key).await?;
                    send_telegram_message(&bot_token, chat_id, "❌ Canceled", Some(remove_keyboard())).await?;
                    let res = Response::empty()?;
                    return Ok(res.try_into()?);
                }
                _ => {}
            }
        }

        // 4. Default classification flow
        let res = detect_type(&text);
        console_log!("DETECTED: type={} data={:?}", res.result_type, res.data);

        match res.result_type.as_str() {
            "pressure" => {
                let sys = res.data.get("sys").unwrap();
                let dia = res.data.get("dia").unwrap();
                let pulse = res.data.get("pulse").unwrap();
                
                let sheet_id = env.secret("SHEET_ID")?.to_string();
                let pressure_sheet = get_env_or_secret(&env, "PRESSURE_SHEET", "pressure");
                let pressure_sheet_id: i64 = get_env_or_secret(&env, "PRESSURE_SHEET_ID", "0")
                    .parse()
                    .unwrap_or(0);
                let tz_str = get_env_or_secret(&env, "TIMEZONE", "Europe/Kiev");
                
                if let Err(e) = add_pressure(&env, &token, &sheet_id, pressure_sheet_id, &pressure_sheet, &tz_str, sys, dia, pulse).await {
                    console_log!("add_pressure failed: {:?}", e);
                    send_telegram_message(&bot_token, chat_id, &format!("❌ Error saving pressure: {}", e), Some(remove_keyboard())).await?;
                } else {
                    let mut msg = format!("✅ Pressure saved: {}/{}", sys, dia);
                    if !pulse.is_empty() {
                        msg.push_str(&format!(" pulse {}", pulse));
                    }
                    send_telegram_message(&bot_token, chat_id, &msg, Some(remove_keyboard())).await?;
                }
            }
            "cost" => {
                let amount = res.data.get("amount").unwrap();
                let comment = res.data.get("comment").unwrap();
                
                let sheet_id = env.secret("SHEET_ID")?.to_string();
                let costs_sheet = get_env_or_secret(&env, "COSTS_SHEET", "costs");
                let costs_sheet_id: i64 = get_env_or_secret(&env, "COSTS_SHEET_ID", "0")
                    .parse()
                    .unwrap_or(0);
                let tz_str = get_env_or_secret(&env, "TIMEZONE", "Europe/Kiev");
                
                if let Err(e) = add_cost(&env, &token, &sheet_id, &costs_sheet, costs_sheet_id, &tz_str, amount, comment).await {
                    console_log!("add_cost failed: {:?}", e);
                    send_telegram_message(&bot_token, chat_id, &format!("❌ Error saving cost: {}", e), Some(remove_keyboard())).await?;
                } else {
                    let mut msg = format!("✅ Cost saved: {}", amount);
                    if !comment.is_empty() {
                        msg.push_str(&format!(" {}", comment));
                    }
                    send_telegram_message(&bot_token, chat_id, &msg, Some(remove_keyboard())).await?;
                }
            }
            _ => {
                // Unknown action: save to KV and offer menu
                kv.put(&chat_key, &text)?.expiration_ttl(600).execute().await?;
                send_telegram_message(&bot_token, chat_id, "Where to save?", Some(choose_keyboard())).await?;
            }
        }
    }

    let res = Response::empty()?;
    Ok(res.try_into()?)
}