use worker::*;
use serde::{Deserialize, Serialize};
use jwt_simple::prelude::*;

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

pub struct GoogleSheetsService;

impl GoogleSheetsService {
    /// Generates a new OAuth 2.0 token or retrieves a cached one from the KV store
    pub async fn get_token(env: &Env) -> Result<String> {
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

    /// Performs an authorized HTTP request to Google Sheets
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
