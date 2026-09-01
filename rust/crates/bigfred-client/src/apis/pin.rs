//! `POST /api/v1/auth/login` — verify a participant PIN and drop the JWT.

use serde::Serialize;
use serde_json::Value;

use crate::config::BigFredConfig;
use crate::error::{Error, Result};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginRequest<'a> {
    login: &'a str,
    pin: &'a str,
    layout_id: u64,
}

/// Checks `{ login, pin, layoutId }` against BigFred. On success the minted
/// session body is discarded so a kiosk never keeps a driver JWT.
pub async fn verify_pin(
    http: &reqwest::Client,
    cfg: &BigFredConfig,
    login: &str,
    pin: &str,
    layout_id: u64,
) -> Result<()> {
    let url = format!("{}/api/v1/auth/login", cfg.api_base);
    let res = http
        .post(&url)
        .json(&LoginRequest {
            login,
            pin,
            layout_id,
        })
        .send()
        .await
        .map_err(|err| Error::OauthUnreachable(err.to_string()))?;

    if res.status().is_success() {
        // Drop the minted session — the caller must not keep a driver JWT.
        let _ = res.bytes().await;
        return Ok(());
    }

    let status = res.status().as_u16();
    let text = res.text().await.unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    let code = parsed
        .as_ref()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| {
            if status == 401 {
                "invalid_credentials".to_string()
            } else {
                format!("http_{status}")
            }
        });
    let detail = parsed
        .as_ref()
        .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string));
    Err(Error::BadStatus {
        status,
        code,
        detail,
    })
}
