//! Persistent data root: `DATA_DIR` / `BIGFRED_DATA_DIR`, then `/data`.

use std::path::{Path, PathBuf};

/// Env var for the persistent data root (hub daemons).
pub const ENV_DATA_DIR: &str = "DATA_DIR";
/// Alternate env var used by some BigFred-branded tools.
pub const ENV_BIGFRED_DATA_DIR: &str = "BIGFRED_DATA_DIR";
/// Hub image default when no env var is set.
pub const DEFAULT_ROOT: &str = "/data";

/// Which environment variables participate in resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvPolicy {
    /// `DATA_DIR` only (micronet, microdns, microinit).
    DataDir,
    /// `BIGFRED_DATA_DIR`, then `DATA_DIR` (microwaf socket, wireless-programmer).
    BigfredThenDataDir,
}

/// How a value from the environment is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRule {
    /// Ignore empty and relative values (cannot silently redirect under cwd).
    AbsoluteOnly,
    /// Take the env string as-is, including relative paths.
    AcceptAny,
}

/// Snapshot of the resolved data root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// Resolve from the process environment.
    #[must_use]
    pub fn resolve(policy: EnvPolicy, rule: PathRule) -> Self {
        Self {
            root: resolve_root(policy, rule),
        }
    }

    /// Use an explicit root (tests, CLI `--data-dir` after validation).
    #[must_use]
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { root: path.into() }
    }

    /// Resolved root path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.root
    }

    /// Join `parts` under the root.
    #[must_use]
    pub fn join<I, P>(&self, parts: I) -> PathBuf
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut out = self.root.clone();
        for part in parts {
            out.push(part);
        }
        out
    }

    /// `$DATA_DIR/etc/{name}.{ext}` (e.g. `micronet.json`).
    #[must_use]
    pub fn etc_file(&self, name: &str, ext: &str) -> PathBuf {
        self.join(["etc", &format!("{name}.{ext}")])
    }

    /// `$DATA_DIR/etc/{name}/` (e.g. microwaf directory layout).
    #[must_use]
    pub fn etc_dir(&self, name: &str) -> PathBuf {
        self.join(["etc", name])
    }

    /// `$DATA_DIR/run/{name}.sock`.
    #[must_use]
    pub fn run_socket(&self, name: &str) -> PathBuf {
        self.join(["run", &format!("{name}.sock")])
    }

    /// `$DATA_DIR/run/{name}/{name}.sock`.
    #[must_use]
    pub fn run_nested_socket(&self, name: &str) -> PathBuf {
        self.join(["run", name, &format!("{name}.sock")])
    }
}

/// Process-wide root using [`EnvPolicy::DataDir`] + [`PathRule::AbsoluteOnly`].
#[must_use]
pub fn root() -> PathBuf {
    DataDir::resolve(EnvPolicy::DataDir, PathRule::AbsoluteOnly)
        .as_path()
        .to_path_buf()
}

/// Join `parts` under [`root`].
#[must_use]
pub fn path<I, P>(parts: I) -> PathBuf
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    DataDir::from_path(root()).join(parts)
}

/// Override `DATA_DIR` for this process (absolute paths only).
pub fn set_root(path: impl AsRef<Path>) {
    let p = path.as_ref();
    if p.is_absolute() {
        std::env::set_var(ENV_DATA_DIR, p.as_os_str());
    }
}

fn resolve_root(policy: EnvPolicy, rule: PathRule) -> PathBuf {
    match policy {
        EnvPolicy::DataDir => {
            if let Some(p) = from_env(ENV_DATA_DIR, rule) {
                return p;
            }
        }
        EnvPolicy::BigfredThenDataDir => {
            if let Some(p) = from_env(ENV_BIGFRED_DATA_DIR, rule) {
                return p;
            }
            if let Some(p) = from_env(ENV_DATA_DIR, rule) {
                return p;
            }
        }
    }
    PathBuf::from(DEFAULT_ROOT)
}

fn from_env(name: &str, rule: PathRule) -> Option<PathBuf> {
    match rule {
        PathRule::AbsoluteOnly => {
            let v = std::env::var_os(name)?;
            if v.is_empty() {
                return None;
            }
            let p = PathBuf::from(v);
            if p.is_absolute() {
                Some(p)
            } else {
                None
            }
        }
        PathRule::AcceptAny => {
            let v = std::env::var(name).ok()?;
            Some(PathBuf::from(v))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_env<F: FnOnce()>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(ENV_DATA_DIR);
        std::env::remove_var(ENV_BIGFRED_DATA_DIR);
        f();
        std::env::remove_var(ENV_DATA_DIR);
        std::env::remove_var(ENV_BIGFRED_DATA_DIR);
    }

    #[test]
    fn root_defaults_to_data() {
        with_clean_env(|| {
            assert_eq!(root(), PathBuf::from(DEFAULT_ROOT));
        });
    }

    #[test]
    fn data_dir_absolute_wins() {
        with_clean_env(|| {
            let dir = std::env::temp_dir().join(format!("bigfred-shared-daemon-dd-{}", std::process::id()));
            std::env::set_var(ENV_DATA_DIR, &dir);
            assert_eq!(root(), dir);
            assert_eq!(
                path(["etc", "micronet.json"]),
                dir.join("etc/micronet.json")
            );
        });
    }

    #[test]
    fn relative_data_dir_ignored() {
        with_clean_env(|| {
            std::env::set_var(ENV_DATA_DIR, "relative/path");
            assert_eq!(root(), PathBuf::from(DEFAULT_ROOT));
        });
    }

    #[test]
    fn empty_data_dir_ignored() {
        with_clean_env(|| {
            std::env::set_var(ENV_DATA_DIR, "");
            assert_eq!(root(), PathBuf::from(DEFAULT_ROOT));
        });
    }

    #[test]
    fn bigfred_env_wins_over_data_dir() {
        with_clean_env(|| {
            std::env::set_var(ENV_DATA_DIR, "/data-from-data");
            std::env::set_var(ENV_BIGFRED_DATA_DIR, "/data-from-bf");
            let d = DataDir::resolve(EnvPolicy::BigfredThenDataDir, PathRule::AbsoluteOnly);
            assert_eq!(d.as_path(), Path::new("/data-from-bf"));
        });
    }

    #[test]
    fn accept_any_keeps_relative() {
        with_clean_env(|| {
            std::env::set_var(ENV_BIGFRED_DATA_DIR, "rel");
            let d = DataDir::resolve(EnvPolicy::BigfredThenDataDir, PathRule::AcceptAny);
            assert_eq!(d.as_path(), Path::new("rel"));
        });
    }

    #[test]
    fn identity_helpers() {
        let d = DataDir::from_path("/data");
        assert_eq!(
            d.etc_file("micronet", "json"),
            PathBuf::from("/data/etc/micronet.json")
        );
        assert_eq!(d.etc_dir("microwaf"), PathBuf::from("/data/etc/microwaf"));
        assert_eq!(
            d.run_socket("micronet"),
            PathBuf::from("/data/run/micronet.sock")
        );
        assert_eq!(
            d.run_nested_socket("microwaf"),
            PathBuf::from("/data/run/microwaf/microwaf.sock")
        );
    }

    #[test]
    fn set_root_absolute_only() {
        with_clean_env(|| {
            set_root("relative");
            assert_eq!(root(), PathBuf::from(DEFAULT_ROOT));
            let dir = std::env::temp_dir().join(format!("bigfred-shared-daemon-set-{}", std::process::id()));
            set_root(&dir);
            assert_eq!(root(), dir);
        });
    }
}
