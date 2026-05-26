use crate::get_env_or_secret;
use worker::*;

/// Default vision model for better text recognition on images.
/// Can be overridden via env var AI_VISION_MODEL.
/// - "@cf/meta/llama-3.2-11b-vision-instruct" (best for reading numbers)
/// - "@cf/llava-hf/llava-1.5-7b-hf" (lighter, less accurate)
const DEFAULT_VISION_MODEL: &str = "@cf/meta/llama-3.2-11b-vision-instruct";

/// Default number of parallel AI recognition requests.
///
/// Can be overridden via AI_VISION_PARALLEL_REQUESTS.
const DEFAULT_VISION_PARALLEL_REQUESTS: u32 = 4;

pub struct AiVisionService;

impl AiVisionService {
    fn value_keys(value: &serde_json::Value) -> String {
        value
            .as_object()
            .map(|obj| obj.keys().map(String::as_str).collect::<Vec<_>>().join(","))
            .unwrap_or_else(|| value_type_name(value).to_string())
    }

    fn extract_text_response(ai_response: &serde_json::Value) -> Option<String> {
        let result = ai_response.get("result").unwrap_or(ai_response);

        [
            result.get("response"),
            result.get("description"),
            result.get("text"),
            result.get("output"),
            result.get("output_text"),
            result.get("tool_calls"),
            ai_response.get("response"),
            ai_response.get("description"),
            ai_response.get("text"),
            ai_response.get("output"),
            ai_response.get("output_text"),
            ai_response.get("tool_calls"),
        ]
        .into_iter()
        .flatten()
        .find_map(string_from_value)
    }

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

    /// Runs multiple recognition requests in parallel and returns all successful responses.
    /// This is not retry-after-failure logic; all requests are started together.
    pub async fn recognize_pressure_batch(env: &Env, image_bytes: &[u8]) -> Result<Vec<String>> {
        let parallel_requests_str = get_env_or_secret(
            env,
            "AI_VISION_PARALLEL_REQUESTS",
            &DEFAULT_VISION_PARALLEL_REQUESTS.to_string(),
        );
        let parallel_requests: u32 = parallel_requests_str
            .parse()
            .unwrap_or(DEFAULT_VISION_PARALLEL_REQUESTS)
            .max(1);

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

        for i in 0..parallel_requests {
            let ai_url = ai_url.clone();
            let input_json = input_json.clone();
            let auth_header = format!("Bearer {}", api_token.clone());

            handles.push(async move {
                let headers = Headers::new();
                if let Err(e) = headers.set("Content-Type", "application/json") {
                    crate::log_event!(
                        "error",
                        "ai_vision.request.header_failed",
                        "attempt={} header=content-type error={}",
                        i,
                        e
                    );
                    return (i, Err(worker::Error::from(format!("Header error: {}", e))));
                }
                if let Err(e) = headers.set("Authorization", &auth_header) {
                    crate::log_event!(
                        "error",
                        "ai_vision.request.header_failed",
                        "attempt={} header=authorization error={}",
                        i,
                        e
                    );
                    return (i, Err(worker::Error::from(format!("Header error: {}", e))));
                }

                let mut req_init = RequestInit::new();
                req_init.with_method(Method::Post);
                req_init.with_headers(headers);
                req_init.with_body(Some(input_json.clone().into()));

                let req = match Request::new_with_init(&ai_url, &req_init) {
                    Ok(r) => r,
                    Err(e) => {
                        crate::log_event!(
                            "error",
                            "ai_vision.request.build_failed",
                            "attempt={} error={:?}",
                            i,
                            e
                        );
                        return (i, Err(e));
                    }
                };

                let mut resp = match Fetch::Request(req).send().await {
                    Ok(r) => r,
                    Err(e) => {
                        crate::log_event!(
                            "error",
                            "ai_vision.request.fetch_failed",
                            "attempt={} error={:?}",
                            i,
                            e
                        );
                        return (i, Err(e));
                    }
                };

                let status = resp.status_code();
                let body = match resp.text().await {
                    Ok(body) => body,
                    Err(e) => {
                        crate::log_event!(
                            "error",
                            "ai_vision.response.read_failed",
                            "attempt={} status={} error={:?}",
                            i,
                            status,
                            e
                        );
                        return (i, Err(e));
                    }
                };

                if status != 200 {
                    crate::log_event!(
                        "warn",
                        "ai_vision.request.failed",
                        "attempt={} status={} body_chars={}",
                        i,
                        status,
                        body.chars().count()
                    );
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
                        crate::log_event!(
                            "warn",
                            "ai_vision.response.invalid_json",
                            "attempt={} body_chars={} error={}",
                            i,
                            body.chars().count(),
                            e
                        );
                        return (
                            i,
                            Err(worker::Error::from(format!("AI JSON parse error: {}", e))),
                        );
                    }
                };

                let ai_text = Self::extract_text_response(&ai_response).unwrap_or_default();

                if ai_text.is_empty() {
                    let result_keys = ai_response
                        .get("result")
                        .map(Self::value_keys)
                        .unwrap_or_else(|| "missing".to_string());
                    crate::log_event!(
                        "warn",
                        "ai_vision.response.empty_text",
                        "attempt={} body_chars={} top_keys={} result_keys={}",
                        i,
                        body.chars().count(),
                        Self::value_keys(&ai_response),
                        result_keys
                    );
                    crate::log_event!(
                        "warn",
                        "ai_vision.response.raw_preview",
                        "attempt={} body_preview={}",
                        i,
                        log_preview(&body)
                    );
                    return (i, Err(worker::Error::from("AI returned empty response")));
                }

                crate::log_event!(
                    "info",
                    "ai_vision.request.succeeded",
                    "attempt={} response_chars={}",
                    i,
                    ai_text.chars().count()
                );
                crate::log_event!(
                    "info",
                    "ai_vision.response.recognized",
                    "attempt={} response={}",
                    i,
                    log_preview(&ai_text)
                );
                (i, Ok(ai_text))
            });
        }

        let results: Vec<String> = futures::future::join_all(handles)
            .await
            .into_iter()
            .filter_map(|(_i, result)| result.ok())
            .collect();

        crate::log_event!(
            "info",
            "ai_vision.batch.completed",
            "successes={} attempts={}",
            results.len(),
            parallel_requests
        );

        if results.is_empty() {
            return Err(worker::Error::from("All AI recognition attempts failed"));
        }

        Ok(results)
    }
}

fn string_from_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        serde_json::Value::Array(items) => items.iter().find_map(string_from_value),
        serde_json::Value::Object(obj) => {
            if obj.contains_key("sys") && obj.contains_key("dia") {
                serde_json::to_string(value).ok()
            } else {
                obj.values().find_map(string_from_value)
            }
        }
        _ => None,
    }
}

fn value_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn log_preview(value: &str) -> String {
    const MAX_CHARS: usize = 700;

    let normalized = value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");

    let mut preview: String = normalized.chars().take(MAX_CHARS).collect();
    if normalized.chars().count() > MAX_CHARS {
        preview.push_str("...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_response_should_read_result_response() {
        let value = serde_json::json!({
            "result": {
                "response": "{\"sys\":120,\"dia\":80,\"pulse\":70}"
            }
        });

        assert_eq!(
            AiVisionService::extract_text_response(&value),
            Some("{\"sys\":120,\"dia\":80,\"pulse\":70}".to_string())
        );
    }

    #[test]
    fn extract_text_response_should_read_result_output() {
        let value = serde_json::json!({
            "result": {
                "output": "{\"sys\":121,\"dia\":81,\"pulse\":71}"
            }
        });

        assert_eq!(
            AiVisionService::extract_text_response(&value),
            Some("{\"sys\":121,\"dia\":81,\"pulse\":71}".to_string())
        );
    }

    #[test]
    fn extract_text_response_should_read_first_string_from_array_output() {
        let value = serde_json::json!({
            "result": {
                "output": [
                    {"ignored": true},
                    "{\"sys\":122,\"dia\":82,\"pulse\":72}"
                ]
            }
        });

        assert_eq!(
            AiVisionService::extract_text_response(&value),
            Some("{\"sys\":122,\"dia\":82,\"pulse\":72}".to_string())
        );
    }

    #[test]
    fn extract_text_response_should_serialize_object_response() {
        let value = serde_json::json!({
            "result": {
                "response": {
                    "sys": 123,
                    "dia": 83,
                    "pulse": 73
                }
            }
        });

        assert_eq!(
            AiVisionService::extract_text_response(&value),
            Some("{\"dia\":83,\"pulse\":73,\"sys\":123}".to_string())
        );
    }

    #[test]
    fn extract_text_response_should_fall_back_to_tool_calls() {
        let value = serde_json::json!({
            "result": {
                "response": "",
                "tool_calls": [
                    {
                        "arguments": {
                            "sys": 124,
                            "dia": 84,
                            "pulse": 74
                        }
                    }
                ]
            }
        });

        assert_eq!(
            AiVisionService::extract_text_response(&value),
            Some("{\"dia\":84,\"pulse\":74,\"sys\":124}".to_string())
        );
    }

    #[test]
    fn log_preview_should_escape_multiline_text_and_truncate() {
        let long_text = format!("{}\n{}", "a".repeat(700), "tail");

        let preview = log_preview(&long_text);

        assert_eq!(preview, format!("{}...", "a".repeat(700)));
    }
}
