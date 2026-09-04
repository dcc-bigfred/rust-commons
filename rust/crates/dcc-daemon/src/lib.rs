//! Shared runtime pieces for BigFred OS daemons.
//!
//! Memory profile: **allocation-conscious** (administrative paths, not a packet
//! hot path). IPC frames are bounded by [`ipc::DEFAULT_MAX_FRAME_BYTES`] or a
//! caller-supplied cap.
//!
//! - [`datadir`]: `$DATA_DIR` / `$BIGFRED_DATA_DIR` resolution (always on).
//! - [`config`]: JSON load + inotify hot-reload (`config` feature).
//! - [`ipc`]: length-prefixed JSON Unix socket + [`ipc::Command`] (`ipc` feature).
//!   Bind is always a **singleton**: a live socket is an error, never stolen.

#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod datadir;

#[cfg(feature = "config")]
pub mod config;

#[cfg(feature = "ipc")]
pub mod ipc;

pub use datadir::{
    path, root, set_root, DataDir, EnvPolicy, PathRule, DEFAULT_ROOT, ENV_BIGFRED_DATA_DIR,
    ENV_DATA_DIR,
};
