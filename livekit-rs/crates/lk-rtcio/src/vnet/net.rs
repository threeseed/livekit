//! The virtual network itself.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use ahash::AHashMap;
use bytes::BytesMut;
use rand::{Rng, SeedableRng, rngs::SmallRng};

use crate::datagram::{Datagram, Proto};

use super::link::LinkConfig;
use super::nat::{Nat, NatDrop, NatKind};

/// Handle to an endpoint on the virtual network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EndpointId(pub usize);

/// Handle to a NAT on the virtual network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NatId(pub usize);

/// Why a datagram never arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropReason {
    /// The link's random loss model dropped it.
    RandomLoss,
    /// The link's burst-loss model dropped it.
    BurstLoss,
    /// The link's queue was already deeper than `max_queue_delay`.
    QueueFull,
    /// The destination address belongs to no endpoint and no NAT.
    NoRoute,
    /// A NAT refused it.
    Nat(NatDrop),
}

/// Counters for one simulation run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VnetStats {
    /// Datagrams handed to [`Vnet::send`], whatever became of them. `offered`
    /// always equals `sent` plus every drop counter.
    pub offered: u64,
    /// Datagrams that entered flight, having survived every loss and queue
    /// model.
    pub sent: u64,
    /// Datagrams delivered.
    pub delivered: u64,
    /// Datagrams dropped by a loss model.
    pub dropped_loss: u64,
    /// Datagrams tail-dropped by a full queue.
    pub dropped_queue: u64,
    /// Datagrams dropped for want of a route or by a NAT.
    pub dropped_unroutable: u64,
    /// Datagrams that were deliberately delayed out of order.
    pub reordered: u64,
    /// Total bytes that entered flight.
    pub bytes_sent: u64,
}

struct Endpoint {
    addr: SocketAddr,
    behind: Option<NatId>,
}

/// Per-direction queue state for a link.
#[derive(Default)]
struct LinkState {
    /// When the link finishes serialising everything already queued.
    busy_until: Option<Instant>,
    /// Whether the burst-loss model is currently in its lossy state.
    in_burst: bool,
}

struct InFlight {
    at: Instant,
    seq: u64,
    to: EndpointId,
    from_addr: SocketAddr,
    to_addr: SocketAddr,
    proto: Proto,
    payload: BytesMut,
}

impl PartialEq for InFlight {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.seq == other.seq
    }
}
impl Eq for InFlight {}
impl Ord for InFlight {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Equal delivery times keep send order, which is what makes a run
        // reproducible from its seed.
        self.at
            .cmp(&other.at)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}
impl PartialOrd for InFlight {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// An in-memory network of endpoints driven by a virtual clock.
///
/// This is webrtc-rs gap #16: `rtc`'s `MockRuntime` has no loss, latency or NAT
/// modelling and no TCP, and the Go server's two pion-vnet suites have no Rust
/// equivalent. Without it, RTX repair, TWCC timing, BWE convergence, allocator
/// behaviour under loss and mid-stream migration can only be tested against a
/// real network, which is not reproducible.
///
/// Time never advances on its own: [`Vnet::advance_to`] is the only way it
/// moves, so a run takes as long as its CPU work and not as long as its
/// simulated seconds.
///
/// # Reproducibility
///
/// Every random decision comes from one seeded stream. A run is a pure function
/// of its seed and the order in which its endpoints send, so a failure found in
/// CI reproduces from the seed alone. See [`super::seed_from_env`].
pub struct Vnet {
    seed: u64,
    rng: SmallRng,
    now: Instant,
    endpoints: Vec<Endpoint>,
    by_addr: AHashMap<SocketAddr, EndpointId>,
    nats: Vec<Nat>,
    default_link: LinkConfig,
    link_configs: AHashMap<(EndpointId, EndpointId), LinkConfig>,
    link_states: AHashMap<(EndpointId, EndpointId), LinkState>,
    in_flight: BinaryHeap<Reverse<InFlight>>,
    seq: u64,
    stats: VnetStats,
    drops: Vec<(SocketAddr, SocketAddr, DropReason)>,
}

impl Vnet {
    /// A network seeded with `seed`, starting at `Instant::now()`, where every
    /// unconfigured link is perfect.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            rng: SmallRng::seed_from_u64(seed),
            now: Instant::now(),
            endpoints: Vec::new(),
            by_addr: AHashMap::new(),
            nats: Vec::new(),
            default_link: LinkConfig::perfect(),
            link_configs: AHashMap::new(),
            link_states: AHashMap::new(),
            in_flight: BinaryHeap::new(),
            seq: 0,
            stats: VnetStats::default(),
            drops: Vec::new(),
        }
    }

    /// The seed this network was built with. Print it when a test fails.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The current virtual time.
    #[must_use]
    pub fn now(&self) -> Instant {
        self.now
    }

    /// Apply `config` to every link that has no explicit configuration.
    ///
    /// Links already configured through [`Vnet::set_link`] are left alone.
    pub fn set_default_link(&mut self, config: LinkConfig) {
        self.default_link = config;
    }

    /// Add an endpoint at `addr`.
    ///
    /// # Panics
    ///
    /// If `addr` is already taken. A duplicate address in a simulation is a
    /// test bug, not a runtime condition.
    #[allow(clippy::panic)]
    pub fn add_endpoint(&mut self, addr: SocketAddr) -> EndpointId {
        assert!(
            !self.by_addr.contains_key(&addr),
            "vnet address {addr} is already in use"
        );
        let id = EndpointId(self.endpoints.len());
        self.endpoints.push(Endpoint { addr, behind: None });
        self.by_addr.insert(addr, id);
        id
    }

    /// Add a NAT of `kind` with external address `external_ip`.
    pub fn add_nat(&mut self, kind: NatKind, external_ip: IpAddr) -> NatId {
        let id = NatId(self.nats.len());
        self.nats.push(Nat::new(kind, external_ip));
        id
    }

    /// Put `endpoint` behind `nat`.
    ///
    /// Its datagrams are then seen with a translated source address, and
    /// whether a peer can reach it unprompted depends on the NAT's kind.
    ///
    /// # Panics
    ///
    /// If either handle is unknown.
    #[allow(clippy::panic)]
    pub fn put_behind_nat(&mut self, endpoint: EndpointId, nat: NatId) {
        assert!(nat.0 < self.nats.len(), "unknown nat {nat:?}");
        let ep = self
            .endpoints
            .get_mut(endpoint.0)
            .unwrap_or_else(|| panic!("unknown endpoint {endpoint:?}"));
        ep.behind = Some(nat);
    }

    /// Configure the link from `from` to `to`. Directional: call it twice for
    /// a symmetric path.
    pub fn set_link(&mut self, from: EndpointId, to: EndpointId, config: LinkConfig) {
        self.link_configs.insert((from, to), config);
    }

    /// Configure both directions of the path between `a` and `b`.
    pub fn set_link_both_ways(&mut self, a: EndpointId, b: EndpointId, config: LinkConfig) {
        self.link_configs.insert((a, b), config.clone());
        self.link_configs.insert((b, a), config);
    }

    /// The address an endpoint is bound to, before any NAT translation.
    #[must_use]
    pub fn addr_of(&self, endpoint: EndpointId) -> Option<SocketAddr> {
        self.endpoints.get(endpoint.0).map(|e| e.addr)
    }

    /// The address peers see this endpoint as, once it has sent to `peer`.
    ///
    /// For an endpoint not behind a NAT this is just its bound address. For one
    /// behind a NAT it is the mapped address, allocated on demand, which is
    /// what a test must advertise as that endpoint's host candidate.
    pub fn observed_addr(&mut self, endpoint: EndpointId, peer: SocketAddr) -> Option<SocketAddr> {
        let ep = self.endpoints.get(endpoint.0)?;
        let addr = ep.addr;
        match ep.behind {
            None => Some(addr),
            Some(nat) => self
                .nats
                .get_mut(nat.0)
                .map(|n| n.translate_outbound(addr, peer)),
        }
    }

    /// Send a datagram from `from` to `to_addr`.
    ///
    /// Returns `None` when the datagram was dropped, with the reason recorded
    /// in [`Vnet::take_drops`].
    pub fn send(&mut self, from: EndpointId, to_addr: SocketAddr, payload: BytesMut) -> Option<()> {
        self.send_with_proto(from, to_addr, Proto::Udp, payload)
    }

    /// Send a datagram on a named transport.
    pub fn send_with_proto(
        &mut self,
        from: EndpointId,
        to_addr: SocketAddr,
        proto: Proto,
        payload: BytesMut,
    ) -> Option<()> {
        let from_ep = self.endpoints.get(from.0)?;
        let from_internal = from_ep.addr;
        let from_nat = from_ep.behind;

        // 1. Source translation, which also creates the inbound mapping the
        //    reply will need.
        let source = match from_nat {
            None => from_internal,
            Some(nat) => self
                .nats
                .get_mut(nat.0)?
                .translate_outbound(from_internal, to_addr),
        };

        // 2. Destination resolution: either a bound address, or a NAT external
        //    address that maps back to one.
        let (to, dest_internal) = match self.resolve(to_addr, source) {
            Ok(pair) => pair,
            Err(reason) => {
                self.stats.dropped_unroutable += 1;
                self.drops.push((source, to_addr, reason));
                return None;
            }
        };

        self.stats.offered += 1;

        let config = self
            .link_configs
            .get(&(from, to))
            .unwrap_or(&self.default_link)
            .clone();
        let state = self.link_states.entry((from, to)).or_default();

        // 3. Loss. Burst state advances on every packet whether or not the
        //    independent model already dropped it, so the two models compose
        //    without the burst chain stalling.
        let in_burst = if let Some(burst) = config.burst_loss {
            let roll: f64 = self.rng.random();
            state.in_burst = if state.in_burst {
                roll >= burst.exit
            } else {
                roll < burst.enter
            };
            state.in_burst
        } else {
            false
        };
        if in_burst {
            let roll: f64 = self.rng.random();
            if let Some(burst) = config.burst_loss
                && roll < burst.loss_in_burst
            {
                self.stats.dropped_loss += 1;
                self.drops.push((source, to_addr, DropReason::BurstLoss));
                return None;
            }
        }
        if config.loss > 0.0 {
            let roll: f64 = self.rng.random();
            if roll < config.loss {
                self.stats.dropped_loss += 1;
                self.drops.push((source, to_addr, DropReason::RandomLoss));
                return None;
            }
        }

        // 4. Bandwidth: a serialisation queue rather than a token bucket, so a
        //    sender that overshoots the cap sees queueing delay grow before it
        //    sees loss. That is the signal a delay-based estimator needs.
        let serialise = config.serialisation_delay(payload.len());
        let now = self.now;
        let queue_start = state.busy_until.unwrap_or(now).max(now);
        if queue_start.saturating_duration_since(now) > config.max_queue_delay {
            self.stats.dropped_queue += 1;
            self.drops.push((source, to_addr, DropReason::QueueFull));
            return None;
        }
        let egress = queue_start + serialise;
        state.busy_until = Some(egress);

        // 5. Propagation, jitter and reorder.
        let mut at = egress + config.delay;
        if !config.jitter.is_zero() {
            let nanos = self
                .rng
                .random_range(0..config.jitter.as_nanos().max(1) as u64);
            at += Duration::from_nanos(nanos);
        }
        let mut reordered = false;
        if config.reorder > 0.0 {
            let roll: f64 = self.rng.random();
            if roll < config.reorder {
                at += config.reorder_delay;
                reordered = true;
            }
        }
        if reordered {
            self.stats.reordered += 1;
        }

        self.stats.sent += 1;
        self.stats.bytes_sent += payload.len() as u64;
        self.seq += 1;
        self.in_flight.push(Reverse(InFlight {
            at,
            seq: self.seq,
            to,
            from_addr: source,
            to_addr: dest_internal,
            proto,
            payload,
        }));
        Some(())
    }

    /// When the next datagram is due, if any is in flight.
    #[must_use]
    pub fn next_delivery(&self) -> Option<Instant> {
        self.in_flight.peek().map(|Reverse(f)| f.at)
    }

    /// Move virtual time to `to` and return everything that became due, in
    /// delivery order.
    ///
    /// Time only moves forward: a `to` in the past delivers nothing and leaves
    /// the clock alone.
    pub fn advance_to(&mut self, to: Instant) -> Vec<(EndpointId, Datagram)> {
        if to > self.now {
            self.now = to;
        }
        self.drain_due()
    }

    /// Move virtual time forward by `by` and return everything due.
    pub fn advance(&mut self, by: Duration) -> Vec<(EndpointId, Datagram)> {
        self.advance_to(self.now + by)
    }

    /// Move virtual time to `to` without delivering anything.
    ///
    /// The caller is then responsible for draining: [`Vnet::advance_to`] with
    /// the same instant returns everything that came due. A driver that fires
    /// timers and delivers datagrams in a fixed order needs to move the clock
    /// and collect the mail as two separate steps, or the mail it collects
    /// while moving the clock has nowhere to go.
    pub fn skip_to(&mut self, to: Instant) {
        if to > self.now {
            self.now = to;
        }
    }

    /// Move virtual time to the next scheduled delivery and return it.
    ///
    /// Returns an empty vector when nothing is in flight, which is the signal
    /// that only a timer can make further progress.
    pub fn advance_to_next_delivery(&mut self) -> Vec<(EndpointId, Datagram)> {
        match self.next_delivery() {
            Some(at) => self.advance_to(at),
            None => Vec::new(),
        }
    }

    fn drain_due(&mut self) -> Vec<(EndpointId, Datagram)> {
        let mut out = Vec::new();
        while let Some(Reverse(front)) = self.in_flight.peek() {
            if front.at > self.now {
                break;
            }
            let Some(Reverse(f)) = self.in_flight.pop() else {
                break;
            };
            self.stats.delivered += 1;
            out.push((
                f.to,
                Datagram {
                    peer: f.from_addr,
                    local: f.to_addr,
                    proto: f.proto,
                    payload: f.payload,
                },
            ));
        }
        out
    }

    /// Resolve a destination address to an endpoint and the local address that
    /// endpoint will see.
    fn resolve(
        &self,
        to_addr: SocketAddr,
        from: SocketAddr,
    ) -> Result<(EndpointId, SocketAddr), DropReason> {
        if let Some(&id) = self.by_addr.get(&to_addr) {
            // A datagram addressed straight at an endpoint behind a NAT would
            // not reach it on a real network; only its external address works.
            if let Some(ep) = self.endpoints.get(id.0)
                && ep.behind.is_some()
            {
                return Err(DropReason::NoRoute);
            }
            return Ok((id, to_addr));
        }

        for nat in &self.nats {
            if !nat.owns(to_addr) {
                continue;
            }
            let internal = nat
                .translate_inbound(to_addr, from)
                .map_err(DropReason::Nat)?;
            let id = self
                .by_addr
                .get(&internal)
                .copied()
                .ok_or(DropReason::NoRoute)?;
            return Ok((id, internal));
        }

        Err(DropReason::NoRoute)
    }

    /// Counters for the run so far.
    #[must_use]
    pub fn stats(&self) -> VnetStats {
        self.stats
    }

    /// Take the recorded drops, clearing the log.
    ///
    /// Tests assert on this rather than on a bare counter, because "10 packets
    /// were lost" and "10 packets went to an address nothing is listening on"
    /// are very different failures.
    pub fn take_drops(&mut self) -> Vec<(SocketAddr, SocketAddr, DropReason)> {
        std::mem::take(&mut self.drops)
    }

    /// How many datagrams are still in flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}

impl std::fmt::Debug for Vnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vnet")
            .field("seed", &self.seed)
            .field("endpoints", &self.endpoints.len())
            .field("nats", &self.nats.len())
            .field("in_flight", &self.in_flight.len())
            .field("stats", &self.stats)
            .finish()
    }
}
