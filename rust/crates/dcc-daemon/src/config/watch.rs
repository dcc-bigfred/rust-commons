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
/// Specs whose path is not a directory yet are retried until they appear
/// (late-attach). A failed `watch()` on one spec is skipped; remaining specs
/// keep running.
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
/// Same late-attach and per-spec watch isolation as [`spawn_signal`].
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

/// Register inotify for one spec. Missing recursive trees are left pending
/// (late-attach). A failed `watch()` is logged and skipped so other specs
/// keep working.
fn attach_spec(watcher: &mut RecommendedWatcher, spec: &WatchSpec) -> bool {
    let target = if spec.recursive {
        spec.path.clone()
    } else {
        watch_dir_for(&spec.path)
    };
    if !spec.recursive && !target.is_dir() {
        let _ = std::fs::create_dir_all(&target);
    }
    if !target.is_dir() {
        return false;
    }
    let mode = if spec.recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    match watcher.watch(&target, mode) {
        Ok(()) => {
            log::info!("config watch active on {}", target.display());
            true
        }
        Err(e) => {
            log::warn!("config watch: cannot watch {}: {e}", target.display());
            false
        }
    }
}

fn attach_pending(
    watcher: &mut RecommendedWatcher,
    specs: &[WatchSpec],
    attached: &mut [bool],
) -> bool {
    let mut newly = false;
    for (i, spec) in specs.iter().enumerate() {
        if attached[i] {
            continue;
        }
        if attach_spec(watcher, spec) {
            attached[i] = true;
            newly = true;
        }
    }
    newly
}

fn fire_reload(reload_tx: &Option<Sender<Reload>>, on_reload: &Option<Box<dyn Fn() + Send>>) {
    if let Some(tx) = reload_tx {
        let _ = tx.send(Reload);
    }
    if let Some(cb) = on_reload {
        cb();
    }
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

    let mut attached = vec![false; specs.len()];
    let _ = attach_pending(&mut watcher, &specs, &mut attached);

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
                if attach_pending(&mut watcher, &specs, &mut attached) {
                    pending = Some(Instant::now());
                }
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
                if attach_pending(&mut watcher, &specs, &mut attached) {
                    pending = Some(Instant::now());
                }
                if pending.is_some_and(|t| t.elapsed() >= debounce) {
                    pending = None;
                    fire_reload(&reload_tx, &on_reload);
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
    use std::time::Duration;

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

    #[test]
    fn dir_watch_fires_on_json_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("microinit.json");
        std::fs::write(&path, "{}\n").unwrap();
        let spec = WatchSpec {
            path: dir.path().to_path_buf(),
            recursive: false,
            filter: PathFilter::Any {
                extensions: vec!["json".into()],
                extra_names: vec![
                    "microinit.json".into(),
                    "microinit.services.enabled-override.json".into(),
                    "microinit.d".into(),
                ],
                ignore_suffixes: Vec::new(),
            },
        };
        let (rx, _stop) = spawn_signal(vec![spec], Duration::from_millis(50)).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let tmp = dir.path().join("microinit.json.tmp");
        std::fs::write(&tmp, "{\"v\":1}\n").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        rx.recv_timeout(Duration::from_secs(2))
            .expect("reload after atomic json replace");
    }

    #[test]
    fn late_recursive_dir_attaches_and_fires_reload() {
        let root = tempfile::tempdir().unwrap();
        let etc = root.path().join("etc");
        std::fs::create_dir(&etc).unwrap();
        std::fs::write(etc.join("microinit.json"), "{}\n").unwrap();
        let dropins = etc.join("microinit.d").join("services");
        let filter = PathFilter::Any {
            extensions: vec!["json".into()],
            extra_names: vec!["microinit.json".into(), "microinit.d".into()],
            ignore_suffixes: Vec::new(),
        };
        let specs = vec![
            WatchSpec {
                path: etc.clone(),
                recursive: false,
                filter: filter.clone(),
            },
            WatchSpec {
                path: dropins.clone(),
                recursive: true,
                filter,
            },
        ];
        let (rx, _stop) = spawn_signal(specs, Duration::from_millis(50)).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        while rx.try_recv().is_ok() {}

        std::fs::create_dir_all(&dropins).unwrap();
        std::fs::write(dropins.join("loco.json"), "{\"name\":\"loco\"}\n").unwrap();
        rx.recv_timeout(Duration::from_secs(3))
            .expect("reload after late drop-in create");
    }

    #[test]
    fn failed_recursive_spec_does_not_kill_file_watch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cfg.json");
        std::fs::write(&file, "{}\n").unwrap();
        let not_a_dir = dir.path().join("blocked");
        std::fs::write(&not_a_dir, "x\n").unwrap();
        let specs = vec![
            WatchSpec::file(file.clone()),
            WatchSpec {
                path: not_a_dir,
                recursive: true,
                filter: PathFilter::Any {
                    extensions: vec!["json".into()],
                    extra_names: Vec::new(),
                    ignore_suffixes: Vec::new(),
                },
            },
        ];
        let (rx, _stop) = spawn_signal(specs, Duration::from_millis(50)).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        while rx.try_recv().is_ok() {}
        std::fs::write(&file, "{\"a\":1}\n").unwrap();
        rx.recv_timeout(Duration::from_secs(2))
            .expect("file watch still live after sibling spec failed");
    }
}
