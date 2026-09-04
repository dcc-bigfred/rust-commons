//! Unix-socket IPC: length-prefixed JSON, singleton bind, auth, and a [`Command`] router.
//!
//! Binding a control socket is always singleton: a live peer at the path is
//! [`BindError::AlreadyRunning`], never an unlink that would steal the inode.

mod auth;
mod bind;
mod command;
mod frame;
mod server;

pub use auth::{peer_pid, peer_uid, username_for_uid, Auth};
pub use bind::{apply_socket_perms, bind, chown_socket, claim, BindError, BindOptions};
pub use command::{json_type_field, Command, Connection, DuplicateCommand, RejectReason, Router};
pub use frame::{
    read_frame, read_frame_bytes, read_frame_with_limit, write_frame, write_frame_bytes,
    write_frame_with_limit, FrameError, DEFAULT_MAX_FRAME_BYTES,
};
pub use server::{
    serve, serve_background, AcceptPolicy, ErrorHandler, IpcError, Server, SessionMode,
};
