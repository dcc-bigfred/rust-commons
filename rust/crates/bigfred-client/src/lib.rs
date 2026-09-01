//! HTTP, OAuth drop-in, and dcc-bus WebSocket client for BigFred.
//!
//! No axum: the host owns HTTP handlers and maps [`Error`] onto its
//! envelope. Config is a snapshot ([`BigFredConfig`]); the host refreshes
//! it on reload.

pub mod apis;
mod config;
mod dccbus;
mod error;
pub mod oauth;
pub mod proxy;
mod wire;

pub use config::BigFredConfig;
pub use dccbus::DccBusClient;
pub use error::{Error, Result};
pub use wire::{
    Ack, CommandStation, CvEntry, Envelope, Status, TokenResponse, FRAME_SET_FUNCTION,
    IMPERSONATE_HEADER,
};
