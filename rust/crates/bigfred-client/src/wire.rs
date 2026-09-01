//! Wire types shared with BigFred (OAuth, catalogue, dcc-bus acks).

use serde::{Deserialize, Serialize};

/// Impersonation header understood by BigFred's `MaybeImpersonate`.
pub const IMPERSONATE_HEADER: &str = "x-bigfred-impersonate-as";

/// dcc-bus frame type used for ops-track function pulses.
pub const FRAME_SET_FUNCTION: &str = "loco.setFunction";

/// `contract.EnvelopeWire` on the wire.
#[derive(Debug, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// One configuration variable, mirroring `protocol.CVEntry`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CvEntry {
    pub cv: u16,
    pub value: u8,
}

/// `protocol.AckPayload` (only the fields the wizard reads back).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ack {
    #[serde(default)]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cvs: Option<Vec<CvEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loco_address: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_address: Option<bool>,
}

/// What programming status reports (socket + station).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_station_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_station_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_programming_track_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub reconnects: u64,
    /// Impersonated drive socket is up (F2 / ops track).
    pub drive_connected: bool,
    /// Participant the drive socket is impersonating, when connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive_as: Option<String>,
}

/// One row of `GET /api/v1/command-stations/catalogue`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandStation {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub programming: bool,
    #[serde(default)]
    pub hide_in_throttle: bool,
    #[serde(default)]
    pub default_programming_track_output: String,
}

/// Successful `POST /api/v1/auth/oauth/token` body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_at: String,
}
