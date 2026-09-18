//! Error type for the I/O layer.

use std::net::SocketAddr;

/// Errors raised by the mux, the shard driver and the transports.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A PeerConnection index was used after the connection was removed.
    #[error("no peer connection at index {0}")]
    UnknownPc(usize),

    /// Two PeerConnections tried to register the same local ICE ufrag. ICE
    /// ufrags are the mux's only handle on a first packet, so a collision is a
    /// routing ambiguity, not a warning.
    #[error("local ice ufrag {0:?} is already registered on this mux")]
    DuplicateUfrag(String),

    /// A datagram arrived that belongs to no PeerConnection.
    #[error("datagram from {peer} is not routable: {reason}")]
    Unroutable {
        /// Source address of the datagram.
        peer: SocketAddr,
        /// Why the mux could not place it.
        reason: crate::mux::Unroutable,
    },

    /// The `rtc` core rejected a call.
    #[error("rtc core: {0}")]
    Rtc(#[from] rtc::shared::error::Error),

    /// A socket operation failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
