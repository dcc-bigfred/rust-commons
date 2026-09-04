//! Linux inotify-based configuration watcher (no polling).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::ConfigError;

/// Default debounce for atomic write+rename bursts.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);

/// Signal that watched configuration may have changed.
pub struct Reload;

/// How to filter inotify paths under a [`WatchSpec`].
#[derive(Debug, Clone)]
pub enum PathFilter {
    /// Basename equals this (single config file).
    Basename(String),
    /// Non-junk names; optional extension allow-list and extra exact names
    /// (e.g. `microinit.d`).
    Any {
        /// If non-empty, the file extension must match one of these (no dot).
        extensions: Vec<String>,
        /// Extra exact file names that always match (directories, override files).
        extra_names: Vec<String>,
        /// Suffixes that never match (e.g. `.example`).
        ignore_suffixes: Vec<String>,
    },
}

/// One watched path (file or directory).
#[derive(Debug, Clone)]
pub struct WatchSpec {
    /// File to watch (parent dir is registered) or directory.
    pub path: PathBuf,
    /// Recursive watch (drop-in trees).
    pub recursive: bool,
    /// Event path filter.
    pub filter: PathFilter,
}

impl WatchSpec {
    /// Watch a single file by basename in its parent directory.
    #[must_use]
    pub fn file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("config")
            .to_string();
        Self {
            path,
            recursive: false,
            filter: PathFilter::Basename(name),
        }
    }

    /// Watch a directory.
    #[must_use]
    pub fn dir(path: impl Into<PathBuf>, recursive: bool) -> Self {
        Self {
            path: path.into(),
            recursive,
            filter: PathFilter::Any {
                extensions: Vec::new(),
                extra_names: Vec::new(),
                ignore_suffixes: Vec::new(),
            },
        }
    }
}

/// Editor/temp artifacts that must not trigger reload.
#[must_use]
pub fn is_junk_name(name: &str) -> bool {
    name.starts_with('.') || name.ends_with('~') || name.ends_with(".swp") || name.ends_with(".tmp")
}

/// Filter a filesystem event path against a config identity.
#[must_use]
pub fn is_relevant_path(path: &Path, filter: &PathFilter) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if is_junk_name(name) {
        return false;
    }
    match filter {
        PathFilter::Basename(want) => name == want,
        PathFilter::Any {
            extensions,
            extra_names,
            ignore_suffixes,
        } => {
            if ignore_suffixes.iter().any(|s| name.ends_with(s)) {
                return false;
            }
            if extra_names.iter().any(|n| n == name) {
                return true;
            }
            if extensions.is_empty() {
                return true;
            }
            path.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| extensions.iter().any(|e| e == ext))
        }
    }
}

/// Spawn an inotify thread. Returns debounce-coalesced [`Reload`] signals.
///
/// # Errors
///
/// Fails if the watcher thread cannot be started.
pub fn spawn_signal(
    specs: Vec<WatchSpec>,
    debounce: Duration,
) -> Result<(Receiver<Reload>, Arc<AtomicBool>), ConfigError> {
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thr = Arc::clone(&stop);
    thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            if let Err(e) = watch_loop(specs, debounce, stop_thr, Some(tx), None) {
                log::warn!("config watcher stopped: {e}");
            }
        })
        .map_err(|e| ConfigError::Other(e.to_string()))?;
    Ok((rx, stop))
}

/// Spawn an inotify thread that invokes `on_reload` after debounce (and does
/// not enqueue a channel signal).
///
/// # Errors
///
/// Fails if the watcher thread cannot be started.
pub fn spawn_callback<F>(
    specs: Vec<WatchSpec>,
    debounce: Duration,
    on_reload: F,
) -> Result<Arc<AtomicBool>, ConfigError>
where
    F: Fn() + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thr = Arc::clone(&stop);
    thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            if let Err(e) = watch_loop(
                specs,
                debounce,
                stop_thr,
                None,
                Some(Box::new(on_reload) as Box<dyn Fn() + Send>),
            ) {
                log::warn!("config watcher stopped: {e}");
            }
        })
        .map_err(|e| ConfigError::Other(e.to_string()))?;
    Ok(stop)
}

fn watch_loop(
    specs: Vec<WatchSpec>,
    debounce: Duration,
    stop: Arc<AtomicBool>,
    reload_tx: Option<Sender<Reload>>,
    on_reload: Option<Box<dyn Fn() + Send>>,
) -> Result<(), ConfigError> {
    let (raw_tx, raw_rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: std::result::Result<Event, notify::Error>| {
            let _ = raw_tx.send(res);
        },
        notify::Config::default(),
    )
    .map_err(|e| ConfigError::Other(format!("inotify watcher: {e}")))?;

    for spec in &specs {
        let watch_dir = watch_dir_for(&spec.path);
        if !watch_dir.is_dir() {
            let _ = std::fs::create_dir_all(&watch_dir);
        }
        let mode = if spec.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        if watch_dir.is_dir() {
            watcher
                .watch(&watch_dir, mode)
                .map_err(|e| ConfigError::Other(format!("watch {}: {e}", watch_dir.display())))?;
            log::info!("config watch active on {}", watch_dir.display());
        } else if let Some(parent) = watch_dir.parent() {
            if parent.is_dir() {
                let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
            }
        }
    }

    let mut pending: Option<Instant> = None;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let timeout = pending
            .map(|t| {
                let elapsed = t.elapsed();
                if elapsed >= debounce {
                    Duration::from_millis(0)
                } else {
                    debounce.saturating_sub(elapsed)
                }
            })
            .unwrap_or(Duration::from_secs(1));

        match raw_rx.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                let relevant = matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                        | EventKind::Any
                ) && event
                    .paths
                    .iter()
                    .any(|p| specs.iter().any(|s| is_relevant_path(p, &s.filter)));
                if relevant {
                    pending = Some(Instant::now());
                }
            }
            Ok(Err(e)) => {
                log::warn!("config watch error: {e}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if pending.is_some_and(|t| t.elapsed() >= debounce) {
                    pending = None;
                    if let Some(tx) = &reload_tx {
                        let _ = tx.send(Reload);
                    }
                    if let Some(cb) = &on_reload {
                        cb();
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn watch_dir_for(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn relevant_filters_basename() {
        let f = PathFilter::Basename("micronet.json".into());
        assert!(is_relevant_path(Path::new("/data/etc/micronet.json"), &f));
        assert!(!is_relevant_path(Path::new("/data/etc/.micronet.json"), &f));
        assert!(!is_relevant_path(Path::new("/data/etc/micronet.json~"), &f));
        assert!(!is_relevant_path(Path::new("/data/etc/other.json"), &f));
        assert!(!is_relevant_path(
            Path::new("/data/etc/micronet.json.tmp"),
            &f
        ));
        assert!(!is_relevant_path(
            Path::new("/data/etc/micronet.json.swp"),
            &f
        ));
    }

    #[test]
    fn relevant_filters_dropins() {
        let f = PathFilter::Any {
            extensions: vec!["json".into()],
            extra_names: vec!["microinit.d".into()],
            ignore_suffixes: Vec::new(),
        };
        assert!(is_relevant_path(Path::new("/data/etc/microinit.json"), &f));
        assert!(is_relevant_path(Path::new("/data/etc/microinit.d"), &f));
        assert!(is_relevant_path(
            Path::new("/data/etc/microinit.d/loco.json"),
            &f
        ));
        assert!(!is_relevant_path(Path::new("/data/etc/notes.txt"), &f));
        assert!(!is_relevant_path(Path::new("/data/etc/.hidden.json"), &f));
    }
}
