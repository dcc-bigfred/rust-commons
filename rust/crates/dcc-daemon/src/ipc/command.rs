//! Command trait, router, and a framed connection.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::frame::{self, FrameError};
use super::server::IpcError;

/// One control-socket method. Wire JSON keeps a `"type"` field equal to [`Command::name`].
pub trait Command<S>: Send + Sync + 'static {
    /// Value of the JSON `"type"` discriminator.
    fn name(&self) -> &'static str;

    /// Handle one request. `body` is the full JSON object.
    /// Streaming commands may write several frames via [`Connection::reply`].
    ///
    /// # Errors
    ///
    /// Implementation-defined; the server maps this through [`super::ErrorHandler`].
    fn execute(&self, state: &S, body: Value, conn: &mut Connection) -> Result<(), IpcError>;
}

/// Why a connection was not dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// [`super::Auth`] denied the peer.
    Auth,
    /// Concurrent client cap reached.
    Busy,
}

/// Duplicate command name in a [`Router`].
#[derive(Debug, Error)]
#[error("duplicate IPC command {0}")]
pub struct DuplicateCommand(pub &'static str);

/// Dispatch table: JSON `"type"` → [`Command`].
pub struct Router<S> {
    commands: HashMap<&'static str, Box<dyn Command<S>>>,
}

impl<S> Router<S> {
    /// Empty router.
    #[must_use]
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    /// Register a command. Last registration for a name errors.
    ///
    /// # Errors
    ///
    /// [`DuplicateCommand`] if `name` is already registered.
    pub fn add<C: Command<S>>(&mut self, cmd: C) -> Result<(), DuplicateCommand> {
        let name = cmd.name();
        if self.commands.contains_key(name) {
            return Err(DuplicateCommand(name));
        }
        self.commands.insert(name, Box::new(cmd));
        Ok(())
    }

    /// Look up by wire type string.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Command<S>> {
        self.commands.get(name).map(|b| &**b)
    }
}

impl<S> Default for Router<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Framed Unix connection for one client.
pub struct Connection {
    stream: UnixStream,
    max_frame: usize,
}

impl Connection {
    /// Wrap an accepted stream.
    #[must_use]
    pub fn new(stream: UnixStream, max_frame: usize) -> Self {
        Self { stream, max_frame }
    }

    /// Write one JSON frame.
    ///
    /// # Errors
    ///
    /// [`FrameError`] on serialise / size / IO.
    pub fn reply<T: Serialize>(&mut self, msg: &T) -> Result<(), FrameError> {
        frame::write_frame_with_limit(&mut self.stream, msg, self.max_frame)
    }

    /// Read one JSON frame as `T`.
    ///
    /// # Errors
    ///
    /// [`FrameError`] on truncate / size / IO / JSON.
    pub fn read_json<T: DeserializeOwned>(&mut self) -> Result<T, FrameError> {
        frame::read_frame_with_limit(&mut self.stream, self.max_frame)
    }

    /// Read one JSON object (command dispatch).
    ///
    /// # Errors
    ///
    /// [`FrameError`] on truncate / size / IO / JSON.
    pub fn read_value(&mut self) -> Result<Value, FrameError> {
        self.read_json()
    }

    /// Borrow the raw stream (streaming / timeouts).
    pub fn stream(&mut self) -> &mut UnixStream {
        &mut self.stream
    }

    /// Configured frame cap.
    #[must_use]
    pub fn max_frame(&self) -> usize {
        self.max_frame
    }
}

/// Read the `"type"` field from a request object.
#[must_use]
pub fn json_type_field(body: &Value) -> Option<&str> {
    body.get("type").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ping;

    impl Command<()> for Ping {
        fn name(&self) -> &'static str {
            "ping"
        }
        fn execute(
            &self,
            _state: &(),
            _body: Value,
            _conn: &mut Connection,
        ) -> Result<(), IpcError> {
            Ok(())
        }
    }

    #[test]
    fn router_rejects_duplicate() {
        let mut r = Router::<()>::new();
        r.add(Ping).unwrap();
        assert!(r.add(Ping).is_err());
        assert!(r.get("ping").is_some());
        assert!(r.get("pong").is_none());
    }

    #[test]
    fn type_field() {
        let v = serde_json::json!({"type": "status", "x": 1});
        assert_eq!(json_type_field(&v), Some("status"));
        assert_eq!(json_type_field(&serde_json::json!({})), None);
    }
}
