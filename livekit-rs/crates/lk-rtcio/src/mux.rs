//! The UDP mux: one socket, N PeerConnections.
//!
//! This is webrtc-rs gap #1 in its Phase-0 form. `rtc` never binds a socket and
//! the `webrtc` facade binds one set per PeerConnection
//! (`driver.rs:434 bind_transports`), so demultiplexing a single published port
//! across thousands of connections is ours to do.
//!
//! Two tables, in priority order:
//!
//! 1. **5-tuple.** Once a flow has been placed, every later datagram from that
//!    `(peer, proto)` goes straight to the same connection. This is the hot
//!    path: one hash lookup, no parsing.
//! 2. **Local ICE ufrag.** A datagram from an address we have never seen can
//!    only be an ICE connectivity check, so the mux reads the local ufrag out
//!    of the STUN `USERNAME` and binds the address to that connection.
//!
//! Anything else is counted and dropped. Silence is the wrong answer here: an
//! SFU that quietly discards datagrams looks identical to one with a routing
//! bug, so every reason is a named counter.

use std::net::SocketAddr;

use ahash::AHashMap;

use crate::datagram::Proto;
use crate::shard::PcIndex;
use crate::stun;

/// Why a datagram could not be placed on a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Unroutable {
    /// Not a known flow, and not a STUN Binding Request either, so there is no
    /// ufrag to route on. Usually a late packet from a closed connection.
    #[error("unknown flow and not a STUN binding request")]
    NotStun,
    /// A Binding Request whose `USERNAME` names a ufrag no connection has
    /// registered. Usually a client reconnecting after an ICE restart on our
    /// side, or a scan. The ufrag itself is logged rather than carried, so that
    /// the error type stays `Copy` on the hot path.
    #[error("no peer connection is registered under the offered local ufrag")]
    UnknownUfrag,
}

/// Where the mux placed a datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Matched an existing 5-tuple binding.
    Established(PcIndex),
    /// Matched a registered ufrag; the flow has just been bound to that
    /// connection and later datagrams will take the fast path.
    FirstPacket(PcIndex),
    /// Belongs to no connection.
    Unroutable(Unroutable),
}

/// Counters for the routing decisions the mux made.
///
/// These are the Phase-0 shape of the `livekit_mux_*` Prometheus families; the
/// names are settled when `lk-telemetry` lands in Phase 4.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MuxStats {
    /// Datagrams routed by 5-tuple.
    pub established: u64,
    /// Flows bound by STUN ufrag.
    pub first_packets: u64,
    /// Datagrams dropped because they were neither.
    pub unroutable: u64,
    /// Flows whose `(peer, proto)` was rebound to a different connection. A
    /// non-zero value is normal (NAT rebinding, port reuse) but a fast-growing
    /// one means two connections are fighting over an address.
    pub rebinds: u64,
}

/// Routing table for one socket.
#[derive(Debug, Default)]
pub struct UdpMux {
    by_flow: AHashMap<(SocketAddr, Proto), PcIndex>,
    by_ufrag: AHashMap<String, PcIndex>,
    /// Reverse index, so removing a connection does not scan the flow table.
    flows_of: AHashMap<PcIndex, Vec<(SocketAddr, Proto)>>,
    ufrags_of: AHashMap<PcIndex, Vec<String>>,
    stats: MuxStats,
}

impl UdpMux {
    /// An empty mux.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `ufrag` as the local ICE ufrag of connection `pc`.
    ///
    /// Called once the connection's local description exists, and again after
    /// every ICE restart, since a restart mints a new ufrag. The old one stays
    /// registered until [`UdpMux::remove_pc`]: in-flight checks against the
    /// pre-restart ufrag must still land on the same connection.
    ///
    /// # Errors
    ///
    /// [`Error::DuplicateUfrag`](crate::Error::DuplicateUfrag) if another
    /// connection already owns the ufrag. Re-registering the same ufrag on the
    /// same connection is a no-op.
    pub fn register_ufrag(&mut self, pc: PcIndex, ufrag: &str) -> crate::Result<()> {
        match self.by_ufrag.get(ufrag) {
            Some(&owner) if owner == pc => return Ok(()),
            Some(_) => return Err(crate::Error::DuplicateUfrag(ufrag.to_owned())),
            None => {}
        }
        self.by_ufrag.insert(ufrag.to_owned(), pc);
        self.ufrags_of.entry(pc).or_default().push(ufrag.to_owned());
        Ok(())
    }

    /// Drop every flow and ufrag belonging to `pc`.
    pub fn remove_pc(&mut self, pc: PcIndex) {
        for flow in self.flows_of.remove(&pc).unwrap_or_default() {
            self.by_flow.remove(&flow);
        }
        for ufrag in self.ufrags_of.remove(&pc).unwrap_or_default() {
            self.by_ufrag.remove(&ufrag);
        }
    }

    /// Route one inbound datagram.
    ///
    /// An established flow is answered from the hash table. The one exception
    /// is a STUN Binding Request, which is re-checked against the ufrag table
    /// even on a known address: a NAT that hands the same external port to a
    /// new client would otherwise pin that client's checks to the dead
    /// connection that held the port before, for as long as the entry lives.
    /// [`crate::stun::is_stun`] rejects SRTP and DTLS on their first byte, so
    /// media never reaches the parser.
    pub fn route(&mut self, peer: SocketAddr, proto: Proto, payload: &[u8]) -> Route {
        let established = self.by_flow.get(&(peer, proto)).copied();
        if let Some(pc) = established
            && !stun::is_stun(payload)
        {
            self.stats.established += 1;
            return Route::Established(pc);
        }

        let Some(ufrag) = stun::local_ufrag_of_binding_request(payload) else {
            if let Some(pc) = established {
                // STUN-shaped but not a Binding Request: a response or an
                // indication on a flow we already know. Route it, do not
                // re-bind on it.
                self.stats.established += 1;
                return Route::Established(pc);
            }
            self.stats.unroutable += 1;
            return Route::Unroutable(Unroutable::NotStun);
        };
        let Some(&pc) = self.by_ufrag.get(ufrag) else {
            if let Some(pc) = established {
                // A check for a ufrag we do not know, on a flow we do. Keep the
                // flow rather than dropping a packet we can place.
                self.stats.established += 1;
                return Route::Established(pc);
            }
            self.stats.unroutable += 1;
            tracing::debug!(peer = %peer, ufrag, "dropping stun check for an unregistered ufrag");
            return Route::Unroutable(Unroutable::UnknownUfrag);
        };

        if established == Some(pc) {
            self.stats.established += 1;
            return Route::Established(pc);
        }

        self.bind(peer, proto, pc);
        self.stats.first_packets += 1;
        Route::FirstPacket(pc)
    }

    /// Bind `(peer, proto)` to `pc`, moving it off whatever connection held it.
    fn bind(&mut self, peer: SocketAddr, proto: Proto, pc: PcIndex) {
        if let Some(previous) = self.by_flow.insert((peer, proto), pc)
            && previous != pc
        {
            self.stats.rebinds += 1;
            if let Some(flows) = self.flows_of.get_mut(&previous) {
                flows.retain(|f| *f != (peer, proto));
            }
        }
        self.flows_of.entry(pc).or_default().push((peer, proto));
    }

    /// Number of bound flows. Exposed for the shard's gauge.
    #[must_use]
    pub fn flow_count(&self) -> usize {
        self.by_flow.len()
    }

    /// Number of registered ufrags.
    #[must_use]
    pub fn ufrag_count(&self) -> usize {
        self.by_ufrag.len()
    }

    /// Routing counters since the mux was created.
    #[must_use]
    pub fn stats(&self) -> MuxStats {
        self.stats
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const ATTR_USERNAME: u16 = 0x0006;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([203, 0, 113, 7], port))
    }

    fn binding_request(username: &str) -> Vec<u8> {
        let mut attrs = Vec::new();
        attrs.extend_from_slice(&ATTR_USERNAME.to_be_bytes());
        attrs.extend_from_slice(&(username.len() as u16).to_be_bytes());
        attrs.extend_from_slice(username.as_bytes());
        attrs.resize(attrs.len().next_multiple_of(4), 0);

        let mut msg = Vec::new();
        msg.extend_from_slice(&0x0001u16.to_be_bytes());
        msg.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
        msg.extend_from_slice(&0x2112_A442u32.to_be_bytes());
        msg.extend_from_slice(&[7u8; 12]);
        msg.extend_from_slice(&attrs);
        msg
    }

    #[test]
    fn first_packet_binds_the_flow_and_later_packets_take_the_fast_path() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(3), "abcd").unwrap();

        let check = binding_request("abcd:efgh");
        assert_eq!(
            mux.route(addr(5000), Proto::Udp, &check),
            Route::FirstPacket(PcIndex(3))
        );

        // A DTLS record from the same address now routes without any parsing.
        assert_eq!(
            mux.route(addr(5000), Proto::Udp, &[22u8; 64]),
            Route::Established(PcIndex(3))
        );

        let stats = mux.stats();
        assert_eq!(stats.first_packets, 1);
        assert_eq!(stats.established, 1);
        assert_eq!(stats.unroutable, 0);
    }

    #[test]
    fn the_same_address_on_udp_and_tcp_are_separate_flows() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(1), "u1").unwrap();
        mux.register_ufrag(PcIndex(2), "u2").unwrap();

        mux.route(addr(5000), Proto::Udp, &binding_request("u1:x"));
        mux.route(addr(5000), Proto::Tcp, &binding_request("u2:x"));

        assert_eq!(
            mux.route(addr(5000), Proto::Udp, &[22u8; 8]),
            Route::Established(PcIndex(1))
        );
        assert_eq!(
            mux.route(addr(5000), Proto::Tcp, &[22u8; 8]),
            Route::Established(PcIndex(2))
        );
    }

    #[test]
    fn a_non_stun_datagram_from_an_unknown_address_is_counted_not_guessed() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(1), "abcd").unwrap();

        assert_eq!(
            mux.route(addr(9999), Proto::Udp, &[22u8; 64]),
            Route::Unroutable(Unroutable::NotStun)
        );
        assert_eq!(mux.stats().unroutable, 1);
        assert_eq!(mux.flow_count(), 0);
    }

    #[test]
    fn a_check_for_an_unregistered_ufrag_is_rejected() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(1), "abcd").unwrap();

        let route = mux.route(addr(9999), Proto::Udp, &binding_request("zzzz:x"));
        assert_eq!(route, Route::Unroutable(Unroutable::UnknownUfrag));
        assert_eq!(mux.flow_count(), 0);
    }

    #[test]
    fn two_connections_cannot_share_a_ufrag() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(1), "abcd").unwrap();
        // Idempotent for the same connection: an ICE restart may re-register.
        mux.register_ufrag(PcIndex(1), "abcd").unwrap();

        let err = mux.register_ufrag(PcIndex(2), "abcd").unwrap_err();
        assert!(matches!(err, crate::Error::DuplicateUfrag(u) if u == "abcd"));
    }

    #[test]
    fn an_ice_restart_keeps_the_old_ufrag_routable() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(1), "old").unwrap();
        mux.register_ufrag(PcIndex(1), "new").unwrap();

        // A check still in flight against the pre-restart ufrag lands on the
        // same connection rather than being dropped.
        assert_eq!(
            mux.route(addr(1), Proto::Udp, &binding_request("old:x")),
            Route::FirstPacket(PcIndex(1))
        );
        assert_eq!(
            mux.route(addr(2), Proto::Udp, &binding_request("new:x")),
            Route::FirstPacket(PcIndex(1))
        );
    }

    #[test]
    fn nat_rebinding_moves_the_flow_and_is_counted() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(1), "u1").unwrap();
        mux.register_ufrag(PcIndex(2), "u2").unwrap();

        mux.route(addr(5000), Proto::Udp, &binding_request("u1:x"));
        // The NAT hands the same external port to a different connection.
        mux.route(addr(5000), Proto::Udp, &binding_request("u2:x"));

        assert_eq!(
            mux.route(addr(5000), Proto::Udp, &[22u8; 8]),
            Route::Established(PcIndex(2))
        );
        assert_eq!(mux.stats().rebinds, 1);
        assert_eq!(mux.flow_count(), 1);
    }

    #[test]
    fn removing_a_connection_frees_its_flows_and_ufrags() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(1), "abcd").unwrap();
        mux.route(addr(5000), Proto::Udp, &binding_request("abcd:x"));
        assert_eq!(mux.flow_count(), 1);
        assert_eq!(mux.ufrag_count(), 1);

        mux.remove_pc(PcIndex(1));

        assert_eq!(mux.flow_count(), 0);
        assert_eq!(mux.ufrag_count(), 0);
        // The ufrag is now free for another connection.
        mux.register_ufrag(PcIndex(2), "abcd").unwrap();
    }

    #[test]
    fn removing_a_rebound_connection_does_not_take_the_new_owners_flow() {
        let mut mux = UdpMux::new();
        mux.register_ufrag(PcIndex(1), "u1").unwrap();
        mux.register_ufrag(PcIndex(2), "u2").unwrap();
        mux.route(addr(5000), Proto::Udp, &binding_request("u1:x"));
        mux.route(addr(5000), Proto::Udp, &binding_request("u2:x"));

        mux.remove_pc(PcIndex(1));

        assert_eq!(
            mux.route(addr(5000), Proto::Udp, &[22u8; 8]),
            Route::Established(PcIndex(2))
        );
    }
}
