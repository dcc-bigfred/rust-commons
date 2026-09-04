//! SO_PEERCRED helpers and auth policy.

use std::os::unix::net::UnixStream;

/// Peer authentication policy. Checked after accept, before dispatch.
#[derive(Debug, Clone)]
pub enum Auth {
    /// Unix mode bits only (typical `0600` sockets).
    None,
    /// Allow daemon uid and an extra uid list (microinit `socketAllowUsers`).
    PeerUid {
        /// Daemon process uid (always allowed).
        daemon_uid: u32,
        /// Extra allowed uids.
        allow_uids: Vec<u32>,
    },
    /// Match peer login name against an allow-list.
    PeerUser {
        /// Login names.
        allow_users: Vec<String>,
        /// UID 0 always allowed (microwaf).
        root_always: bool,
        /// Empty allow-list: `true` fail-closed (WP require-auth); `false` allow all (microwaf).
        fail_closed_if_empty: bool,
    },
}

impl Auth {
    /// Whether this peer may use the control socket.
    #[must_use]
    pub fn allows(&self, stream: &UnixStream) -> bool {
        match self {
            Self::None => true,
            Self::PeerUid {
                daemon_uid,
                allow_uids,
            } => match peer_uid(stream) {
                Some(uid) => uid == *daemon_uid || allow_uids.contains(&uid),
                None => false,
            },
            Self::PeerUser {
                allow_users,
                root_always,
                fail_closed_if_empty,
            } => {
                if allow_users.is_empty() {
                    return !*fail_closed_if_empty;
                }
                match peer_uid(stream) {
                    Some(0) if *root_always => true,
                    Some(uid) => match username_for_uid(uid) {
                        Some(name) => allow_users.iter().any(|u| u == &name),
                        None => false,
                    },
                    None => false,
                }
            }
        }
    }
}

/// Peer uid via `SO_PEERCRED`, if available.
#[must_use]
pub fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    getsockopt(stream, PeerCredentials).ok().map(|c| c.uid())
}

/// Peer pid via `SO_PEERCRED`, or `0` if unavailable.
#[must_use]
pub fn peer_pid(stream: &UnixStream) -> u32 {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    getsockopt(stream, PeerCredentials)
        .map(|c| c.pid() as u32)
        .unwrap_or(0)
}

/// Login name for `uid`, if present in the passwd database.
#[must_use]
pub fn username_for_uid(uid: u32) -> Option<String> {
    use nix::unistd::{Uid, User};
    User::from_uid(Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.name)
}
