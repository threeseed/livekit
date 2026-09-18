//! Driving `rtc` PeerConnections over the virtual network.
//!
//! [`super::Vnet`] moves bytes; this drives the connections that produce them.
//! Keeping the two apart means a test that only needs impairments does not pay
//! for a WebRTC stack, and the exit-gate scenario does not have to reimplement
//! an event loop.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::event::RTCPeerConnectionEvent;
use rtc::peer_connection::state::RTCPeerConnectionState;
use rtc::sansio::Protocol;

use crate::datagram::Datagram;
use crate::shard::{PcIndex, Shard, ShardEvent};

use super::net::{EndpointId, Vnet};

/// A bare `rtc` PeerConnection attached to a vnet endpoint.
///
/// This is the *client* side of a simulation: the server side is a [`Shard`],
/// which is what the SFU actually runs. Keeping them asymmetric is the point
/// of the exit gate: many single-connection peers against one multiplexed
/// shard is the shape production has.
pub struct VnetPeer {
    /// The endpoint this peer sends from.
    pub endpoint: EndpointId,
    /// The peer's connection.
    pub pc: RTCPeerConnection,
    /// The address peers reach it at, after any NAT translation.
    pub addr: SocketAddr,
    state: RTCPeerConnectionState,
    failed: bool,
}

impl VnetPeer {
    /// Attach `pc` to `endpoint`, which is reachable at `addr`.
    #[must_use]
    pub fn new(endpoint: EndpointId, addr: SocketAddr, pc: RTCPeerConnection) -> Self {
        Self {
            endpoint,
            pc,
            addr,
            state: RTCPeerConnectionState::New,
            failed: false,
        }
    }

    /// The last connection state this peer reported.
    #[must_use]
    pub fn state(&self) -> RTCPeerConnectionState {
        self.state
    }

    /// Whether the peer reached `Connected`, meaning ICE, DTLS and SRTP keying
    /// all completed.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.state == RTCPeerConnectionState::Connected
    }

    /// Whether the peer reported `Failed` or `Closed`.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.failed
    }
}

/// Counters for one simulation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SimulationStats {
    /// Iterations of the drive loop.
    pub steps: u64,
    /// Datagrams the shard handed to the network.
    pub shard_transmits: u64,
    /// Datagrams the peers handed to the network.
    pub peer_transmits: u64,
    /// Datagrams the network refused to carry.
    pub undeliverable: u64,
    /// Datagrams the shard's mux could not place.
    pub shard_unroutable: u64,
}

/// One SFU shard and a set of peers, driven together on virtual time.
pub struct Simulation {
    /// The virtual network.
    pub net: Vnet,
    /// The shard under test.
    pub shard: Shard,
    /// The endpoint the shard sends from.
    pub shard_endpoint: EndpointId,
    /// The peers.
    pub peers: Vec<VnetPeer>,
    shard_states: Vec<RTCPeerConnectionState>,
    stats: SimulationStats,
}

impl Simulation {
    /// Build a simulation around `shard`, sending from `shard_endpoint`.
    #[must_use]
    pub fn new(net: Vnet, shard: Shard, shard_endpoint: EndpointId) -> Self {
        Self {
            net,
            shard,
            shard_endpoint,
            peers: Vec::new(),
            shard_states: Vec::new(),
            stats: SimulationStats::default(),
        }
    }

    /// Add a peer.
    pub fn push_peer(&mut self, peer: VnetPeer) {
        self.peers.push(peer);
    }

    /// Counters for the simulation so far.
    #[must_use]
    pub fn stats(&self) -> SimulationStats {
        self.stats
    }

    /// Run until `done` returns true or `budget` of virtual time elapses.
    ///
    /// Returns whether `done` was satisfied. The budget is virtual, so a
    /// 30-second budget costs only the CPU of the packets actually exchanged.
    pub fn run_until(&mut self, budget: Duration, mut done: impl FnMut(&Self) -> bool) -> bool {
        let deadline = self.net.now() + budget;
        loop {
            self.step();
            if done(self) {
                return true;
            }
            if self.net.now() >= deadline {
                return false;
            }
            // Nothing in flight and no timer pending means no further input can
            // arrive: the run is wedged and burning virtual time would not
            // change that.
            if !self.advance() {
                return done(self);
            }
        }
    }

    /// Run until every peer is connected, or `budget` elapses.
    pub fn run_until_all_connected(&mut self, budget: Duration) -> bool {
        self.run_until(budget, |sim| sim.peers.iter().all(VnetPeer::is_connected))
    }

    /// One pass: deliver what is due, then drain every connection into the
    /// network.
    pub fn step(&mut self) {
        self.stats.steps += 1;
        let now = self.net.now();

        // 1. Deliver. Datagrams for the shard go through the mux; a datagram
        //    for a peer goes straight in, since a peer has exactly one
        //    connection.
        let due = self.net.advance_to(now);
        for (endpoint, datagram) in due {
            if endpoint == self.shard_endpoint {
                if self.shard.handle_datagram(now, datagram).is_err() {
                    self.stats.shard_unroutable += 1;
                }
            } else if let Some(peer) = self.peers.iter_mut().find(|p| p.endpoint == endpoint) {
                // Present the datagram as arriving at the address the peer
                // advertised, not at its private one. A peer behind a NAT
                // advertises its mapped address as its candidate, and `rtc`
                // matches an inbound datagram against the candidate it is
                // checking: handing it the private address would make every
                // reply look like it arrived on an unknown local interface.
                let mut datagram = datagram;
                datagram.local = peer.addr;
                let _ = peer.pc.handle_read(datagram.into_tagged(now));
            }
        }

        // 2. Fire whatever timers are due.
        self.shard.handle_timeout(now);
        for peer in &mut self.peers {
            if peer.pc.poll_timeout().is_some_and(|at| at <= now) {
                let _ = peer.pc.handle_timeout(now);
            }
        }

        // 3. Drain. The shard keeps draining while it still has dirty
        //    connections: one pass is capped by `max_transmit_queue`.
        loop {
            self.shard.drain();
            let mut drained = false;
            while let Some(datagram) = self.shard.poll_transmit() {
                drained = true;
                self.stats.shard_transmits += 1;
                if self
                    .net
                    .send(self.shard_endpoint, datagram.peer, datagram.payload)
                    .is_none()
                {
                    self.stats.undeliverable += 1;
                }
            }
            while let Some(event) = self.shard.poll_event() {
                // Phase 0 has no media sink. Connection-state events are kept
                // so a stalled handshake can be diagnosed from both sides;
                // everything else is dropped so the queue does not grow.
                if let ShardEvent::Connection { pc, event } = event
                    && let RTCPeerConnectionEvent::OnConnectionStateChangeEvent(state) = *event
                {
                    if self.shard_states.len() <= pc.0 {
                        self.shard_states
                            .resize(pc.0 + 1, RTCPeerConnectionState::New);
                    }
                    if let Some(slot) = self.shard_states.get_mut(pc.0) {
                        *slot = state;
                    }
                }
            }
            let _ = drained;
            if !self.shard.has_pending_work() {
                break;
            }
        }

        for peer in &mut self.peers {
            while let Some(event) = peer.pc.poll_event() {
                if let RTCPeerConnectionEvent::OnConnectionStateChangeEvent(state) = event {
                    peer.state = state;
                    peer.failed |= matches!(
                        state,
                        RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
                    );
                }
            }
            while peer.pc.poll_read().is_some() {}
            while let Some(tagged) = peer.pc.poll_write() {
                let datagram = Datagram::from_tagged(tagged);
                self.stats.peer_transmits += 1;
                if self
                    .net
                    .send(peer.endpoint, datagram.peer, datagram.payload)
                    .is_none()
                {
                    self.stats.undeliverable += 1;
                }
            }
        }
    }

    /// Move virtual time to the next thing that can happen.
    ///
    /// Returns false when nothing can: no datagram in flight and no timer
    /// armed anywhere.
    pub fn advance(&mut self) -> bool {
        let mut next = self.net.next_delivery();
        let consider = |next: &mut Option<Instant>, at: Option<Instant>| {
            if let Some(at) = at {
                *next = Some(next.map_or(at, |current: Instant| current.min(at)));
            }
        };
        consider(&mut next, self.shard.poll_timeout());
        for peer in &mut self.peers {
            consider(&mut next, peer.pc.poll_timeout());
        }

        match next {
            Some(at) => {
                // A deadline already in the past still counts as progress: the
                // next step fires it. Nudge the clock forward so a stuck timer
                // cannot spin forever at the same instant.
                let at = at.max(self.net.now() + Duration::from_micros(1));
                // `skip_to`, not `advance_to`: draining here would throw the
                // datagrams away, because `step` is what delivers them.
                self.net.skip_to(at);
                true
            }
            None => false,
        }
    }

    /// How many peers have reached `Connected`.
    #[must_use]
    pub fn connected_count(&self) -> usize {
        self.peers.iter().filter(|p| p.is_connected()).count()
    }

    /// The shard-side handle of the connection facing `peer_index`, by
    /// registration order.
    #[must_use]
    pub fn shard_pc(&self, peer_index: usize) -> PcIndex {
        PcIndex(peer_index)
    }

    /// The shard-side connection state of each connection, by registration
    /// order.
    ///
    /// A handshake that stalls is almost never stalled on both sides, so the
    /// two state vectors together say which side stopped talking.
    #[must_use]
    pub fn shard_states(&self) -> &[RTCPeerConnectionState] {
        &self.shard_states
    }

    /// The peer-side connection state of each peer.
    #[must_use]
    pub fn peer_states(&self) -> Vec<RTCPeerConnectionState> {
        self.peers.iter().map(VnetPeer::state).collect()
    }
}
