//! The single-shard PeerConnection driver.
//!
//! One shard owns a set of `rtc` PeerConnections outright: `!Send` state on one
//! thread, no per-connection mutex, no cross-thread handoff on the packet path.
//! The Go server reaches the same place with a goroutine and a lock per PC; the
//! sans-IO core lets us do it with a loop and no lock at all.
//!
//! The shard performs no I/O. It takes [`Datagram`]s in through
//! [`Shard::handle_datagram`] and hands them out through
//! [`Shard::poll_transmit`], which is what lets the same code run over a real
//! socket and over [`crate::vnet`].
//!
//! # Drain discipline
//!
//! Polling every connection on every wakeup is O(connections) per packet, which
//! at LiveKit's connection counts is the whole CPU budget. Instead the shard
//! keeps a dirty queue: only a connection that was just fed a datagram, a
//! timeout or an application write can have new output, so only those are
//! drained. Timers are a heap rather than a scan for the same reason.

use std::collections::{BinaryHeap, VecDeque};
use std::net::SocketAddr;
use std::time::Instant;

use ahash::{AHashMap, AHashSet};
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::event::RTCPeerConnectionEvent;
use rtc::peer_connection::message::TaggedRTCMessage;
use rtc::sansio::Protocol;

use crate::datagram::{Datagram, Proto};
use crate::error::{Error, Result};
use crate::mux::{Route, UdpMux, Unroutable};

/// Handle to a PeerConnection inside a shard.
///
/// Indices are not reused while a connection is alive, but they are reused
/// after it is removed, so a handle held across a removal is stale. The shard
/// carries a generation alongside each slot and rejects stale handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PcIndex(pub usize);

/// Something a connection produced that the layer above the shard must see.
///
/// `rtc`'s `TaggedRTCMessage` is not `Debug`, so neither is this; the shard
/// logs the parts it can name instead.
#[non_exhaustive]
pub enum ShardEvent {
    /// A state change, track or data-channel notification from `poll_event`.
    Connection {
        /// Which connection.
        pc: PcIndex,
        /// The event.
        event: Box<RTCPeerConnectionEvent>,
    },
    /// Inbound application data from `poll_read`: RTP, RTCP or a data-channel
    /// message. In Phase 0 the caller is a stub sink; from Phase 2 this feeds
    /// `lk-sfu`'s buffer.
    Media {
        /// Which connection.
        pc: PcIndex,
        /// The message.
        message: Box<TaggedRTCMessage>,
    },
}

/// Tunables for one shard.
#[derive(Debug, Clone)]
pub struct ShardConfig {
    /// The address the shard's socket is bound to. Written into every
    /// [`Datagram`] handed to `rtc` as the local half of the transport context.
    pub local_addr: SocketAddr,
    /// How many outbound datagrams may queue before the shard stops draining
    /// connections within one turn. This bounds the burst a single wakeup can
    /// build, not the total: the remainder is drained on the next turn because
    /// the connection stays dirty.
    pub max_transmit_queue: usize,
}

impl ShardConfig {
    /// A config for `local_addr` with the default queue bound.
    #[must_use]
    pub fn new(local_addr: SocketAddr) -> Self {
        Self {
            local_addr,
            // ~1 GSO batch worth of 1200-byte datagrams per turn.
            max_transmit_queue: 1024,
        }
    }
}

/// Counters for one shard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShardStats {
    /// Datagrams accepted and fed to a connection.
    pub datagrams_in: u64,
    /// Datagrams produced by connections.
    pub datagrams_out: u64,
    /// Datagrams dropped by the mux.
    pub datagrams_unroutable: u64,
    /// Timer firings dispatched.
    pub timeouts: u64,
    /// Connections whose `handle_read` or `handle_timeout` returned an error.
    /// The connection is left in place; the layer above decides whether to
    /// tear it down.
    pub connection_errors: u64,
}

struct Slot {
    pc: RTCPeerConnection,
    /// Bumped when the slot is reused, so a stale [`PcIndex`] cannot address
    /// a different connection.
    generation: u64,
    /// Whether the connection is already in `dirty`.
    queued: bool,
    /// The deadline currently in the timer heap for this slot, if any.
    scheduled: Option<Instant>,
}

/// An entry in the timer heap. Ordered so that `BinaryHeap` pops the earliest
/// deadline first.
#[derive(PartialEq, Eq)]
struct Timer {
    at: Instant,
    pc: PcIndex,
}

impl Ord for Timer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.at.cmp(&self.at).then_with(|| other.pc.cmp(&self.pc))
    }
}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A set of PeerConnections driven on one thread over one socket.
pub struct Shard {
    config: ShardConfig,
    slots: Vec<Option<Slot>>,
    free: Vec<usize>,
    generations: Vec<u64>,
    mux: UdpMux,
    /// Connections with output that has not been drained yet.
    dirty: VecDeque<PcIndex>,
    dirty_set: AHashSet<PcIndex>,
    timers: BinaryHeap<Timer>,
    out: VecDeque<Datagram>,
    events: VecDeque<ShardEvent>,
    ufrags: AHashMap<PcIndex, Vec<String>>,
    stats: ShardStats,
}

impl Shard {
    /// An empty shard bound to `config.local_addr`.
    #[must_use]
    pub fn new(config: ShardConfig) -> Self {
        Self {
            config,
            slots: Vec::new(),
            free: Vec::new(),
            generations: Vec::new(),
            mux: UdpMux::new(),
            dirty: VecDeque::new(),
            dirty_set: AHashSet::new(),
            timers: BinaryHeap::new(),
            out: VecDeque::new(),
            events: VecDeque::new(),
            ufrags: AHashMap::new(),
            stats: ShardStats::default(),
        }
    }

    /// Take ownership of `pc` and register `local_ufrag` for first-packet
    /// routing.
    ///
    /// The ufrag is the one in the connection's *local* description. It must be
    /// registered before the remote peer starts sending checks, which in
    /// practice means right after `set_local_description`.
    ///
    /// # Errors
    ///
    /// [`Error::DuplicateUfrag`] if another connection on this shard already
    /// owns the ufrag.
    pub fn insert(&mut self, pc: RTCPeerConnection, local_ufrag: &str) -> Result<PcIndex> {
        let index = match self.free.pop() {
            Some(i) => i,
            None => {
                self.slots.push(None);
                self.generations.push(0);
                self.slots.len() - 1
            }
        };
        let Some(&generation) = self.generations.get(index) else {
            // `index` came from `free` or from a push above, so both vectors
            // cover it; this arm is unreachable in practice.
            return Err(Error::UnknownPc(index));
        };
        let handle = PcIndex(index);

        // Register before storing, so a duplicate ufrag leaves no slot behind.
        self.mux.register_ufrag(handle, local_ufrag)?;
        self.ufrags
            .entry(handle)
            .or_default()
            .push(local_ufrag.to_owned());

        if let Some(slot) = self.slots.get_mut(index) {
            *slot = Some(Slot {
                pc,
                generation,
                queued: false,
                scheduled: None,
            });
        }
        // A freshly built connection may already have gathering output queued.
        self.mark_dirty(handle);
        Ok(handle)
    }

    /// Register an additional local ufrag for `pc`, as an ICE restart mints.
    ///
    /// # Errors
    ///
    /// [`Error::DuplicateUfrag`] if another connection owns it, or
    /// [`Error::UnknownPc`] if the handle is stale.
    pub fn register_ufrag(&mut self, pc: PcIndex, local_ufrag: &str) -> Result<()> {
        self.slot(pc).ok_or(Error::UnknownPc(pc.0))?;
        self.mux.register_ufrag(pc, local_ufrag)?;
        self.ufrags
            .entry(pc)
            .or_default()
            .push(local_ufrag.to_owned());
        Ok(())
    }

    /// Remove a connection, returning it so the caller can close it.
    ///
    /// The mux forgets its flows and ufrags immediately, so datagrams still in
    /// flight from that peer become unroutable rather than landing on whichever
    /// connection later takes the slot.
    pub fn remove(&mut self, pc: PcIndex) -> Option<RTCPeerConnection> {
        let slot = self.slots.get_mut(pc.0)?.take()?;
        if let Some(generation) = self.generations.get_mut(pc.0) {
            *generation = generation.wrapping_add(1);
        }
        self.free.push(pc.0);
        self.mux.remove_pc(pc);
        self.ufrags.remove(&pc);
        self.dirty_set.remove(&pc);
        Some(slot.pc)
    }

    /// Borrow a connection, for signalling calls (`create_offer`,
    /// `set_remote_description`, `add_remote_candidate`, and application
    /// writes).
    ///
    /// The connection is marked dirty on return: any of those calls can queue
    /// output or move a deadline.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownPc`] if the handle is stale.
    pub fn with_pc<T>(
        &mut self,
        pc: PcIndex,
        f: impl FnOnce(&mut RTCPeerConnection) -> T,
    ) -> Result<T> {
        let slot =
            Self::slot_mut(&mut self.slots, &self.generations, pc).ok_or(Error::UnknownPc(pc.0))?;
        let out = f(&mut slot.pc);
        self.mark_dirty(pc);
        Ok(out)
    }

    /// Feed one inbound datagram.
    ///
    /// Routing failures are counted and returned rather than swallowed; a
    /// caller reading from a public socket will normally log and continue,
    /// since anyone can send to that port.
    ///
    /// # Errors
    ///
    /// [`Error::Unroutable`] when the datagram belongs to no connection.
    pub fn handle_datagram(&mut self, now: Instant, datagram: Datagram) -> Result<PcIndex> {
        let route = self
            .mux
            .route(datagram.peer, datagram.proto, &datagram.payload);
        let pc = match route {
            Route::Established(pc) | Route::FirstPacket(pc) => pc,
            Route::Unroutable(reason) => {
                self.stats.datagrams_unroutable += 1;
                return Err(Error::Unroutable {
                    peer: datagram.peer,
                    reason,
                });
            }
        };

        // A stale flow can outlive its connection if `remove` raced the socket
        // read; treat it as unroutable rather than indexing a dead slot.
        let Some(slot) = Self::slot_mut(&mut self.slots, &self.generations, pc) else {
            self.stats.datagrams_unroutable += 1;
            return Err(Error::Unroutable {
                peer: datagram.peer,
                reason: Unroutable::NotStun,
            });
        };

        self.stats.datagrams_in += 1;
        let peer = datagram.peer;
        if let Err(err) = slot.pc.handle_read(datagram.into_tagged(now)) {
            self.stats.connection_errors += 1;
            tracing::debug!(?pc, %peer, %err, "peer connection rejected an inbound datagram");
        }
        self.mark_dirty(pc);
        Ok(pc)
    }

    /// Fire every timer due at or before `now`.
    pub fn handle_timeout(&mut self, now: Instant) {
        while let Some(timer) = self.timers.peek() {
            if timer.at > now {
                break;
            }
            let Some(Timer { at, pc }) = self.timers.pop() else {
                break;
            };

            let Some(slot) = Self::slot_mut(&mut self.slots, &self.generations, pc) else {
                continue;
            };
            // The heap holds no back-pointers, so a deadline that has since
            // moved earlier leaves a stale entry behind. Drop it rather than
            // firing the connection twice.
            if slot.scheduled != Some(at) {
                continue;
            }
            slot.scheduled = None;

            self.stats.timeouts += 1;
            if let Err(err) = slot.pc.handle_timeout(now) {
                self.stats.connection_errors += 1;
                tracing::debug!(?pc, %err, "peer connection failed a timeout");
            }
            self.mark_dirty(pc);
        }
    }

    /// The earliest deadline across every connection, if any.
    ///
    /// Only meaningful once [`Shard::drain`] has run: draining is what puts
    /// each connection's next deadline into the heap.
    #[must_use]
    pub fn poll_timeout(&self) -> Option<Instant> {
        self.timers.peek().map(|t| t.at)
    }

    /// Drain output from every dirty connection into the transmit and event
    /// queues, and reschedule their timers.
    ///
    /// Call this once per turn, after feeding datagrams and firing timeouts and
    /// before reading [`Shard::poll_transmit`].
    pub fn drain(&mut self) {
        while let Some(pc) = self.dirty.pop_front() {
            self.dirty_set.remove(&pc);
            let Some(slot) = Self::slot_mut(&mut self.slots, &self.generations, pc) else {
                continue;
            };
            slot.queued = false;

            while let Some(event) = slot.pc.poll_event() {
                self.events.push_back(ShardEvent::Connection {
                    pc,
                    event: Box::new(event),
                });
            }
            while let Some(message) = slot.pc.poll_read() {
                self.events.push_back(ShardEvent::Media {
                    pc,
                    message: Box::new(message),
                });
            }
            while let Some(tagged) = slot.pc.poll_write() {
                self.out.push_back(Datagram::from_tagged(tagged));
                self.stats.datagrams_out += 1;
            }

            // Reschedule after draining: the drain itself can move the
            // deadline (an ICE check queued here sets its own retransmit).
            let next = slot.pc.poll_timeout();
            if slot.scheduled != next {
                slot.scheduled = next;
                if let Some(at) = next {
                    self.timers.push(Timer { at, pc });
                }
            }

            if self.out.len() >= self.config.max_transmit_queue {
                // Stop here, but leave the rest of the queue dirty so the next
                // turn picks up where this one stopped.
                break;
            }
        }
    }

    /// Take the next outbound datagram, if any.
    pub fn poll_transmit(&mut self) -> Option<Datagram> {
        self.out.pop_front()
    }

    /// Take the next event or inbound application message, if any.
    pub fn poll_event(&mut self) -> Option<ShardEvent> {
        self.events.pop_front()
    }

    /// Whether any connection still has undrained output.
    #[must_use]
    pub fn has_pending_work(&self) -> bool {
        !self.dirty.is_empty() || !self.out.is_empty() || !self.events.is_empty()
    }

    /// Number of live connections.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Whether the shard holds no connections.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The address this shard's socket is bound to.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.config.local_addr
    }

    /// Shard counters.
    #[must_use]
    pub fn stats(&self) -> ShardStats {
        self.stats
    }

    /// Mux counters.
    #[must_use]
    pub fn mux_stats(&self) -> crate::MuxStats {
        self.mux.stats()
    }

    /// The protocols this shard accepts. Phase 0 is UDP only.
    #[must_use]
    pub fn protocols(&self) -> &'static [Proto] {
        &[Proto::Udp]
    }

    /// The live slot at `pc`, or `None` if the handle is stale.
    ///
    /// Every read of a slot goes through this or its `_mut` twin: a `PcIndex`
    /// is reused after its connection is removed, so a lookup that skips the
    /// generation check can address a different connection entirely.
    fn slot(&self, pc: PcIndex) -> Option<&Slot> {
        let generation = *self.generations.get(pc.0)?;
        self.slots
            .get(pc.0)?
            .as_ref()
            .filter(|s| s.generation == generation)
    }

    /// The live slot at `pc`, mutably.
    ///
    /// Takes the two fields rather than `&mut self` so that a caller can hold
    /// the slot while it also touches `stats` and `events`: those are disjoint
    /// fields, and the borrow checker can only see that if the signature says
    /// so.
    fn slot_mut<'a>(
        slots: &'a mut [Option<Slot>],
        generations: &[u64],
        pc: PcIndex,
    ) -> Option<&'a mut Slot> {
        let generation = *generations.get(pc.0)?;
        slots
            .get_mut(pc.0)?
            .as_mut()
            .filter(|s| s.generation == generation)
    }

    fn mark_dirty(&mut self, pc: PcIndex) {
        if let Some(slot) = self.slots.get_mut(pc.0).and_then(Option::as_mut)
            && !slot.queued
        {
            slot.queued = true;
            self.dirty.push_back(pc);
            self.dirty_set.insert(pc);
        }
    }
}

impl std::fmt::Debug for Shard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shard")
            .field("local_addr", &self.config.local_addr)
            .field("connections", &self.len())
            .field("dirty", &self.dirty.len())
            .field("pending_transmits", &self.out.len())
            .field("stats", &self.stats)
            .finish()
    }
}
