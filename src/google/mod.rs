use std::time::Duration;
use worker::*;

pub mod auth;

pub struct GoogleSheetsService;

/// Max HTTP attempts per Sheets API call before giving up on transient failures.
const MAX_ATTEMPTS: u8 = 3;
/// Base exponential backoff before a retry, in milliseconds (doubled per attempt).
const RETRY_BASE_DELAY_MS: u64 = 500;
/// Upper bound for the backoff delay, also caps `Retry-After`, in milliseconds.
const RETRY_MAX_DELAY_MS: u64 = 10_000;

impl GoogleSheetsService {
    /// Performs an authorized HTTP request to Google Sheets.
    ///
    /// Transient failures (network errors, HTTP 408/425/429 and 5xx) are retried
    /// up to [`MAX_ATTEMPTS`] times with backoff so short Google API outages don't
    /// surface as user-facing errors. The last response is returned unchanged once
    /// retries are exhausted so callers format the same error text as before.
    pub async fn request(
        token: &str,
        url: &str,
        method: Method,
        body: Option<serde_json::Value>,
    ) -> Result<Response> {
        let auth_header = format!("Bearer {}", token);

        let mut body_json: Option<String> = None;
        if let Some(json_body) = body {
            body_json = Some(serde_json::to_string(&json_body)?);
        }

        let mut attempt = 1u8;
        while attempt <= MAX_ATTEMPTS {
            // Build a fresh Request per attempt: a JS Request body can only be
            // consumed once, so reusing the same Request would fail on retry.
            let headers = Headers::new();
            headers.set("Authorization", &auth_header)?;
            headers.set("Content-Type", "application/json")?;

            let mut req_init = RequestInit::new();
            req_init.with_method(method.clone());
            req_init.with_headers(headers);
            if let Some(ref json_str) = body_json {
                req_init.with_body(Some(json_str.clone().into()));
            }

            let req = Request::new_with_init(url, &req_init)?;
            let result = Fetch::Request(req).send().await;

            match result {
                Err(e) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(e);
                    }
                    crate::log_event!(
                        "warn",
                        "google.sheets.retry_fetch",
                        "attempt={}/{} error={:?}",
                        attempt,
                        MAX_ATTEMPTS,
                        e
                    );
                    Delay::from(Duration::from_millis(retry_delay_ms("", attempt))).await;
                    attempt += 1;
                }
                Ok(resp) => {
                    let status = resp.status_code();
                    if !should_retry(status) {
                        return Ok(resp);
                    }

                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    if attempt >= MAX_ATTEMPTS {
                        // Retries exhausted: return the last response so callers
                        // render the same descriptive error message as before.
                        return Ok(resp);
                    }

                    let mut resp_mut = resp;
                    let err_text = resp_mut.text().await.ok().unwrap_or_default();
                    crate::log_event!(
                        "warn",
                        "google.sheets.retry_http",
                        "attempt={}/{} status={} retry_after={} body_chars={}",
                        attempt,
                        MAX_ATTEMPTS,
                        status,
                        retry_after,
                        err_text.chars().count()
                    );
                    Delay::from(Duration::from_millis(retry_delay_ms(&retry_after, attempt)))
                        .await;
                    attempt += 1;
                }
            }
        }

        unreachable!("request() always returns from inside the retry loop")
    }
}

/// Returns true when `status` indicates a transient failure worth retrying.
fn should_retry(status: u16) -> bool {
    status == 408 || status == 425 || status == 429 || status >= 500
}

/// Returns the sleep duration in milliseconds before a retry.
///
/// Prefers a numeric `Retry-After` header (in seconds) when present, otherwise
/// falls back to exponential backoff: [`RETRY_BASE_DELAY_MS`] doubled per
/// attempt. The result is capped at [`RETRY_MAX_DELAY_MS`].
fn retry_delay_ms(retry_after: &str, attempt: u8) -> u64 {
    match retry_after.parse::<u64>().ok() {
        Some(secs) => cap_delay(secs * 1000),
        None => {
            let shift = (attempt - 1) as u64;
            let exp_ms = RETRY_BASE_DELAY_MS * (1u64 << shift);
            cap_delay(exp_ms)
        }
    }
}

fn cap_delay(ms: u64) -> u64 {
    if ms > RETRY_MAX_DELAY_MS {
        RETRY_MAX_DELAY_MS
    } else {
        ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_retry_accepts_transient_http_errors() {
        for status in [408, 425, 429, 500, 502, 503, 504] {
            assert!(should_retry(status));
        }
    }

    #[test]
    fn should_retry_rejects_success_and_client_errors() {
        for status in [200, 201, 301, 400, 401, 403, 404, 409] {
            assert!(!should_retry(status));
        }
    }

    #[test]
    fn retry_delay_uses_exponential_backoff() {
        assert_eq!(retry_delay_ms("", 1), 500);
        assert_eq!(retry_delay_ms("", 2), 1000);
        assert_eq!(retry_delay_ms("", 3), 2000);
    }

    #[test]
    fn retry_delay_prefers_numeric_retry_after_header() {
        assert_eq!(retry_delay_ms("2", 1), 2000);
    }

    #[test]
    fn retry_delay_falls_back_to_backoff_on_invalid_retry_after() {
        assert_eq!(retry_delay_ms("not-a-number", 2), 1000);
    }

    #[test]
    fn retry_delay_is_capped_at_max_delay() {
        assert_eq!(retry_delay_ms("1000", 1), RETRY_MAX_DELAY_MS);
        assert_eq!(retry_delay_ms("", 10), RETRY_MAX_DELAY_MS);
    }
}
