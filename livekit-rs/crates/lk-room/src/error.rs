//! Room-layer errors.

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors raised while reading or patching SDP.
///
/// The variants mirror the `webrtc.ErrSessionDescription*` sentinels the Go
/// helpers return, because callers branch on them: a missing fingerprint is a
/// client that has not offered DTLS yet, a conflicting one is a description
/// that must be refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The description carries no `a=fingerprint`.
    #[error("session description has no fingerprint")]
    NoFingerprint,

    /// The description carries fingerprints that disagree.
    #[error("session description has conflicting fingerprints")]
    ConflictingFingerprints,

    /// The fingerprint is not `<algorithm> <value>`.
    #[error("session description has an invalid fingerprint")]
    InvalidFingerprint,

    /// The description carries no `a=ice-ufrag`.
    #[error("session description is missing ice-ufrag")]
    MissingIceUfrag,

    /// The description carries no `a=ice-pwd`.
    #[error("session description is missing ice-pwd")]
    MissingIcePwd,

    /// The description carries ice-ufrags that disagree.
    #[error("session description has conflicting ice-ufrag")]
    ConflictingIceUfrag,

    /// The description carries ice-pwds that disagree.
    #[error("session description has conflicting ice-pwd")]
    ConflictingIcePwd,

    /// A media format is not an 8-bit payload type.
    #[error("invalid payload type {0:?}")]
    InvalidPayloadType(String),

    /// A payload type in the format list has no codec.
    #[error("payload type {0} has no codec")]
    UnknownPayloadType(u8),

    /// An SDP fragment could not be read or applied.
    #[error("invalid sdp fragment: {0}")]
    InvalidFragment(&'static str),
}
