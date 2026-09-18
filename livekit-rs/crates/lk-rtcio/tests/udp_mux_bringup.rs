//! The Phase-0 exit gate (issues #982 and #983).
//!
//! One Rust process on a single UDP port completes ICE, DTLS and SRTP with a
//! large number of concurrent `rtc` peers, all demultiplexed by the mux, all
//! driven by one shard, all over the deterministic vnet.
//!
//! What this proves: that the sans-IO core can replace the `webrtc` facade's
//! one-socket-per-PeerConnection model. What it does not prove: anything about
//! sharding, cross-shard forwarding or TCP mux, which are webrtc-rs gaps #1 and
//! #2 in Phase 1, or anything about a real Chrome client, which needs a
//! browser and lives in the `interop` CI job.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lk_rtcio::vnet::{LinkConfig, NatKind, Simulation, Vnet, VnetPeer, seed_from_env};
use lk_rtcio::{Shard, ShardConfig};
use rtc::peer_connection::RTCPeerConnectionBuilder;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::setting_engine::SettingEngineBuilder;
use rtc::peer_connection::transport::{
    CandidateConfig, CandidateHostConfig, RTCDtlsRole, RTCIceCandidate, RTCIceCandidateInit,
};

/// The address the SFU publishes. One port, every connection.
const SFU_ADDR: ([u8; 4], u16) = ([10, 0, 0, 1], 7881);

/// Peer `index` gets its own /16 so a 500-peer run does not run out of hosts.
fn peer_addr(index: u16) -> SocketAddr {
    SocketAddr::from((
        [10, 1, (index >> 8) as u8, (index & 0xFF) as u8],
        50_000 + (index % 1000),
    ))
}

/// The local ufrag `rtc` chose for a connection, read back out of its SDP.
///
/// The mux routes a first packet on this and nothing else, so reading it from
/// the description rather than generating it is the only way to be sure the
/// mux and the connection agree.
fn ufrag_of(sdp: &str) -> String {
    sdp.lines()
        .find_map(|line| line.trim().strip_prefix("a=ice-ufrag:"))
        .map(str::to_owned)
        .expect("every offer or answer carries an ice-ufrag")
}

fn host_candidate(addr: SocketAddr) -> RTCIceCandidateInit {
    let candidate = CandidateHostConfig {
        base_config: CandidateConfig {
            network: "udp".to_owned(),
            address: addr.ip().to_string(),
            port: addr.port(),
            component: 1,
            ..Default::default()
        },
        ..Default::default()
    }
    .new_candidate_host()
    .expect("a host candidate for a literal address");
    RTCIceCandidate::from(&candidate)
        .to_json()
        .expect("candidate serialises")
}

/// Build one SFU-side connection and one peer-side connection, negotiate them,
/// and register both with the simulation.
///
/// The SFU always answers, which is what `livekit-server` does for a
/// subscriber transport, and takes the DTLS server role.
fn connect_one(
    sim: &mut Simulation,
    index: u16,
    now: Instant,
    behind_nat: Option<lk_rtcio::vnet::NatId>,
) {
    let sfu_addr = SocketAddr::from(SFU_ADDR);
    let addr = peer_addr(index);
    let endpoint = sim.net.add_endpoint(addr);
    if let Some(nat) = behind_nat {
        sim.net.put_behind_nat(endpoint, nat);
    }
    // The address the SFU will see, which is the one the peer must advertise.
    let observed = sim
        .net
        .observed_addr(endpoint, sfu_addr)
        .expect("the endpoint exists");

    // --- peer: offers ------------------------------------------------------
    let mut peer_pc = RTCPeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_setting_engine(
            SettingEngineBuilder::new()
                .with_answering_dtls_role(RTCDtlsRole::Server)
                .build(),
        )
        .build(now)
        .expect("peer connection builds");
    peer_pc
        .create_data_channel("_reliable", None)
        .expect("a data channel, so the offer has an m=application section");
    peer_pc
        .add_local_candidate(host_candidate(observed))
        .expect("peer advertises the address the SFU will see");

    let offer = peer_pc.create_offer(None).expect("peer creates an offer");
    peer_pc
        .set_local_description(now, offer.clone())
        .expect("peer sets its local description");

    // --- sfu: answers ------------------------------------------------------
    let mut sfu_pc = RTCPeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_setting_engine(
            SettingEngineBuilder::new()
                .with_answering_dtls_role(RTCDtlsRole::Client)
                .build(),
        )
        .build(now)
        .expect("sfu connection builds");
    sfu_pc
        .set_remote_description(now, offer)
        .expect("sfu takes the offer");
    sfu_pc
        .add_local_candidate(host_candidate(sfu_addr))
        .expect("sfu advertises its single published port");

    let answer = sfu_pc.create_answer(None).expect("sfu creates an answer");
    sfu_pc
        .set_local_description(now, answer.clone())
        .expect("sfu sets its local description");
    let sfu_ufrag = ufrag_of(&answer.sdp);

    peer_pc
        .set_remote_description(now, answer)
        .expect("peer takes the answer");

    // --- candidate exchange ------------------------------------------------
    peer_pc
        .add_remote_candidate(host_candidate(sfu_addr))
        .expect("peer learns the sfu's candidate");
    sfu_pc
        .add_remote_candidate(host_candidate(observed))
        .expect("sfu learns the peer's candidate");

    // The mux registration is the whole point: nothing else tells the shard
    // which connection a first STUN check belongs to.
    sim.shard
        .insert(sfu_pc, &sfu_ufrag)
        .expect("each connection has a distinct ufrag");
    sim.push_peer(VnetPeer::new(endpoint, observed, peer_pc));
}

fn build_simulation(seed: u64, link: LinkConfig) -> Simulation {
    let mut net = Vnet::new(seed);
    net.set_default_link(link);
    let sfu_addr = SocketAddr::from(SFU_ADDR);
    let sfu_endpoint = net.add_endpoint(sfu_addr);
    let shard = Shard::new(ShardConfig::new(sfu_addr));
    Simulation::new(net, shard, sfu_endpoint)
}

#[test]
fn one_peer_completes_ice_dtls_and_srtp_over_the_mux() {
    let mut sim = build_simulation(seed_from_env(1), LinkConfig::perfect());
    let now = sim.net.now();
    connect_one(&mut sim, 0, now, None);

    assert!(
        sim.run_until_all_connected(Duration::from_secs(30)),
        "peer did not connect: state {:?}, shard {:?}, net {:?}",
        sim.peers[0].state(),
        sim.shard.stats(),
        sim.net.stats()
    );

    // Routing actually went through the mux rather than a single implicit
    // connection: one flow was bound by ufrag, and the rest by 5-tuple.
    let mux = sim.shard.mux_stats();
    assert_eq!(
        mux.first_packets, 1,
        "the flow should be bound exactly once"
    );
    assert!(
        mux.established > 0,
        "later datagrams should take the fast path"
    );
    assert_eq!(mux.unroutable, 0, "nothing should have been dropped");
}

#[test]
fn a_peer_behind_a_full_cone_nat_connects_on_its_mapped_address() {
    let mut sim = build_simulation(seed_from_env(2), LinkConfig::perfect());
    let nat = sim.net.add_nat(NatKind::FullCone, [198, 51, 100, 1].into());
    let now = sim.net.now();
    connect_one(&mut sim, 0, now, Some(nat));

    assert!(
        sim.run_until_all_connected(Duration::from_secs(30)),
        "peer behind a NAT did not connect: state {:?}",
        sim.peers[0].state()
    );
    // It really was the translated address, not the private one.
    assert_eq!(
        sim.peers[0].addr.ip(),
        std::net::IpAddr::from([198, 51, 100, 1])
    );
}

#[test]
fn jitter_and_reorder_do_not_stall_the_handshake() {
    // Everything a real path does to timing, without dropping anything: a
    // 30 ms link with 10 ms of jitter and 2% reorder. ICE and DTLS both have
    // to tolerate out-of-order arrival, and a regression in the shard's timer
    // handling shows up here rather than on the perfect link.
    //
    // Loss is deliberately absent. `rtc` 0.21.0-rc.2 does not retransmit a
    // lost handshake flight, so a lossy variant of this test would be
    // measuring that gap and not the shard; it lives in
    // `rtc_handshake_loss_recovery.rs`, ignored, as the regression test for
    // the fix.
    let link = LinkConfig::perfect()
        .with_delay(Duration::from_millis(30))
        .with_jitter(Duration::from_millis(10))
        .with_reorder(0.02, Duration::from_millis(40));
    let mut sim = build_simulation(seed_from_env(3), link);
    let now = sim.net.now();
    for index in 0..4 {
        connect_one(&mut sim, index, now, None);
    }

    assert!(
        sim.run_until_all_connected(Duration::from_secs(60)),
        "{} of 4 connected over a jittery link, seed {}; peers {:?}; sfu {:?}; net {:?}",
        sim.connected_count(),
        sim.net.seed(),
        sim.peer_states(),
        sim.shard_states(),
        sim.net.stats()
    );
    assert!(
        sim.net.stats().reordered > 0,
        "the link should have reordered something at 2%"
    );
    assert_eq!(sim.shard.mux_stats().unroutable, 0);
}

/// The exit gate: 500 concurrent peers, one UDP port, under one second of wall
/// clock for the vnet suite.
#[test]
fn five_hundred_peers_complete_the_handshake_on_one_port() {
    const PEERS: u16 = 500;

    let started = Instant::now();
    let mut sim = build_simulation(seed_from_env(4), LinkConfig::perfect());
    let now = sim.net.now();
    for index in 0..PEERS {
        connect_one(&mut sim, index, now, None);
    }
    let setup = started.elapsed();

    let drive_started = Instant::now();
    let all_connected = sim.run_until_all_connected(Duration::from_secs(60));
    let drive = drive_started.elapsed();

    assert!(
        all_connected,
        "{} of {PEERS} connected; shard {:?}, mux {:?}, net {:?}",
        sim.connected_count(),
        sim.shard.stats(),
        sim.shard.mux_stats(),
        sim.net.stats()
    );

    // One connection per peer, all on one shard behind one address.
    assert_eq!(sim.shard.len(), usize::from(PEERS));
    let mux = sim.shard.mux_stats();
    assert_eq!(
        mux.first_packets,
        u64::from(PEERS),
        "each peer's flow should be bound exactly once"
    );
    assert_eq!(mux.unroutable, 0, "no datagram should have been dropped");
    assert_eq!(mux.rebinds, 0, "no two peers should have shared an address");

    // No dropped-packet errors anywhere: not in the mux, not on the network.
    assert_eq!(sim.shard.stats().datagrams_unroutable, 0);
    assert_eq!(sim.net.stats().dropped_loss, 0);
    assert_eq!(sim.net.stats().dropped_queue, 0);
    assert_eq!(sim.net.stats().dropped_unroutable, 0);
    assert_eq!(sim.stats().undeliverable, 0);

    // The exit gate's timing clause. Setup is 1000 DTLS certificates and key
    // pairs, which is `rtc`'s cost and not the simulator's, so the budget
    // measured here is the drive loop: the part this crate owns.
    println!(
        "{PEERS} peers: setup {setup:?}, drive {drive:?}, {} steps, {} shard transmits",
        sim.stats().steps,
        sim.stats().shard_transmits
    );
    assert!(
        drive < Duration::from_secs(1),
        "driving {PEERS} peers took {drive:?}, over the one-second exit gate"
    );
}
