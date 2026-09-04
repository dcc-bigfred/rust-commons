//! Config loaders and inotify hot-reload.

mod json;
mod sighup;
mod watch;

use std::sync::RwLock;

pub use json::JsonFile;
pub use sighup::install_sighup_flag;
pub use watch::{
    is_junk_name, is_relevant_path, spawn_callback, spawn_signal, PathFilter, Reload, WatchSpec,
    DEFAULT_DEBOUNCE,
};

/// Load a configuration snapshot from some backing store.
pub trait Load: Send + Sync {
    /// Loaded value type.
    type Value: Send + Sync;
    /// Read and parse the current snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the backing store cannot be read or parsed.
    fn load(&self) -> Result<Self::Value, ConfigError>;
}

/// Errors from config IO / JSON.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Filesystem failure, optionally with the path involved.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path being read or written.
        path: String,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// JSON (de)serialisation failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Watcher / thread setup.
    #[error("{0}")]
    Other(String),
}

impl ConfigError {
    /// Wrap an IO error with a path display.
    #[must_use]
    pub fn io_at(path: impl AsRef<std::path::Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }
}

/// Process-shared live snapshot. A failed [`Load::load`] must not call [`Live::replace`].
#[derive(Debug)]
pub struct Live<T> {
    inner: RwLock<T>,
}

impl<T> Live<T> {
    /// Wrap an initial value.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
        }
    }

    /// Replace the snapshot (only after a successful load).
    pub fn replace(&self, next: T) {
        match self.inner.write() {
            Ok(mut g) => *g = next,
            Err(p) => *p.into_inner() = next,
        }
    }

    /// Borrow the current snapshot.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        match self.inner.read() {
            Ok(g) => f(&g),
            Err(p) => f(&p.into_inner()),
        }
    }

    /// Clone the current snapshot.
    #[must_use]
    pub fn snapshot(&self) -> T
    where
        T: Clone,
    {
        self.with(Clone::clone)
    }
}

/// Load `source` and swap into `live` only on success.
///
/// # Errors
///
/// Propagates [`Load::load`] without touching `live`.
pub fn try_reload<L: Load>(source: &L, live: &Live<L::Value>) -> Result<(), ConfigError> {
    let next = source.load()?;
    live.replace(next);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Boom;

    impl Load for Boom {
        type Value = u32;
        fn load(&self) -> Result<u32, ConfigError> {
            Err(ConfigError::Other("nope".into()))
        }
    }

    struct Once(std::sync::atomic::AtomicBool);

    impl Load for Once {
        type Value = u32;
        fn load(&self) -> Result<u32, ConfigError> {
            if self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return Err(ConfigError::Other("second".into()));
            }
            Ok(7)
        }
    }

    #[test]
    fn failed_reload_keeps_previous() {
        let live = Live::new(1u32);
        assert!(try_reload(&Boom, &live).is_err());
        assert_eq!(live.snapshot(), 1);

        let src = Once(std::sync::atomic::AtomicBool::new(false));
        try_reload(&src, &live).unwrap();
        assert_eq!(live.snapshot(), 7);
        assert!(try_reload(&src, &live).is_err());
        assert_eq!(live.snapshot(), 7);
    }
}
