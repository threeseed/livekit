//! The unit the mux and the transports exchange.

use std::net::SocketAddr;
use std::time::Instant;

use bytes::BytesMut;
use rtc::shared::{TaggedBytesMut, TransportContext, TransportProtocol};

/// Transport protocol a datagram arrived on or leaves by.
///
/// `rtc`'s own [`TransportProtocol`] is not `Hash`, and the mux keys its flow
/// table on the protocol, so the mux carries its own two-variant copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Proto {
    /// Plain UDP, the single published `rtc.udp_port`.
    Udp,
    /// ICE-TCP passive, RFC 4571 framed. Reserved for the Phase 1 TCP mux.
    Tcp,
}

impl From<Proto> for TransportProtocol {
    fn from(value: Proto) -> Self {
        match value {
            Proto::Udp => TransportProtocol::UDP,
            Proto::Tcp => TransportProtocol::TCP,
        }
    }
}

impl From<TransportProtocol> for Proto {
    fn from(value: TransportProtocol) -> Self {
        match value {
            TransportProtocol::TCP => Proto::Tcp,
            _ => Proto::Udp,
        }
    }
}

/// One datagram, in either direction.
#[derive(Debug, Clone)]
pub struct Datagram {
    /// The address on the far side: source when inbound, destination when
    /// outbound.
    pub peer: SocketAddr,
    /// The address on our side. With a single published port this is constant,
    /// but it is carried explicitly because `rtc` wants it in the
    /// [`TransportContext`] and the sharded transport will not have one.
    pub local: SocketAddr,
    /// Transport the datagram belongs to.
    pub proto: Proto,
    /// Payload.
    pub payload: BytesMut,
}

impl Datagram {
    /// Build a UDP datagram.
    pub fn udp(local: SocketAddr, peer: SocketAddr, payload: impl Into<BytesMut>) -> Self {
        Self {
            peer,
            local,
            proto: Proto::Udp,
            payload: payload.into(),
        }
    }

    /// Convert into the form `rtc::sansio::Protocol::handle_read` takes.
    pub fn into_tagged(self, now: Instant) -> TaggedBytesMut {
        TaggedBytesMut {
            now,
            transport: TransportContext {
                local_addr: self.local,
                peer_addr: self.peer,
                ecn: None,
                transport_protocol: self.proto.into(),
            },
            message: self.payload,
        }
    }

    /// Convert an outbound message produced by `poll_write`.
    pub fn from_tagged(tagged: TaggedBytesMut) -> Self {
        Self {
            peer: tagged.transport.peer_addr,
            local: tagged.transport.local_addr,
            proto: tagged.transport.transport_protocol.into(),
            payload: tagged.message,
        }
    }
}
