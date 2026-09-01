//! Typed errors with stable string codes the host maps onto HTTP.

use std::path::PathBuf;

/// Failures talking to BigFred or maintaining the OAuth drop-in.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("oauth unreachable: {0}")]
    OauthUnreachable(String),
    #[error("proxy unreachable: {0}")]
    ProxyUnreachable(String),
    #[error("proxy read failed: {0}")]
    ProxyReadFailed(String),
    #[error("dcc-bus unreachable: {0}")]
    DccBusUnreachable(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("programming timeout")]
    ProgrammingTimeout,
    #[error("function off timeout")]
    FunctionOffTimeout,
    #[error("{code}")]
    BadStatus {
        status: u16,
        code: String,
        detail: Option<String>,
    },
    #[error("oauth bad response: {0}")]
    OauthBadResponse(String),
    #[error("catalogue unavailable: {0}")]
    CatalogueUnavailable(String),
    #[error("catalogue bad response: {0}")]
    CatalogueBadResponse(String),
    #[error("oauth client ensure failed: {0}")]
    OauthClientEnsureFailed(String),
    #[error("oauth client unreadable: {0}")]
    OauthClientUnreadable(String),
    #[error("oauth client missing")]
    OauthClientMissing,
    #[error("no programming station")]
    NoProgrammingStation,
    #[error("dcc-bus session lost")]
    DccBusSessionLost,
    #[error("dcc-bus drive session lost")]
    DccBusDriveSessionLost,
    #[error("dcc-bus pending poisoned")]
    DccBusPendingPoisoned,
    #[error("dcc-bus bad url: {0}")]
    DccBusBadUrl(String),
    #[error("frame encode failed: {0}")]
    FrameEncodeFailed(String),
    #[error("impersonate required")]
    ImpersonateRequired,
    #[error("invalid impersonate login")]
    InvalidImpersonateLogin,
    #[error("io {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("serialize {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

impl Error {
    /// Stable machine code used by the host HTTP envelope and status logs.
    pub fn code(&self) -> String {
        match self {
            Self::OauthUnreachable(_) | Self::ProxyUnreachable(_) => {
                "bigfred_unreachable".to_string()
            }
            Self::ProxyReadFailed(_) => "bigfred_read_failed".to_string(),
            Self::DccBusUnreachable(_) => "dcc_bus_unavailable".to_string(),
            Self::Unauthorized => "unauthorized".to_string(),
            Self::ProgrammingTimeout => "programming_timeout".to_string(),
            Self::FunctionOffTimeout => "function_off_timeout".to_string(),
            Self::BadStatus { code, .. } => code.clone(),
            Self::OauthBadResponse(_) => "oauth_bad_response".to_string(),
            Self::CatalogueUnavailable(_) => "catalogue_unavailable".to_string(),
            Self::CatalogueBadResponse(_) => "catalogue_bad_response".to_string(),
            Self::OauthClientEnsureFailed(_) => "oauth_client_ensure_failed".to_string(),
            Self::OauthClientUnreadable(_) => "oauth_client_unreadable".to_string(),
            Self::OauthClientMissing => "oauth_client_missing".to_string(),
            Self::NoProgrammingStation => "no_programming_station".to_string(),
            Self::DccBusSessionLost => "dcc_bus_session_lost".to_string(),
            Self::DccBusDriveSessionLost => "dcc_bus_drive_session_lost".to_string(),
            Self::DccBusPendingPoisoned => "dcc_bus_pending_poisoned".to_string(),
            Self::DccBusBadUrl(_) => "dcc_bus_bad_url".to_string(),
            Self::FrameEncodeFailed(_) => "frame_encode_failed".to_string(),
            Self::ImpersonateRequired => "impersonate_required".to_string(),
            Self::InvalidImpersonateLogin => "invalid_impersonate_login".to_string(),
            Self::Io { .. } | Self::Parse { .. } | Self::Serialize { .. } => {
                "oauth_client_unreadable".to_string()
            }
        }
    }

    pub fn is_dcc_bus_unavailable(&self) -> bool {
        matches!(self, Self::DccBusUnreachable(_))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
