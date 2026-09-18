//! Cross-crate smoke tests.
//!
//! Phase 0 has one crate with behaviour in it, so this file exists mostly to
//! give the `integration` CI job something to run and later phases somewhere to
//! put a single-node scenario. It asserts the one cross-crate property that
//! holds today: the shard and the simulator agree on what a datagram is.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::Duration;

use lk_rtcio::vnet::{LinkConfig, Vnet};
use lk_rtcio::{Datagram, Shard, ShardConfig};

#[test]
fn a_datagram_survives_a_round_trip_through_the_simulator() {
    let mut net = Vnet::new(1);
    net.set_default_link(LinkConfig::broadband());
    let a = net.add_endpoint(SocketAddr::from(([10, 0, 0, 1], 7881)));
    let b = net.add_endpoint(SocketAddr::from(([10, 0, 0, 2], 50000)));
    let b_addr = net.addr_of(b).unwrap();

    let sent = Datagram::udp(
        net.addr_of(a).unwrap(),
        b_addr,
        &b"\x80\x60\x00\x01payload"[..],
    );
    net.send(a, b_addr, sent.payload.clone()).unwrap();

    let delivered = net.advance(Duration::from_millis(100));
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].1.payload, sent.payload);
    assert_eq!(delivered[0].1.peer, net.addr_of(a).unwrap());
    assert_eq!(delivered[0].1.local, b_addr);
}

#[test]
fn an_empty_shard_reports_no_work_and_no_deadline() {
    let shard = Shard::new(ShardConfig::new(SocketAddr::from(([0, 0, 0, 0], 7881))));
    assert!(shard.is_empty());
    assert!(!shard.has_pending_work());
    assert_eq!(shard.poll_timeout(), None);
    assert_eq!(shard.local_addr(), SocketAddr::from(([0, 0, 0, 0], 7881)));
}
