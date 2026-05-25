use crate::get_env_or_secret;
use worker::*;

/// Default vision model for better text recognition on images.
/// Can be overridden via env var AI_VISION_MODEL.
/// - "@cf/meta/llama-3.2-11b-vision-instruct" (best for reading numbers)
/// - "@cf/llava-hf/llava-1.5-7b-hf" (lighter, less accurate)
const DEFAULT_VISION_MODEL: &str = "@cf/meta/llama-3.2-11b-vision-instruct";

/// Default number of parallel AI recognition attempts (can be overridden via AI_VISION_RETRIES)
const DEFAULT_VISION_RETRIES: u32 = 4;

pub struct AiVisionService;

impl AiVisionService {
    /// Builds the JSON request payload for a vision model call.
    fn build_request(model: &str, image_bytes: &[u8], mime: &str) -> serde_json::Value {
        // Encode image to base64
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        let image_b64 = engine.encode(image_bytes);

        let model_is_llama = model.contains("llama");

        if model_is_llama {
            serde_json::json!({
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "text",
                                "text": "You are an expert OCR assistant specialized in medical displays. Read digits ONLY from this blood pressure monitor screen.\n\nCRITICAL OUTPUT RULES:\n- The first character of your response must be `{`.\n- Return exactly one JSON object with keys: sys (integer), dia (integer), pulse (integer).\n- Do not include explanations, labels, markdown, code fences, or examples in the response.\n\nThe screen has a clear vertical layout with three distinct rows:\n1. TOP ROW (labeled 'SYS mmHg'): Systolic blood pressure (typically 80-250).\n2. MIDDLE ROW (labeled 'DIA mmHg'): Diastolic blood pressure (typically 40-150).\n3. BOTTOM ROW (labeled 'PULSE /min'): Pulse rate (typically 40-200).\n\nLook at the image carefully:\n- First, locate the TOP row digits. Be extremely careful with digit segments: do not confuse '7' with '0' or '1'. If a digit has a top horizontal bar and a slanted right leg, it is a '7' (not a '1' or '0').\n- Second, locate the MIDDLE row digits (Diastolic). Do not confuse it with the bottom row!\n- Third, locate the BOTTOM row digits (Pulse).\n\nIMPORTANT EXCLUSION RULES:\n- On the rightmost side of the LCD screen, there is a vertical level indicator bar that lights up one or more small black rectangular blocks, and ticks like '-135' or '-85'. Do NOT merge these rectangular blocks with the numbers! Ignore the entire vertical indicator bar and its ticks completely.\n- Ignore any small dots or decimal points (like the dot next to SYS or DIA numbers). All readings are whole integers."
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
                "max_tokens": 512
            })
        } else {
            let image_array: Vec<u8> = image_bytes.to_vec();
            serde_json::json!({
                "prompt": "Look at this vertical blood pressure monitor screen. Read the three numbers from top to bottom:\n1. Top row (SYS): Systolic pressure (80-250).\n2. Middle row (DIA): Diastolic pressure (40-150).\n3. Bottom row (PULSE): Pulse rate (40-200).\n\nBe extremely precise reading each digit. Ignore the vertical indicator bar and rectangular blocks on the far right of the screen (along with '-135' and '-85' ticks). Also ignore any decimal dots.\n\nCRITICAL OUTPUT RULES:\n- The first character of your response must be `{`.\n- Return exactly one JSON object with keys: sys (integer), dia (integer), pulse (integer).\n- Do not include explanations, labels, markdown, code fences, or examples in the response.",
                "image": image_array
            })
        }
    }

    /// Runs multiple parallel recognition attempts and returns all successful responses.
    /// The number of attempts is configured via AI_VISION_RETRIES env (default 4).
    pub async fn recognize_pressure_batch(env: &Env, image_bytes: &[u8]) -> Result<Vec<String>> {
        let retries_str = get_env_or_secret(
            env,
            "AI_VISION_RETRIES",
            &DEFAULT_VISION_RETRIES.to_string(),
        );
        let retries: u32 = retries_str.parse().unwrap_or(DEFAULT_VISION_RETRIES);

        // Build the environment snapshot once (Env is not Send+Sync in worker-rs)
        let account_id = get_env_or_secret(env, "CLOUDFLARE_ACCOUNT_ID", "");
        let api_token = get_env_or_secret(env, "CLOUDFLARE_API_TOKEN", "");
        let model = get_env_or_secret(env, "AI_VISION_MODEL", DEFAULT_VISION_MODEL);

        if account_id.is_empty() || api_token.is_empty() {
            return Err(worker::Error::from(
                "CLOUDFLARE_ACCOUNT_ID or CLOUDFLARE_API_TOKEN not configured",
            ));
        }

        let ai_url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}",
            account_id, model
        );

        // Detect mime once
        let mime = if image_bytes.len() > 4
            && image_bytes[0] == 0x89
            && image_bytes[1] == 0x50
            && image_bytes[2] == 0x4E
            && image_bytes[3] == 0x47
        {
            "image/png"
        } else if image_bytes.len() > 2 && image_bytes[0] == 0xFF && image_bytes[1] == 0xD8 {
            "image/jpeg"
        } else if image_bytes.len() > 3
            && image_bytes[0] == 0x47
            && image_bytes[1] == 0x49
            && image_bytes[2] == 0x46
        {
            "image/gif"
        } else if image_bytes.len() > 4
            && image_bytes[0] == 0x52
            && image_bytes[1] == 0x49
            && image_bytes[2] == 0x46
            && image_bytes[3] == 0x46
        {
            "image/webp"
        } else {
            "image/jpeg"
        };

        let input = Self::build_request(&model, image_bytes, mime);
        let input_json = serde_json::to_string(&input)?;

        // Use futures::future::join_all to run requests in parallel
        let mut handles = Vec::new();

        for i in 0..retries {
            let ai_url = ai_url.clone();
            let input_json = input_json.clone();
            let auth_header = format!("Bearer {}", api_token.clone());

            handles.push(async move {
                let headers = Headers::new();
                if let Err(e) = headers.set("Content-Type", "application/json") {
                    return (i, Err(worker::Error::from(format!("Header error: {}", e))));
                }
                if let Err(e) = headers.set("Authorization", &auth_header) {
                    return (i, Err(worker::Error::from(format!("Header error: {}", e))));
                }

                let mut req_init = RequestInit::new();
                req_init.with_method(Method::Post);
                req_init.with_headers(headers);
                req_init.with_body(Some(input_json.clone().into()));

                let req = match Request::new_with_init(&ai_url, &req_init) {
                    Ok(r) => r,
                    Err(e) => return (i, Err(e)),
                };

                let mut resp = match Fetch::Request(req).send().await {
                    Ok(r) => r,
                    Err(e) => return (i, Err(e)),
                };

                let status = resp.status_code();
                let body = resp.text().await.unwrap_or_default();

                if status != 200 {
                    console_log!("AI batch request {} error ({}): {}", i, status, body);
                    return (
                        i,
                        Err(worker::Error::from(format!(
                            "AI API error ({}): {}",
                            status, body
                        ))),
                    );
                }

                let ai_response: serde_json::Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(e) => {
                        return (
                            i,
                            Err(worker::Error::from(format!("AI JSON parse error: {}", e))),
                        );
                    }
                };

                let ai_text = ai_response
                    .get("result")
                    .and_then(|r| r.get("response").or_else(|| r.get("description")))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if ai_text.is_empty() {
                    return (i, Err(worker::Error::from("AI returned empty response")));
                }

                console_log!("AI batch request {} response: {}", i, ai_text);
                (i, Ok(ai_text))
            });
        }

        let results: Vec<String> = futures::future::join_all(handles)
            .await
            .into_iter()
            .filter_map(|(_i, result)| result.ok())
            .collect();

        console_log!(
            "AI batch complete: {} successful responses out of {} attempts",
            results.len(),
            retries
        );

        if results.is_empty() {
            return Err(worker::Error::from("All AI recognition attempts failed"));
        }

        Ok(results)
    }
}
