//! JSON file loader (`$DATA_DIR/etc/{name}.json` style).

use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::{ConfigError, Load};

/// Load `T` from a JSON file; optionally seed defaults when missing.
pub struct JsonFile<T> {
    path: PathBuf,
    seed: Seed,
    _ty: PhantomData<fn() -> T>,
}

enum Seed {
    ErrorIfMissing,
    DefaultInMemory,
    CreateFile,
}

impl<T> JsonFile<T> {
    /// Missing file is an error.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            seed: Seed::ErrorIfMissing,
            _ty: PhantomData,
        }
    }

    /// Missing file → `T::default()` in memory, do not write.
    #[must_use]
    pub fn missing_defaults(mut self) -> Self {
        self.seed = Seed::DefaultInMemory;
        self
    }

    /// Missing file → write pretty `T::default()` then return it.
    #[must_use]
    pub fn create_default(mut self) -> Self {
        self.seed = Seed::CreateFile;
        self
    }

    /// Path being loaded.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<T> Load for JsonFile<T>
where
    T: DeserializeOwned + Serialize + Default + Send + Sync,
{
    type Value = T;

    fn load(&self) -> Result<T, ConfigError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match self.seed {
                Seed::ErrorIfMissing => Err(ConfigError::io_at(&self.path, e)),
                Seed::DefaultInMemory => Ok(T::default()),
                Seed::CreateFile => {
                    let cfg = T::default();
                    if let Some(parent) = self.path.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|err| ConfigError::io_at(parent, err))?;
                    }
                    let body = serde_json::to_string_pretty(&cfg)?;
                    fs::write(&self.path, body + "\n")
                        .map_err(|err| ConfigError::io_at(&self.path, err))?;
                    Ok(cfg)
                }
            },
            Err(e) => Err(ConfigError::io_at(&self.path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::config::Load;

    #[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Sample {
        host_name: String,
        count: u32,
    }

    #[test]
    fn load_or_create_writes_pretty_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.json");
        let src = JsonFile::<Sample>::new(&path).create_default();
        let v = src.load().unwrap();
        assert_eq!(v, Sample::default());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("hostName"));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn missing_defaults_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let src = JsonFile::<Sample>::new(&path).missing_defaults();
        let v = src.load().unwrap();
        assert_eq!(v, Sample::default());
        assert!(!path.exists());
    }

    #[test]
    fn invalid_json_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        fs::write(&path, "{").unwrap();
        let src = JsonFile::<Sample>::new(&path);
        assert!(src.load().is_err());
    }

    #[test]
    fn existing_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.json");
        fs::write(&path, r#"{"hostName":"hub","count":2}"#).unwrap();
        let src = JsonFile::<Sample>::new(&path);
        assert_eq!(
            src.load().unwrap(),
            Sample {
                host_name: "hub".into(),
                count: 2,
            }
        );
    }
}
