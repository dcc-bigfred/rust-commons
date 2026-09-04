//! Bind / claim a Unix control socket.
//!
//! IPC is **always a singleton**. [`bind`] and [`claim`] probe the path with
//! `connect` before touching the inode:
//!
//! - a live peer → [`BindError::AlreadyRunning`]; the path is left untouched
//! - a stale leftover (`ENOENT` / `ECONNREFUSED`) → unlink, then bind
//!
//! A second process must never unlink a live socket. That would steal the path
//! from the running daemon and break its clients.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use super::auth::peer_pid;
use thiserror::Error;

/// Options for claiming and binding a control socket.
///
/// Bind is always singleton: see the [module-level docs](self).
#[derive(Debug, Clone)]
pub struct BindOptions {
    /// Socket path.
    pub path: PathBuf,
    /// Permission bits after bind (e.g. `0o600`, `0o660`, `0o666`).
    pub mode: u32,
    /// Optional `chown(uid, gid)` after chmod.
    pub chown: Option<(u32, u32)>,
    /// Process name used in [`BindError::AlreadyRunning`] (`"micronet"`).
    pub process_name: &'static str,
}

/// Bind / claim failures.
#[derive(Debug, Error)]
pub enum BindError {
    /// Another daemon answered `connect` on the socket. The inode was not unlinked.
    #[error("{process_name} already running at {location}")]
    AlreadyRunning {
        /// Daemon name for the error string.
        process_name: &'static str,
        /// Path, optionally with `(pid N)`.
        location: String,
        /// Socket path.
        path: PathBuf,
        /// Peer pid when available.
        pid: u32,
    },
    /// Filesystem or bind failure.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
}

impl BindError {
    fn io_at(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Probe `path` and, if it is stale, unlink the leftover inode. Does not bind.
///
/// A live peer is an error: this never unlinks a socket that accepted `connect`.
///
/// # Errors
///
/// [`BindError::AlreadyRunning`] or IO.
pub fn claim(path: &Path, process_name: &'static str) -> Result<(), BindError> {
    match UnixStream::connect(path) {
        Ok(stream) => {
            let pid = peer_pid(&stream);
            let location = if pid != 0 {
                format!("{} (pid {pid})", path.display())
            } else {
                path.display().to_string()
            };
            return Err(BindError::AlreadyRunning {
                process_name,
                location,
                path: path.to_path_buf(),
                pid,
            });
        }
        Err(e) if is_stale_socket_connect_error(&e) => {}
        Err(e) => return Err(BindError::io_at(path, e)),
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(BindError::io_at(path, e)),
    }
    Ok(())
}

fn is_stale_socket_connect_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Create parent dirs, claim (singleton), bind, chmod, optional chown.
///
/// # Errors
///
/// [`BindError`] on claim, bind, or permission failures.
pub fn bind(opts: &BindOptions) -> Result<UnixListener, BindError> {
    if let Some(parent) = opts.path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| BindError::io_at(parent, e))?;
        }
    }
    claim(&opts.path, opts.process_name)?;
    let listener = UnixListener::bind(&opts.path).map_err(|e| BindError::io_at(&opts.path, e))?;
    apply_socket_perms(&opts.path, opts.mode).map_err(|e| BindError::io_at(&opts.path, e))?;
    if let Some((uid, gid)) = opts.chown {
        chown_socket(&opts.path, uid, gid).map_err(|e| {
            BindError::io_at(&opts.path, io::Error::other(format!("chown socket: {e}")))
        })?;
    }
    Ok(listener)
}

/// `chmod` the socket path.
///
/// # Errors
///
/// IO on stat/chmod.
pub fn apply_socket_perms(socket_path: &Path, mode: u32) -> io::Result<()> {
    let mut perms = std::fs::metadata(socket_path)?.permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(socket_path, perms)?;
    Ok(())
}

/// `chown` the socket path.
///
/// # Errors
///
/// nix chown failure.
pub fn chown_socket(socket_path: &Path, uid: u32, gid: u32) -> Result<(), nix::Error> {
    use nix::unistd::{chown, Gid, Uid};
    chown(
        socket_path,
        Some(Uid::from_raw(uid)),
        Some(Gid::from_raw(gid)),
    )
}
