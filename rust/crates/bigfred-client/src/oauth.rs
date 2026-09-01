//! OAuth confidential-client drop-in and authorization-code exchange.

use std::io::Write;
use std::path::{Path, PathBuf};

use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::config::BigFredConfig;
use crate::error::{Error, Result};
use crate::wire::TokenResponse;

/// One drop-in registration, mirroring `cmd.OAuthClient` on the Go side.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientFile {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub cors_enabled: bool,
    #[serde(default)]
    pub cors_origins: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub share_session: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpstreamRequest<'a> {
    grant_type: &'a str,
    code: &'a str,
    client_id: &'a str,
    client_secret: &'a str,
    redirect_uri: &'a str,
}

/// Creates the drop-in when missing. An existing file keeps its secret, but
/// redirect URIs from `cfg` are always merged in. Directory and file modes
/// are re-asserted so BigFred (`bigfred`) can read.
pub fn ensure_dropin(cfg: &BigFredConfig) -> Result<PathBuf> {
    let dir = &cfg.oauth_dropin_dir;
    std::fs::create_dir_all(dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;
    harden_dropin_dir(dir);

    let path = cfg.oauth_client_path();
    if path.exists() {
        let changed = sync_redirect_uris(&path, cfg)?;
        harden_dropin_file(&path);
        if changed {
            touch_for_reload(&path);
        }
        tracing::info!(path = %path.display(), "oauth client drop-in present");
        return Ok(path);
    }

    let file = OAuthClientFile {
        client_id: cfg.sso_client_id.clone(),
        client_secret: random_secret(),
        display_name: cfg.oauth_display_name.clone(),
        redirect_uris: cfg.redirect_uris.clone(),
        cors_enabled: false,
        cors_origins: Vec::new(),
        enabled: true,
        share_session: false,
    };
    write_private(
        &path,
        &serde_json::to_vec_pretty(&file).map_err(|source| Error::Serialize {
            path: path.clone(),
            source,
        })?,
    )?;
    harden_dropin_file(&path);
    tracing::info!(path = %path.display(), "seeded oauth client drop-in");
    Ok(path)
}

/// Reads the client secret back for the token exchange.
pub fn load_secret(cfg: &BigFredConfig) -> Result<Option<String>> {
    let path = cfg.oauth_client_path();
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::Io { path, source }),
    };
    let file: OAuthClientFile =
        serde_json::from_slice(&raw).map_err(|source| Error::Parse { path, source })?;
    Ok(Some(file.client_secret))
}

/// Adds the confidential `clientSecret` and exchanges `code` with BigFred.
pub async fn exchange_token(
    http: &reqwest::Client,
    cfg: &BigFredConfig,
    code: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    let secret = match load_secret(cfg) {
        Ok(Some(secret)) => secret,
        Ok(None) => {
            tracing::info!("oauth drop-in missing on token exchange — seeding");
            ensure_dropin(cfg).map_err(|err| Error::OauthClientEnsureFailed(err.to_string()))?;
            load_secret(cfg)
                .map_err(|err| Error::OauthClientUnreadable(err.to_string()))?
                .ok_or(Error::OauthClientMissing)?
        }
        Err(err) => {
            tracing::warn!(error = %err, "oauth drop-in unreadable — re-seeding");
            ensure_dropin(cfg).map_err(|err| Error::OauthClientEnsureFailed(err.to_string()))?;
            load_secret(cfg)
                .map_err(|err| Error::OauthClientUnreadable(err.to_string()))?
                .ok_or(Error::OauthClientMissing)?
        }
    };

    let url = format!("{}/api/v1/auth/oauth/token", cfg.api_base);
    let res = http
        .post(&url)
        .json(&UpstreamRequest {
            grant_type: "authorization_code",
            code,
            client_id: &cfg.sso_client_id,
            client_secret: &secret,
            redirect_uri,
        })
        .send()
        .await
        .map_err(|err| Error::OauthUnreachable(err.to_string()))?;

    let status = res.status();
    let payload = res.bytes().await.unwrap_or_default();
    if !status.is_success() {
        let code = serde_json::from_slice::<serde_json::Value>(&payload)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| "oauth_exchange_failed".to_string());
        return Err(Error::BadStatus {
            status: status.as_u16(),
            code,
            detail: None,
        });
    }

    serde_json::from_slice(&payload).map_err(|err| Error::OauthBadResponse(err.to_string()))
}

fn sync_redirect_uris(path: &Path, cfg: &BigFredConfig) -> Result<bool> {
    let raw = std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut file: OAuthClientFile =
        serde_json::from_slice(&raw).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })?;

    let before = file.redirect_uris.clone();
    for uri in &cfg.redirect_uris {
        let trimmed = uri.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !file.redirect_uris.iter().any(|u| u.trim() == trimmed) {
            file.redirect_uris.push(trimmed.to_string());
        }
    }
    if file.redirect_uris == before {
        return Ok(false);
    }

    let mut data = serde_json::to_vec_pretty(&file).map_err(|source| Error::Serialize {
        path: path.to_path_buf(),
        source,
    })?;
    data.push(b'\n');
    std::fs::write(path, data).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    tracing::info!(path = %path.display(), "merged redirect URIs into oauth client drop-in");
    Ok(true)
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let io = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o640);
    }
    let mut f = opts.open(path).map_err(io)?;
    f.write_all(data).map_err(io)?;
    f.write_all(b"\n").map_err(io)?;
    f.sync_all().map_err(io)?;
    Ok(())
}

fn harden_dropin_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{chown, PermissionsExt};
        if let Err(err) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750)) {
            tracing::warn!(path = %dir.display(), error = %err, "could not set drop-in dir mode 0750");
        }
        if let Some(gid) = bigfred_gid() {
            if let Err(err) = chown(dir, None, Some(gid)) {
                tracing::warn!(path = %dir.display(), error = %err, "could not chown drop-in dir to bigfred — BigFred may not read it");
            }
        } else {
            tracing::warn!(path = %dir.display(), "bigfred group not found — drop-in dir stays root-owned; BigFred may return invalid_client");
        }
        if let Some(parent) = dir.parent() {
            if let Err(err) =
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o750))
            {
                tracing::warn!(path = %parent.display(), error = %err, "could not set drop-in parent dir mode 0750");
            }
            if let Some(gid) = bigfred_gid() {
                if let Err(err) = chown(parent, None, Some(gid)) {
                    tracing::warn!(path = %parent.display(), error = %err, "could not chown drop-in parent dir to bigfred");
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

fn harden_dropin_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{chown, PermissionsExt};
        if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640)) {
            tracing::warn!(path = %path.display(), error = %err, "could not set drop-in mode 0640");
        }
        if let Some(gid) = bigfred_gid() {
            if let Err(err) = chown(path, None, Some(gid)) {
                tracing::warn!(path = %path.display(), error = %err, "could not chown drop-in to bigfred — BigFred may return invalid_client");
            }
        } else {
            tracing::warn!(path = %path.display(), "bigfred group not found — drop-in stays root-owned; BigFred may return invalid_client");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn bigfred_gid() -> Option<u32> {
    use std::sync::OnceLock;
    static GID: OnceLock<Option<u32>> = OnceLock::new();
    *GID.get_or_init(|| {
        let Ok(text) = std::fs::read_to_string("/etc/group") else {
            tracing::warn!("could not read /etc/group — cannot resolve bigfred gid");
            return None;
        };
        for line in text.lines() {
            let mut parts = line.split(':');
            if parts.next() != Some("bigfred") {
                continue;
            }
            let _passwd = parts.next();
            let Some(gid_str) = parts.next() else {
                tracing::warn!(
                    line,
                    "malformed bigfred entry in /etc/group — missing gid field"
                );
                continue;
            };
            match gid_str.parse::<u32>() {
                Ok(gid) => return Some(gid),
                Err(err) => {
                    tracing::warn!(line, error = %err, "malformed bigfred gid in /etc/group");
                    continue;
                }
            }
        }
        tracing::warn!("bigfred group not found in /etc/group");
        None
    })
}

fn touch_for_reload(path: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;

    #[test]
    fn secret_is_64_hex_chars() {
        let s = random_secret();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(s, random_secret());
    }

    #[test]
    fn ensure_writes_once_and_reads_back() {
        let tmp = std::env::temp_dir().join(format!("bf-oauth-{}", uuid::Uuid::new_v4()));
        let cfg = test_config(tmp.join("oauth-clients"));

        let path = ensure_dropin(&cfg).expect("seed");
        let first = load_secret(&cfg).expect("read").expect("some");
        ensure_dropin(&cfg).expect("idempotent");
        let second = load_secret(&cfg).expect("read").expect("some");

        assert_eq!(first, second);
        assert!(path.ends_with("bigfred-wizard.json"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640);
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sync_redirect_uris_preserves_share_session() {
        let tmp = std::env::temp_dir().join(format!("bf-oauth-{}", uuid::Uuid::new_v4()));
        let cfg = test_config(tmp.join("oauth-clients"));
        let path = cfg.oauth_client_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = OAuthClientFile {
            client_id: cfg.sso_client_id.clone(),
            client_secret: "aabbccdd".repeat(8),
            display_name: "BigFred Wizard".to_string(),
            redirect_uris: vec!["http://example.test/cb".to_string()],
            cors_enabled: false,
            cors_origins: Vec::new(),
            enabled: true,
            share_session: true,
        };
        std::fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();

        ensure_dropin(&cfg).expect("ensure");
        let raw = std::fs::read(&path).unwrap();
        let parsed: OAuthClientFile = serde_json::from_slice(&raw).unwrap();
        assert!(
            parsed.share_session,
            "shareSession must survive redirect URI merge"
        );
        assert!(parsed
            .redirect_uris
            .iter()
            .any(|u| u == "http://example.test/cb"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
