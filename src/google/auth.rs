use jwt_simple::prelude::*;
use serde::{Deserialize, Serialize};
use worker::*;

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

pub struct GoogleAuthService;

impl GoogleAuthService {
    /// Generates a new OAuth 2.0 token or retrieves a cached one from the KV store.
    pub async fn get_token(env: &Env) -> Result<String> {
        let kv = env.kv("STATE_STORE")?;

        if let Some(cached_token) = kv.get("google_oauth_token").text().await? {
            crate::log_event!("info", "google.auth.cache_hit");
            return Ok(cached_token);
        }

        crate::log_event!("info", "google.auth.token_requested");

        let creds_str = env.secret("GOOGLE_CREDENTIALS_JSON")?.to_string();
        let creds: GoogleCredentials =
            serde_json::from_str(&creds_str).map_err(|e| worker::Error::from(e.to_string()))?;

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
        let assertion = key_pair
            .sign(claims)
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
            return Err(worker::Error::from(format!(
                "Google Auth Error: {}",
                err_text
            )));
        }

        let token_res: GoogleTokenResponse = resp.json().await?;
        let cache_ttl = token_res.expires_in.saturating_sub(300).max(60);

        kv.put("google_oauth_token", &token_res.access_token)?
            .expiration_ttl(cache_ttl)
            .execute()
            .await?;

        crate::log_event!(
            "info",
            "google.auth.cache_store",
            "ttl_seconds={}",
            cache_ttl
        );

        Ok(token_res.access_token)
    }
}
