use worker::*;

/// Default vision model for better text recognition on images.
/// Can be overridden via env var AI_VISION_MODEL.
/// - "@cf/meta/llama-3.2-11b-vision-instruct" (best for reading numbers)
/// - "@cf/llava-hf/llava-1.5-7b-hf" (lighter, less accurate)
const DEFAULT_VISION_MODEL: &str = "@cf/meta/llama-3.2-11b-vision-instruct";

pub struct AiVisionService;

impl AiVisionService {
    /// Sends image bytes to Workers AI and returns the recognized text description
    pub async fn recognize_pressure(env: &Env, image_bytes: &[u8]) -> Result<String> {
        let account_id = get_env_or_secret(env, "CLOUDFLARE_ACCOUNT_ID", "");
        let api_token = get_env_or_secret(env, "CLOUDFLARE_API_TOKEN", "");
        let model = get_env_or_secret(env, "AI_VISION_MODEL", DEFAULT_VISION_MODEL);

        if account_id.is_empty() || api_token.is_empty() {
            return Err(worker::Error::from("CLOUDFLARE_ACCOUNT_ID or CLOUDFLARE_API_TOKEN not configured"));
        }

        let ai_url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}",
            account_id,
            model
        );

        // Detect image mime type from magic bytes
        let mime = if image_bytes.len() > 4 && image_bytes[0] == 0x89 && image_bytes[1] == 0x50 && image_bytes[2] == 0x4E && image_bytes[3] == 0x47 {
            "image/png"
        } else if image_bytes.len() > 2 && image_bytes[0] == 0xFF && image_bytes[1] == 0xD8 {
            "image/jpeg"
        } else if image_bytes.len() > 3 && image_bytes[0] == 0x47 && image_bytes[1] == 0x49 && image_bytes[2] == 0x46 {
            "image/gif"
        } else if image_bytes.len() > 4 && image_bytes[0] == 0x52 && image_bytes[1] == 0x49 && image_bytes[2] == 0x46 && image_bytes[3] == 0x46 {
            "image/webp"
        } else {
            "image/jpeg"
        };

        // Encode image to base64
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        let image_b64 = engine.encode(image_bytes);

        let model_is_llama = model.contains("llama");

        let input = if model_is_llama {
            serde_json::json!({
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "text",
                                "text": "You are an OCR assistant. Read digits ONLY from this blood pressure monitor display. Look carefully at each digit individually. The display shows systolic (top/left, 80-250) and diastolic (bottom/right, 40-150) numbers. A pulse number (40-200, labeled PULSE/HR) may be nearby. Return ONLY the numbers with spaces: SYSTOLIC DIASTOLIC PULSE. Make sure you read each digit correctly - do not confuse 5 and 8, or 0 and 8, or 1 and 7. Examples: '130 85 72' or '120 80' or '158 95 65'. Read each digit character by character. If you see 158, return 158 not 150."
                            },
                            {
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", mime, image_b64)
                                }
                            }
                        ]
                    }
                ],
                "max_tokens": 256
            })
        } else {
            let image_array: Vec<u8> = image_bytes.to_vec();
            serde_json::json!({
                "prompt": "Look at this blood pressure monitor display. Find systolic (80-250), diastolic (40-150), and pulse (40-200) if visible. Return ONLY numbers separated by spaces like '130 85 72' or '120 80'. No slash, no text, just numbers.",
                "image": image_array
            })
        };

        console_log!("Calling Workers AI...");

        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;
        headers.set("Authorization", &format!("Bearer {}", api_token))?;

        let mut req_init = RequestInit::new();
        req_init.with_method(Method::Post);
        req_init.with_headers(headers);
        req_init.with_body(Some(serde_json::to_string(&input)?.into()));

        let req = Request::new_with_init(&ai_url, &req_init)?;
        let mut resp = Fetch::Request(req).send().await?;

        let status = resp.status_code();
        let body = resp.text().await?;

        if status != 200 {
            console_log!("AI API error ({}): {}", status, body);
            return Err(worker::Error::from(format!("AI API error ({}): {}", status, body)));
        }

        console_log!("AI response body: {}", body);

        let ai_response: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| worker::Error::from(format!("AI JSON parse error: {}", e)))?;

        // Extract text from AI response
        // llava returns result.description, llama vision returns result.response
        let ai_text = ai_response.get("result")
            .and_then(|r| {
                r.get("response")
                    .or_else(|| r.get("description"))
            })
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if ai_text.is_empty() {
            console_log!("AI returned empty response. Full: {}", ai_response);
            return Err(worker::Error::from("AI returned empty response"));
        }

        console_log!("AI description: {}", ai_text);
        Ok(ai_text)
    }
}

fn get_env_or_secret(env: &Env, name: &str, default: &str) -> String {
    env.secret(name)
        .map(|v| v.to_string())
        .or_else(|_| env.var(name).map(|v| v.to_string()))
        .unwrap_or_else(|_| default.to_string())
}