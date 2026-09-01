//! Snapshot of BigFred connectivity settings. Callers own hot-reload.

use std::path::PathBuf;

/// Values the client needs to talk to BigFred. Built by the host from its
/// own config; this crate does not read wizard JSON or `$DATA_DIR`.
#[derive(Debug, Clone)]
pub struct BigFredConfig {
    /// Loopback HTTP origin, no trailing slash (`http://127.0.0.1:8080`).
    pub api_base: String,
    /// Same origin with `ws`/`wss`, no trailing slash.
    pub ws_base: String,
    /// OAuth client id written into the drop-in filename.
    pub sso_client_id: String,
    /// Redirect URIs already merged with the host's builtins.
    pub redirect_uris: Vec<String>,
    /// `$DATA_DIR/etc/bigfred/oauth-clients`.
    pub oauth_dropin_dir: PathBuf,
    /// `displayName` written into a newly seeded drop-in.
    pub oauth_display_name: String,
    /// Skip catalogue autodetection when both ids are set.
    pub fixed_dcc_bus: Option<(u64, u64)>,
}

impl BigFredConfig {
    /// Drop-in path for [`Self::sso_client_id`].
    pub fn oauth_client_path(&self) -> PathBuf {
        self.oauth_dropin_dir
            .join(format!("{}.json", self.sso_client_id))
    }
}

#[cfg(test)]
pub(crate) fn test_config(dropin_dir: PathBuf) -> BigFredConfig {
    BigFredConfig {
        api_base: "http://127.0.0.1:8080".into(),
        ws_base: "ws://127.0.0.1:8080".into(),
        sso_client_id: "bigfred-wizard".into(),
        redirect_uris: vec![
            "http://bigfred.local:8091/auth/callback".into(),
            "http://localhost:8091/auth/callback".into(),
        ],
        oauth_dropin_dir: dropin_dir,
        oauth_display_name: "BigFred Wizard".into(),
        fixed_dcc_bus: None,
    }
}
