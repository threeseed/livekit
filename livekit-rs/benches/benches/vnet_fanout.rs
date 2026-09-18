//! Fan-out cost of the virtual network.
//!
//! Placeholder benchmark so that the `perf` CI job has a target from Phase 0
//! and later phases only add to existing infrastructure. It measures the one
//! thing the vnet has to be fast at: pushing a lot of datagrams through a lot
//! of links without the simulator itself becoming the bottleneck.

// `criterion_group!` expands to an undocumented function.
#![allow(missing_docs)]

use std::hint::black_box;
use std::net::SocketAddr;

use bytes::BytesMut;
use criterion::{Criterion, criterion_group, criterion_main};
use lk_rtcio::vnet::{LinkConfig, Vnet};

fn fanout(c: &mut Criterion) {
    c.bench_function("vnet/fanout/64-peers/1200-byte", |b| {
        b.iter(|| {
            let mut net = Vnet::new(1);
            net.set_default_link(LinkConfig::broadband());
            let hub = net.add_endpoint(SocketAddr::from(([10, 0, 0, 1], 7880)));
            let peers: Vec<_> = (0..64u16)
                .map(|i| {
                    net.add_endpoint(SocketAddr::from(([10, 1, (i >> 8) as u8, i as u8], 5000)))
                })
                .collect();
            let addrs: Vec<_> = peers.iter().filter_map(|p| net.addr_of(*p)).collect();

            let payload = BytesMut::from(vec![0u8; 1200].as_slice());
            for _ in 0..16 {
                for addr in &addrs {
                    net.send(hub, *addr, payload.clone());
                }
                black_box(net.advance(std::time::Duration::from_millis(20)).len());
            }
            black_box(net.stats())
        });
    });
}

criterion_group!(benches, fanout);
criterion_main!(benches);
