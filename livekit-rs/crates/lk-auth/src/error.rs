//! Authentication errors.

use std::path::PathBuf;

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong minting or verifying a token.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `ErrKeysMissing`: no API key or secret.
    #[error("api key and secret must be set")]
    KeysMissing,

    /// The token names an API key the server does not know.
    #[error("unknown api key {0:?}")]
    UnknownApiKey(String),

    /// The token is not three base64url segments carrying JSON.
    #[error("malformed token")]
    Malformed,

    /// The token header names an algorithm other than HS256.
    #[error("unsupported signing algorithm {0}, only HS256 is accepted")]
    UnsupportedAlgorithm(String),

    /// `ErrSensitiveCredentials`.
    #[error("room configuration should not contain sensitive credentials")]
    SensitiveCredentials,

    /// A key file could not be read or parsed.
    #[error("key file: {0}")]
    KeyFile(String),

    /// A key file is readable by others.
    #[error("key file {0}: others permissions must be set to 0")]
    KeyFilePermissions(PathBuf),

    /// Signing or verification failed.
    #[error(transparent)]
    Jwt(#[from] jsonwebtoken::errors::Error),
}
