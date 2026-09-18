//! Configuration errors.
//!
//! The variants mirror the sentinel errors in `pkg/config/config.go`, because
//! the server's start-up behaviour branches on them: a permissions error on a
//! key file is fatal, an empty TURN secret file is fatal, and an unknown YAML
//! key is fatal only in strict mode.

use std::path::PathBuf;

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong loading a config.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The YAML document did not parse.
    #[error("could not parse config: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Strict mode found keys the schema does not declare.
    #[error("unknown config field(s): {}", .0.join(", "))]
    UnknownFields(Vec<String>),

    /// A generated CLI flag could not be applied.
    #[error("{0}")]
    Cli(String),

    /// A file named by the config could not be read.
    #[error("{path}: {source}")]
    Io {
        /// The file the server tried to read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// `ErrKeyFileIncorrectPermission`: the key file is readable by others.
    #[error("key file others permissions must be set to 0")]
    KeyFilePermissions,

    /// `ErrTURNSecretFileIncorrectPermission`.
    #[error("turn secret file others permissions must be set to 0")]
    TurnSecretFilePermissions,

    /// `ErrKeysNotSet`.
    #[error("one of key-file or keys must be provided")]
    KeysNotSet,

    /// `ErrTURNSecretEmpty`.
    #[error("turn server {host:?} secret file {path:?}: turn secret is empty")]
    TurnSecretEmpty {
        /// The TURN server host the secret belongs to.
        host: String,
        /// The secret file that was empty.
        path: PathBuf,
    },

    /// `ErrTURNServerNoCredentials`.
    #[error(
        "turn server {0:?} has no usable credentials: set a non-empty secret/secret_file for \
         dynamic auth, or username and credential for static auth"
    )]
    TurnServerNoCredentials(String),

    /// A value was syntactically valid YAML but semantically rejected.
    #[error("{0}")]
    Invalid(String),
}
