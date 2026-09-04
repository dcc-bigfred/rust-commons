//! Accept loop: one-shot or persistent sessions, auth, client cap, command dispatch.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use serde_json::Value;
use thiserror::Error;

use super::auth::Auth;
use super::bind::{bind, BindError, BindOptions};
use super::command::{json_type_field, Connection, RejectReason, Router};
use super::frame::FrameError;

/// One request then close, or request loop until EOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// micronet / microdns / microinit (non-stream).
    OneShot,
    /// microwaf / wireless-programmer.
    Persistent,
}

/// Dispatch / handler failures (not wire-shaped; daemons map to their JSON).
#[derive(Debug, Error)]
pub enum IpcError {
    /// Framing / JSON.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// Handler failure with a display message.
    #[error("{0}")]
    Other(String),
    /// End the session without writing an error (e.g. streaming command done).
    #[error("session closed")]
    Hangup,
}

/// Per-daemon encoding of unknown-type / handler / reject onto the wire.
pub trait ErrorHandler<S>: Send + Sync + 'static {
    /// No command registered for `type_name`.
    fn unknown(&self, state: &S, type_name: &str, body: &Value, conn: &mut Connection);
    /// [`Command::execute`] returned [`IpcError`].
    fn error(&self, state: &S, err: &IpcError, conn: &mut Connection);
    /// Auth or busy reject (optional reply, then drop).
    fn reject(&self, state: &S, reason: RejectReason, conn: &mut Connection);
}

/// Accept-loop policy (auth, session, caps).
#[derive(Debug, Clone)]
pub struct AcceptPolicy {
    /// Peer authentication.
    pub auth: Auth,
    /// One request per connection vs request loop.
    pub session: SessionMode,
    /// Concurrent client cap; `None` is unbounded.
    pub max_clients: Option<usize>,
    /// Frame payload cap in bytes.
    pub max_frame: usize,
}

/// Bound server ready to accept.
pub struct Server<S, H> {
    listener: UnixListener,
    path: PathBuf,
    max_clients: Option<usize>,
    max_frame: usize,
    auth: Auth,
    session: SessionMode,
    router: Arc<Router<S>>,
    hooks: Arc<H>,
}

impl<S, H> Server<S, H>
where
    S: Send + Sync + 'static,
    H: ErrorHandler<S>,
{
    /// Bind using [`BindOptions`], then wrap accept policy.
    ///
    /// The socket is claimed as a singleton: a live peer is
    /// [`BindError::AlreadyRunning`], never unlinked out from under it.
    ///
    /// # Errors
    ///
    /// [`BindError`] from claim/bind/chmod.
    pub fn bind(
        opts: BindOptions,
        policy: AcceptPolicy,
        router: Router<S>,
        hooks: H,
    ) -> Result<Self, BindError> {
        let path = opts.path.clone();
        let listener = bind(&opts)?;
        Ok(Self {
            listener,
            path,
            max_clients: policy.max_clients,
            max_frame: policy.max_frame,
            auth: policy.auth,
            session: policy.session,
            router: Arc::new(router),
            hooks: Arc::new(hooks),
        })
    }

    /// Wrap an already-bound listener (claim/chmod done by the caller).
    #[must_use]
    pub fn from_listener(
        listener: UnixListener,
        path: PathBuf,
        policy: AcceptPolicy,
        router: Router<S>,
        hooks: H,
    ) -> Self {
        Self {
            listener,
            path,
            max_clients: policy.max_clients,
            max_frame: policy.max_frame,
            auth: policy.auth,
            session: policy.session,
            router: Arc::new(router),
            hooks: Arc::new(hooks),
        }
    }

    /// Blocking accept loop. Ends when the socket path disappears.
    pub fn serve(self, state: Arc<S>) {
        serve_listener(
            self.listener,
            self.path,
            state,
            self.router,
            self.hooks,
            self.auth,
            self.session,
            self.max_clients,
            self.max_frame,
        );
    }

    /// Spawn the accept loop on a background thread named `ctl`.
    ///
    /// # Errors
    ///
    /// Thread spawn failure.
    pub fn serve_background(self, state: Arc<S>) -> io::Result<()> {
        thread::Builder::new()
            .name("ctl".into())
            .spawn(move || self.serve(state))?;
        Ok(())
    }
}

/// Bind + background serve (micronet/microdns/microinit style).
///
/// # Errors
///
/// Bind or thread spawn.
pub fn serve_background<S, H>(
    opts: BindOptions,
    policy: AcceptPolicy,
    router: Router<S>,
    hooks: H,
    state: Arc<S>,
) -> Result<(), BindError>
where
    S: Send + Sync + 'static,
    H: ErrorHandler<S>,
{
    let server = Server::bind(opts, policy, router, hooks)?;
    server.serve_background(state).map_err(|e| BindError::Io {
        path: PathBuf::from("ctl thread"),
        source: e,
    })
}

/// Bind + blocking serve (microwaf/WP style).
///
/// # Errors
///
/// Bind failure.
pub fn serve<S, H>(
    opts: BindOptions,
    policy: AcceptPolicy,
    router: Router<S>,
    hooks: H,
    state: Arc<S>,
) -> Result<(), BindError>
where
    S: Send + Sync + 'static,
    H: ErrorHandler<S>,
{
    let server = Server::bind(opts, policy, router, hooks)?;
    server.serve(state);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn serve_listener<S, H>(
    listener: UnixListener,
    path: PathBuf,
    state: Arc<S>,
    router: Arc<Router<S>>,
    hooks: Arc<H>,
    auth: Auth,
    session: SessionMode,
    max_clients: Option<usize>,
    max_frame: usize,
) where
    S: Send + Sync + 'static,
    H: ErrorHandler<S>,
{
    let clients = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Some(max) = max_clients {
                    if clients.load(Ordering::SeqCst) >= max {
                        let mut c = Connection::new(stream, max_frame);
                        hooks.reject(state.as_ref(), RejectReason::Busy, &mut c);
                        continue;
                    }
                    clients.fetch_add(1, Ordering::SeqCst);
                }
                let state = Arc::clone(&state);
                let router = Arc::clone(&router);
                let hooks = Arc::clone(&hooks);
                let auth = auth.clone();
                let clients = Arc::clone(&clients);
                let capped = max_clients.is_some();
                thread::spawn(move || {
                    handle_conn(
                        stream,
                        state.as_ref(),
                        router.as_ref(),
                        hooks.as_ref(),
                        &auth,
                        session,
                        max_frame,
                    );
                    if capped {
                        clients.fetch_sub(1, Ordering::SeqCst);
                    }
                });
            }
            Err(_) => {
                if !path.exists() {
                    break;
                }
            }
        }
    }
}

fn handle_conn<S: 'static, H>(
    stream: UnixStream,
    state: &S,
    router: &Router<S>,
    hooks: &H,
    auth: &Auth,
    session: SessionMode,
    max_frame: usize,
) where
    H: ErrorHandler<S>,
{
    let mut conn = Connection::new(stream, max_frame);
    if !auth.allows(conn.stream()) {
        hooks.reject(state, RejectReason::Auth, &mut conn);
        return;
    }
    match session {
        SessionMode::OneShot => match dispatch_one(state, router, hooks, &mut conn) {
            Ok(()) => {}
            Err(e) => hooks.error(state, &IpcError::Frame(e), &mut conn),
        },
        SessionMode::Persistent => loop {
            match dispatch_one(state, router, hooks, &mut conn) {
                Ok(()) => {}
                Err(FrameError::UnexpectedEof { .. }) => break,
                Err(FrameError::Io(e))
                    if e.kind() == io::ErrorKind::UnexpectedEof
                        || e.kind() == io::ErrorKind::ConnectionReset =>
                {
                    break;
                }
                Err(e) => {
                    hooks.error(state, &IpcError::Frame(e), &mut conn);
                    break;
                }
            }
        },
    }
}

fn dispatch_one<S: 'static, H>(
    state: &S,
    router: &Router<S>,
    hooks: &H,
    conn: &mut Connection,
) -> Result<(), FrameError>
where
    H: ErrorHandler<S>,
{
    let body: Value = conn.read_value()?;
    let Some(name) = json_type_field(&body) else {
        hooks.unknown(state, "", &body, conn);
        return Ok(());
    };
    let Some(cmd) = router.get(name) else {
        hooks.unknown(state, name, &body, conn);
        return Ok(());
    };
    if let Err(e) = cmd.execute(state, body, conn) {
        match e {
            IpcError::Hangup => return Err(FrameError::UnexpectedEof { read: 0, needed: 0 }),
            other => hooks.error(state, &other, conn),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream as StdUnixStream;

    use serde_json::json;

    use super::*;
    use crate::ipc::bind;
    use crate::ipc::command::Command;
    use crate::ipc::frame::{read_frame, write_frame, FrameError};
    use crate::ipc::BindOptions;

    struct Echo;

    impl Command<AtomicUsize> for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn execute(
            &self,
            state: &AtomicUsize,
            body: Value,
            conn: &mut Connection,
        ) -> Result<(), IpcError> {
            state.fetch_add(1, Ordering::SeqCst);
            conn.reply(&body).map_err(IpcError::from)
        }
    }

    struct Hooks;

    impl ErrorHandler<AtomicUsize> for Hooks {
        fn unknown(
            &self,
            _state: &AtomicUsize,
            type_name: &str,
            _body: &Value,
            conn: &mut Connection,
        ) {
            let _ = conn.reply(&json!({"error": format!("unknown:{type_name}")}));
        }
        fn error(&self, _state: &AtomicUsize, err: &IpcError, conn: &mut Connection) {
            let _ = conn.reply(&json!({"error": err.to_string()}));
        }
        fn reject(&self, _state: &AtomicUsize, reason: RejectReason, conn: &mut Connection) {
            let code = match reason {
                RejectReason::Auth => "auth",
                RejectReason::Busy => "busy",
            };
            let _ = conn.reply(&json!({"error": code}));
        }
    }

    #[test]
    fn oneshot_echo_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("t.sock");
        let mut router = Router::new();
        router.add(Echo).unwrap();
        let opts = BindOptions {
            path: sock.clone(),
            mode: 0o600,
            chown: None,
            process_name: "bigfred-shared-daemon-test",
        };
        let state = Arc::new(AtomicUsize::new(0));
        let server = Server::bind(
            opts,
            AcceptPolicy {
                auth: Auth::None,
                session: SessionMode::OneShot,
                max_clients: None,
                max_frame: 64 * 1024,
            },
            router,
            Hooks,
        )
        .unwrap();
        server.serve_background(Arc::clone(&state)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut c = StdUnixStream::connect(&sock).unwrap();
        write_frame(&mut c, &json!({"type":"echo","n":1})).unwrap();
        let back: Value = read_frame(&mut c).unwrap();
        assert_eq!(back["n"], 1);

        let mut c = StdUnixStream::connect(&sock).unwrap();
        write_frame(&mut c, &json!({"type":"nope"})).unwrap();
        let back: Value = read_frame(&mut c).unwrap();
        assert_eq!(back["error"], "unknown:nope");

        assert_eq!(state.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn singleton_refuses_second_bind() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("live.sock");
        let opts = BindOptions {
            path: sock.clone(),
            mode: 0o600,
            chown: None,
            process_name: "bigfred-shared-daemon-test",
        };
        let _first = bind(&opts).unwrap();
        let err = bind(&opts).unwrap_err();
        assert!(matches!(err, BindError::AlreadyRunning { .. }));
        assert!(err.to_string().contains("already running"));
    }

    struct HangupCmd;

    impl Command<AtomicUsize> for HangupCmd {
        fn name(&self) -> &'static str {
            "done"
        }
        fn execute(
            &self,
            _state: &AtomicUsize,
            _body: Value,
            conn: &mut Connection,
        ) -> Result<(), IpcError> {
            conn.reply(&json!({"ok": true})).map_err(IpcError::from)?;
            Err(IpcError::Hangup)
        }
    }

    #[test]
    fn persistent_hangup_closes_without_error_frame() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let mut router = Router::new();
        router.add(HangupCmd).unwrap();
        let opts = BindOptions {
            path: sock.clone(),
            mode: 0o600,
            chown: None,
            process_name: "bigfred-shared-daemon-test",
        };
        let server = Server::bind(
            opts,
            AcceptPolicy {
                auth: Auth::None,
                session: SessionMode::Persistent,
                max_clients: None,
                max_frame: 64 * 1024,
            },
            router,
            Hooks,
        )
        .unwrap();
        server
            .serve_background(Arc::new(AtomicUsize::new(0)))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut c = StdUnixStream::connect(&sock).unwrap();
        write_frame(&mut c, &json!({"type": "done"})).unwrap();
        let back: Value = read_frame(&mut c).unwrap();
        assert_eq!(back["ok"], true);
        let err = read_frame::<_, Value>(&mut c).unwrap_err();
        assert!(matches!(err, FrameError::UnexpectedEof { .. }));
    }

    #[test]
    fn stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("stale.sock");
        let opts = BindOptions {
            path: sock.clone(),
            mode: 0o600,
            chown: None,
            process_name: "bigfred-shared-daemon-test",
        };
        let first = bind(&opts).unwrap();
        drop(first);
        let _second = bind(&opts).unwrap();
    }
}
