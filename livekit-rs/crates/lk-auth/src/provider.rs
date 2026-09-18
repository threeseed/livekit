//! API key providers.
//!
//! Ports `protocol/auth/provider.go`, plus the start-up check `pkg/config`
//! performs on a key file: a file others can read is refused, because an API
//! secret in it mints tokens for every room on the deployment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Looks an API secret up by its key.
pub trait KeyProvider: Send + Sync {
    /// The secret for `key`, or `None` when the key is unknown.
    fn secret(&self, key: &str) -> Option<String>;

    /// How many keys are configured. The server refuses to start with none.
    fn num_keys(&self) -> usize;
}

/// Keys read from a YAML `key: secret` map, as `key_file` holds.
#[derive(Clone, Debug, Default)]
pub struct FileBasedKeyProvider {
    keys: BTreeMap<String, String>,
}

impl FileBasedKeyProvider {
    /// Builds a provider from an in-memory map, such as `config.keys`.
    #[must_use]
    pub fn from_map(keys: BTreeMap<String, String>) -> Self {
        Self { keys }
    }

    /// Parses a YAML `key: secret` document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyFile`] when the document does not parse.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let keys: BTreeMap<String, String> =
            serde_yaml::from_str(yaml).map_err(|err| Error::KeyFile(err.to_string()))?;
        Ok(Self { keys })
    }

    /// Reads a key file, refusing one that others can access.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyFilePermissions`] when the file's others bits are
    /// not zero, and [`Error::KeyFile`] when it cannot be read or parsed.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let metadata = std::fs::metadata(path).map_err(|err| Error::KeyFile(err.to_string()))?;
        if others_can_access(&metadata) {
            return Err(Error::KeyFilePermissions(path.to_path_buf()));
        }
        let contents =
            std::fs::read_to_string(path).map_err(|err| Error::KeyFile(err.to_string()))?;
        Self::from_yaml(&contents)
    }

    /// The configured keys.
    #[must_use]
    pub fn keys(&self) -> &BTreeMap<String, String> {
        &self.keys
    }
}

impl KeyProvider for FileBasedKeyProvider {
    fn secret(&self, key: &str) -> Option<String> {
        self.keys.get(key).cloned()
    }

    fn num_keys(&self) -> usize {
        self.keys.len()
    }
}

/// A single key and secret, as `--keys` or a development config gives.
#[derive(Clone, Debug, Default)]
pub struct SimpleKeyProvider {
    api_key: String,
    api_secret: String,
}

impl SimpleKeyProvider {
    /// A provider holding one pair.
    #[must_use]
    pub fn new(api_key: impl Into<String>, api_secret: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            api_secret: api_secret.into(),
        }
    }
}

impl KeyProvider for SimpleKeyProvider {
    fn secret(&self, key: &str) -> Option<String> {
        (key == self.api_key).then(|| self.api_secret.clone())
    }

    fn num_keys(&self) -> usize {
        1
    }
}

/// The path of a key file that was refused, for the error message.
pub type RefusedKeyFile = PathBuf;

#[cfg(unix)]
fn others_can_access(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o007 != 0
}

#[cfg(not(unix))]
fn others_can_access(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_simple_provider_answers_for_one_key_only() {
        let provider = SimpleKeyProvider::new("devkey", "secret");
        assert_eq!(provider.secret("devkey"), Some("secret".to_owned()));
        assert_eq!(provider.secret("other"), None);
        assert_eq!(provider.num_keys(), 1);
    }

    #[test]
    fn a_key_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.yaml");
        std::fs::write(&path, "devkey: secret\n").unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let provider = FileBasedKeyProvider::from_file(&path).unwrap();
        assert_eq!(provider.num_keys(), 1);
        assert_eq!(provider.secret("devkey"), Some("secret".to_owned()));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            FileBasedKeyProvider::from_file(&path),
            Err(Error::KeyFilePermissions(_))
        ));
    }
}
