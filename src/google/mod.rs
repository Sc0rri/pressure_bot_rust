use worker::*;

pub mod auth;

pub struct GoogleSheetsService;

impl GoogleSheetsService {
    /// Performs an authorized HTTP request to Google Sheets.
    pub async fn request(
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
}
