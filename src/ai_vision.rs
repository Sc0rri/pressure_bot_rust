use worker::*;
use crate::get_env_or_secret;

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
                                "text": "You are an expert OCR assistant specialized in medical displays. Read digits ONLY from this blood pressure monitor screen.\nThe screen has a clear vertical layout with three distinct rows:\n1. TOP ROW (labeled 'SYS mmHg'): Systolic blood pressure (typically 80-250).\n2. MIDDLE ROW (labeled 'DIA mmHg'): Diastolic blood pressure (typically 40-150).\n3. BOTTOM ROW (labeled 'PULSE /min'): Pulse rate (typically 40-200).\n\nLook at the image carefully:\n- First, locate the TOP row digits. Be extremely careful with digit segments: do not confuse '7' with '0' or '1'. If a digit has a top horizontal bar and a slanted right leg, it is a '7' (not a '1' or '0').\n- Second, locate the MIDDLE row digits (Diastolic). Do not confuse it with the bottom row!\n- Third, locate the BOTTOM row digits (Pulse).\n\nIMPORTANT EXCLUSION RULES:\n- On the rightmost side of the LCD screen, there is a vertical level indicator bar that lights up one or more small black rectangular blocks, and ticks like '-135' or '-85'. Do NOT merge these rectangular blocks with the numbers! Ignore the entire vertical indicator bar and its ticks completely.\n- Ignore any small dots or decimal points (like the dot next to SYS or DIA numbers). All readings are whole integers (e.g. 147, not 136).\n\nReturn ONLY the numbers separated by spaces in this exact order: SYSTOLIC DIASTOLIC PULSE.\nExample output format: \"135 85 72\" or \"120 80 65\". Do not add any other words, prefixes, or symbols."
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
                "prompt": "Look at this vertical blood pressure monitor screen. Read the three numbers from top to bottom:\n1. Top row (SYS): Systolic pressure (80-250).\n2. Middle row (DIA): Diastolic pressure (40-150).\n3. Bottom row (PULSE): Pulse rate (40-200).\n\nBe extremely precise reading each digit. Ignore the vertical indicator bar and rectangular blocks on the far right of the screen (along with '-135' and '-85' ticks). Also ignore any decimal dots.\nReturn ONLY the three numbers separated by spaces like '135 85 72'. Do not write any text, prefixes, or slashes.",
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
