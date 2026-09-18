//! Deterministic simulation of the media path.
//!
//! webrtc-rs gap #16. `rtc`'s `MockRuntime` (`runtime/mock.rs:39`) has no loss,
//! latency or NAT modelling and no TCP, and the Go server's two pion-vnet
//! suites have no Rust equivalent. This module is that equivalent: an in-memory
//! network of endpoints on a virtual clock, with configurable delay, jitter,
//! random and burst loss, reorder, a bandwidth cap backed by a real queue, and
//! full-cone or symmetric NAT.
//!
//! Only the sans-IO media path belongs here. Control-plane actors (rooms,
//! participants, the bus) are simulated under `turmoil` instead.
//!
//! # Seeds
//!
//! Every random decision comes from one seeded stream, so a run is reproducible
//! from its seed alone. Tests take their seed from [`seed_from_env`] and print
//! it on failure through [`with_seed`]; re-running with `LK_VNET_SEED=<n>`
//! replays the failure exactly.
//!
//! ```
//! use lk_rtcio::vnet::{LinkConfig, Vnet};
//! use bytes::BytesMut;
//! use std::time::Duration;
//!
//! let mut net = Vnet::new(42);
//! let a = net.add_endpoint("10.0.0.1:7000".parse().unwrap());
//! let b = net.add_endpoint("10.0.0.2:7000".parse().unwrap());
//! net.set_link_both_ways(a, b, LinkConfig::perfect().with_delay(Duration::from_millis(20)));
//!
//! let b_addr = net.addr_of(b).unwrap();
//! net.send(a, b_addr, BytesMut::from(&b"hello"[..]));
//!
//! assert!(net.advance(Duration::from_millis(19)).is_empty());
//! let delivered = net.advance(Duration::from_millis(1));
//! assert_eq!(delivered.len(), 1);
//! ```

mod harness;
mod link;
mod nat;
mod net;

pub use harness::{Simulation, SimulationStats, VnetPeer};
pub use link::{BurstLoss, LinkConfig};
pub use nat::{Nat, NatDrop, NatKind};
pub use net::{DropReason, EndpointId, NatId, Vnet, VnetStats};

/// The environment variable a failing seed is replayed through.
pub const SEED_ENV: &str = "LK_VNET_SEED";

/// The seed for this run: `LK_VNET_SEED` if set, otherwise `default`.
///
/// Suites pass a fixed `default` so that CI is reproducible run to run, and
/// the nightly sim job sweeps seeds by setting the variable.
#[must_use]
pub fn seed_from_env(default: u64) -> u64 {
    std::env::var(SEED_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Run `body` with a seeded network, naming the seed if it fails.
///
/// A seeded test is only reproducible if the seed survives the failure, and a
/// panic message that does not carry it makes a CI failure unactionable. This
/// wrapper guarantees it does.
///
/// ```should_panic
/// # use lk_rtcio::vnet::with_seed;
/// // The panic message names the seed and how to replay it.
/// with_seed(7, |_net| panic!("boom"));
/// ```
#[allow(clippy::panic)]
pub fn with_seed<T>(default_seed: u64, body: impl FnOnce(&mut Vnet) -> T) -> T {
    let seed = seed_from_env(default_seed);
    let mut net = Vnet::new(seed);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut net))) {
        Ok(value) => value,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_owned());
            panic!("vnet run failed with seed {seed}: {message}\nreplay with {SEED_ENV}={seed}");
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use std::net::SocketAddr;
    use std::time::Duration;

    fn pair(net: &mut Vnet) -> (EndpointId, EndpointId, SocketAddr, SocketAddr) {
        let a = net.add_endpoint(SocketAddr::from(([10, 0, 0, 1], 7000)));
        let b = net.add_endpoint(SocketAddr::from(([10, 0, 0, 2], 7000)));
        let aa = net.addr_of(a).unwrap();
        let ba = net.addr_of(b).unwrap();
        (a, b, aa, ba)
    }

    fn payload(n: usize) -> BytesMut {
        BytesMut::from(vec![0u8; n].as_slice())
    }

    #[test]
    fn a_perfect_link_delivers_immediately_and_in_order() {
        let mut net = Vnet::new(1);
        let (a, b, _, b_addr) = pair(&mut net);
        for i in 0..10u8 {
            net.send(a, b_addr, BytesMut::from(&[i][..]));
        }
        let delivered = net.advance(Duration::ZERO);
        assert_eq!(delivered.len(), 10);
        for (i, (to, dg)) in delivered.iter().enumerate() {
            assert_eq!(*to, b);
            assert_eq!(dg.payload[0], i as u8);
        }
    }

    #[test]
    fn delay_holds_a_datagram_until_its_time() {
        let mut net = Vnet::new(1);
        let (a, _b, _, b_addr) = pair(&mut net);
        net.set_default_link(LinkConfig::perfect().with_delay(Duration::from_millis(50)));
        net.send(a, b_addr, payload(100));

        assert!(net.advance(Duration::from_millis(49)).is_empty());
        assert_eq!(net.in_flight(), 1);
        assert_eq!(net.advance(Duration::from_millis(1)).len(), 1);
    }

    #[test]
    fn the_same_seed_produces_the_same_run() {
        fn run(seed: u64) -> (VnetStats, Vec<u64>) {
            let mut net = Vnet::new(seed);
            let (a, _b, _, b_addr) = pair(&mut net);
            net.set_default_link(
                LinkConfig::perfect()
                    .with_delay(Duration::from_millis(20))
                    .with_jitter(Duration::from_millis(10))
                    .with_loss(0.1)
                    .with_reorder(0.05, Duration::from_millis(40)),
            );
            let start = net.now();
            let mut arrivals = Vec::new();
            for i in 0..500u32 {
                net.advance(Duration::from_millis(1));
                net.send(a, b_addr, BytesMut::from(&i.to_be_bytes()[..]));
                for _ in net.advance(Duration::ZERO) {
                    arrivals.push(net.now().saturating_duration_since(start).as_nanos() as u64);
                }
            }
            for _ in 0..200 {
                for _ in net.advance(Duration::from_millis(1)) {
                    arrivals.push(net.now().saturating_duration_since(start).as_nanos() as u64);
                }
            }
            (net.stats(), arrivals)
        }

        assert_eq!(run(99), run(99));
        assert_ne!(run(99).0, run(100).0, "different seeds should diverge");
    }

    #[test]
    fn loss_is_counted_and_roughly_matches_the_configured_rate() {
        let mut net = Vnet::new(4242);
        let (a, _b, _, b_addr) = pair(&mut net);
        net.set_default_link(LinkConfig::perfect().with_loss(0.2));
        for _ in 0..5000 {
            net.send(a, b_addr, payload(100));
        }
        let stats = net.stats();
        let rate = stats.dropped_loss as f64 / 5000.0;
        assert!(
            (0.17..0.23).contains(&rate),
            "20% loss produced {rate}, seed {}",
            net.seed()
        );
        // Every offered datagram is accounted for exactly once.
        assert_eq!(stats.offered, 5000);
        assert_eq!(
            stats.offered,
            stats.sent + stats.dropped_loss + stats.dropped_queue + stats.dropped_unroutable
        );
    }

    #[test]
    fn burst_loss_produces_runs_not_isolated_drops() {
        let mut net = Vnet::new(7);
        let (a, _b, _, b_addr) = pair(&mut net);
        net.set_default_link(LinkConfig::perfect().with_burst_loss(BurstLoss::runs(6.0, 60.0)));

        let mut lost = Vec::new();
        for i in 0..4000u32 {
            let before = net.stats().dropped_loss;
            net.send(a, b_addr, payload(100));
            if net.stats().dropped_loss > before {
                lost.push(i);
            }
        }
        assert!(!lost.is_empty(), "burst model dropped nothing");

        // A run-based model should produce far fewer distinct runs than lost
        // packets; an independent model would produce about as many.
        let runs = lost.windows(2).filter(|w| w[1] != w[0] + 1).count() + 1;
        assert!(
            (lost.len() as f64) / (runs as f64) > 2.0,
            "{} losses in {runs} runs is not bursty",
            lost.len()
        );
    }

    #[test]
    fn a_bandwidth_cap_queues_before_it_drops() {
        let mut net = Vnet::new(1);
        let (a, _b, _, b_addr) = pair(&mut net);
        // 1 Mbps, 200 ms of queue. A 1200-byte packet takes ~9.9 ms.
        net.set_default_link(
            LinkConfig::perfect()
                .with_bandwidth_bps(1_000_000)
                .with_max_queue_delay(Duration::from_millis(200)),
        );
        let start = net.now();
        for _ in 0..40 {
            net.send(a, b_addr, payload(1200));
        }

        let stats = net.stats();
        assert!(stats.dropped_queue > 0, "a 200 ms queue should tail-drop");
        assert!(stats.sent > 10, "it should queue before it drops");

        // Everything that was accepted drains, and the last one is late by
        // about the queue depth rather than arriving instantly.
        let mut last = start;
        for _ in 0..1000 {
            if net.in_flight() == 0 {
                break;
            }
            net.advance_to_next_delivery();
            last = net.now();
        }
        let depth = last.saturating_duration_since(start);
        assert!(
            depth >= Duration::from_millis(150) && depth <= Duration::from_millis(260),
            "queue drained over {depth:?}, expected roughly the configured depth"
        );
    }

    #[test]
    fn reorder_puts_a_packet_behind_a_later_one() {
        let mut net = Vnet::new(3);
        let (a, _b, _, b_addr) = pair(&mut net);
        net.set_default_link(
            LinkConfig::perfect()
                .with_delay(Duration::from_millis(10))
                .with_reorder(1.0, Duration::from_millis(50)),
        );
        net.send(a, b_addr, BytesMut::from(&b"first"[..]));
        net.set_default_link(LinkConfig::perfect().with_delay(Duration::from_millis(10)));
        net.send(a, b_addr, BytesMut::from(&b"second"[..]));

        let delivered = net.advance(Duration::from_millis(100));
        assert_eq!(delivered.len(), 2);
        assert_eq!(&delivered[0].1.payload[..], b"second");
        assert_eq!(&delivered[1].1.payload[..], b"first");
        assert_eq!(net.stats().reordered, 1);
    }

    #[test]
    fn full_cone_nat_lets_a_third_party_in() {
        let mut net = Vnet::new(1);
        let inside = net.add_endpoint(SocketAddr::from(([10, 0, 0, 1], 7000)));
        let server = net.add_endpoint(SocketAddr::from(([203, 0, 113, 1], 3478)));
        let stranger = net.add_endpoint(SocketAddr::from(([203, 0, 113, 2], 5000)));
        let nat = net.add_nat(NatKind::FullCone, [198, 51, 100, 1].into());
        net.put_behind_nat(inside, nat);

        let server_addr = net.addr_of(server).unwrap();
        net.send(inside, server_addr, payload(10));
        let delivered = net.advance(Duration::ZERO);
        assert_eq!(delivered.len(), 1);
        let mapped = delivered[0].1.peer;
        assert_eq!(mapped.ip(), std::net::IpAddr::from([198, 51, 100, 1]));

        // The stranger never received anything from inside, but a full-cone
        // mapping is open to anyone who knows the address.
        net.send(stranger, mapped, payload(10));
        let delivered = net.advance(Duration::ZERO);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].0, inside);
    }

    #[test]
    fn symmetric_nat_rejects_a_third_party() {
        let mut net = Vnet::new(1);
        let inside = net.add_endpoint(SocketAddr::from(([10, 0, 0, 1], 7000)));
        let server = net.add_endpoint(SocketAddr::from(([203, 0, 113, 1], 3478)));
        let stranger = net.add_endpoint(SocketAddr::from(([203, 0, 113, 2], 5000)));
        let nat = net.add_nat(NatKind::Symmetric, [198, 51, 100, 1].into());
        net.put_behind_nat(inside, nat);

        let server_addr = net.addr_of(server).unwrap();
        net.send(inside, server_addr, payload(10));
        let mapped = net.advance(Duration::ZERO)[0].1.peer;

        assert!(net.send(stranger, mapped, payload(10)).is_none());
        let drops = net.take_drops();
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].2, DropReason::Nat(NatDrop::WrongPeer));
    }

    #[test]
    fn an_endpoint_behind_a_nat_is_unreachable_at_its_private_address() {
        let mut net = Vnet::new(1);
        let inside = net.add_endpoint(SocketAddr::from(([10, 0, 0, 1], 7000)));
        let outside = net.add_endpoint(SocketAddr::from(([203, 0, 113, 1], 3478)));
        let nat = net.add_nat(NatKind::FullCone, [198, 51, 100, 1].into());
        net.put_behind_nat(inside, nat);

        let private = net.addr_of(inside).unwrap();
        assert!(net.send(outside, private, payload(10)).is_none());
        assert_eq!(net.take_drops()[0].2, DropReason::NoRoute);
    }

    #[test]
    fn sending_to_nothing_is_a_named_drop_not_a_silent_one() {
        let mut net = Vnet::new(1);
        let (a, _b, _, _) = pair(&mut net);
        assert!(
            net.send(a, SocketAddr::from(([192, 0, 2, 1], 1)), payload(10))
                .is_none()
        );
        assert_eq!(net.take_drops()[0].2, DropReason::NoRoute);
        assert_eq!(net.stats().dropped_unroutable, 1);
    }

    #[test]
    fn time_never_runs_backwards() {
        let mut net = Vnet::new(1);
        let start = net.now();
        net.advance(Duration::from_secs(1));
        net.advance_to(start);
        assert_eq!(net.now(), start + Duration::from_secs(1));
    }

    #[test]
    fn with_seed_reports_the_seed_on_failure() {
        let err = std::panic::catch_unwind(|| {
            with_seed(12345, |_net| -> () { panic!("scenario failed") })
        })
        .expect_err("body panicked, so with_seed must too");
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(
            message.contains("12345"),
            "message did not name the seed: {message}"
        );
        assert!(
            message.contains(SEED_ENV),
            "message did not say how to replay: {message}"
        );
    }
}
