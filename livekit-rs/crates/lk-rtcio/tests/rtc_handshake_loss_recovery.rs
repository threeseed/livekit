//! Characterisation test for a `rtc` gap found by the Phase-0 vnet harness.
//!
//! # The finding
//!
//! `rtc` 0.21.0-rc.2 does not recover a handshake flight that is lost. A single
//! dropped packet at the wrong moment leaves the connection in `Connecting`
//! for as long as the test runs, with ICE traffic continuing normally around
//! it and no error reported on either side.
//!
//! It is not a rate threshold. Runs at 1%, 2% and 5% loss all wedge on some
//! seeds and complete on others, which is the signature of a retransmission
//! that never happens rather than a backoff that gives up: at 5% loss a
//! handshake of a few flights should complete with retries, not stall.
//!
//! # Why this file exists rather than a bug report alone
//!
//! This test uses no `lk-rtcio` code beyond the simulator: two bare
//! `RTCPeerConnection`s, driven directly, with no [`Shard`] and no mux between
//! them. That is deliberate. When this reproduces, the SFU's own I/O layer is
//! not a suspect, and the same scenario run through the shard
//! (`udp_mux_bringup.rs`) is comparable against it.
//!
//! # Status
//!
//! `#[ignore]` because it fails today. It is the regression test for the fix:
//! run it with `--ignored`, and when it passes against a forked or updated
//! `rtc`, drop the attribute and raise the loss rate.
//!
//! Tracked as a webrtc-rs gap in `docs/RTC_FORK.md`. Until it is closed, the
//! plan's Phase-3 scenarios that depend on loss (RTX repair, BWE convergence,
//! allocator behaviour under 20% loss) can only be run on connections that are
//! already established, never across a handshake.
//!
//! [`Shard`]: lk_rtcio::Shard

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use lk_rtcio::Datagram;
use lk_rtcio::vnet::{LinkConfig, Vnet, seed_from_env};
use rtc::peer_connection::RTCPeerConnectionBuilder;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::setting_engine::SettingEngineBuilder;
use rtc::peer_connection::event::RTCPeerConnectionEvent;
use rtc::peer_connection::state::RTCPeerConnectionState;
use rtc::peer_connection::transport::{
    CandidateConfig, CandidateHostConfig, RTCDtlsRole, RTCIceCandidate, RTCIceCandidateInit,
};
use rtc::sansio::Protocol;

/// How many pairs to run. One pair is too few to see a probabilistic stall.
const PAIRS: u16 = 4;

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
    .unwrap();
    RTCIceCandidate::from(&candidate).to_json().unwrap()
}

/// Run `PAIRS` offer/answer pairs over a link with `loss`, returning how many
/// of the `2 * PAIRS` connections reached `Connected`.
fn run(seed: u64, loss: f64) -> (usize, Vec<RTCPeerConnectionState>) {
    let mut net = Vnet::new(seed);
    net.set_default_link(
        LinkConfig::perfect()
            .with_delay(Duration::from_millis(30))
            .with_jitter(Duration::from_millis(10))
            .with_loss(loss),
    );
    let now = net.now();

    // (endpoint, connection, last reported state)
    let mut connections = Vec::new();

    for index in 0..PAIRS {
        let offerer_addr = SocketAddr::from(([10, 0, 0, 1], 7000 + index));
        let answerer_addr = SocketAddr::from(([10, 1, 0, 1], 7000 + index));
        let offerer_ep = net.add_endpoint(offerer_addr);
        let answerer_ep = net.add_endpoint(answerer_addr);

        let mut offerer = RTCPeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().build())
            .with_setting_engine(
                SettingEngineBuilder::new()
                    .with_answering_dtls_role(RTCDtlsRole::Server)
                    .build(),
            )
            .build(now)
            .unwrap();
        offerer.create_data_channel("_reliable", None).unwrap();
        offerer
            .add_local_candidate(host_candidate(offerer_addr))
            .unwrap();
        let offer = offerer.create_offer(None).unwrap();
        offerer.set_local_description(now, offer.clone()).unwrap();

        let mut answerer = RTCPeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().build())
            .with_setting_engine(
                SettingEngineBuilder::new()
                    .with_answering_dtls_role(RTCDtlsRole::Client)
                    .build(),
            )
            .build(now)
            .unwrap();
        answerer.set_remote_description(now, offer).unwrap();
        answerer
            .add_local_candidate(host_candidate(answerer_addr))
            .unwrap();
        let answer = answerer.create_answer(None).unwrap();
        answerer.set_local_description(now, answer.clone()).unwrap();
        offerer.set_remote_description(now, answer).unwrap();

        offerer
            .add_remote_candidate(host_candidate(answerer_addr))
            .unwrap();
        answerer
            .add_remote_candidate(host_candidate(offerer_addr))
            .unwrap();

        connections.push((offerer_ep, offerer, RTCPeerConnectionState::New));
        connections.push((answerer_ep, answerer, RTCPeerConnectionState::New));
    }

    let deadline = net.now() + Duration::from_secs(180);
    loop {
        let now = net.now();
        for (endpoint, datagram) in net.advance_to(now) {
            if let Some((_, pc, _)) = connections.iter_mut().find(|(e, _, _)| *e == endpoint) {
                let _ = pc.handle_read(datagram.into_tagged(now));
            }
        }
        for (_, pc, _) in &mut connections {
            if pc.poll_timeout().is_some_and(|at| at <= now) {
                let _ = pc.handle_timeout(now);
            }
        }
        for (endpoint, pc, state) in &mut connections {
            while let Some(event) = pc.poll_event() {
                if let RTCPeerConnectionEvent::OnConnectionStateChangeEvent(new) = event {
                    *state = new;
                }
            }
            while pc.poll_read().is_some() {}
            while let Some(tagged) = pc.poll_write() {
                let datagram = Datagram::from_tagged(tagged);
                net.send(*endpoint, datagram.peer, datagram.payload);
            }
        }

        if connections
            .iter()
            .all(|(_, _, s)| *s == RTCPeerConnectionState::Connected)
        {
            break;
        }
        if net.now() >= deadline {
            break;
        }

        let mut next = net.next_delivery();
        for (_, pc, _) in &mut connections {
            if let Some(at) = pc.poll_timeout() {
                next = Some(next.map_or(at, |current: Instant| current.min(at)));
            }
        }
        match next {
            Some(at) => net.skip_to(at.max(net.now() + Duration::from_micros(1))),
            None => break,
        }
    }

    let states: Vec<_> = connections.iter().map(|(_, _, s)| *s).collect();
    let connected = states
        .iter()
        .filter(|s| **s == RTCPeerConnectionState::Connected)
        .count();
    (connected, states)
}

#[test]
fn a_clean_link_connects_every_pair() {
    // The control. If this ever fails, the harness is broken, not `rtc`.
    let seed = seed_from_env(3);
    let (connected, states) = run(seed, 0.0);
    assert_eq!(
        connected,
        usize::from(PAIRS) * 2,
        "seed {seed}: {connected} connected on a clean link, states {states:?}"
    );
}

#[test]
fn jitter_and_delay_alone_do_not_stall_a_handshake() {
    // Everything the lossy link does except drop packets. This isolates loss
    // as the trigger rather than timing.
    let seed = seed_from_env(3);
    let (connected, states) = run(seed, 0.0);
    assert_eq!(
        connected,
        usize::from(PAIRS) * 2,
        "seed {seed}: {connected} connected under jitter, states {states:?}"
    );
}

#[test]
#[ignore = "rtc 0.21.0-rc.2 does not retransmit a lost handshake flight; see this file's header and docs/RTC_FORK.md"]
fn a_lost_handshake_flight_is_retransmitted() {
    // 1% loss over a 30 ms link. Every flight of the handshake has a 99%
    // chance of arriving, so a retransmitting implementation completes all
    // eight connections comfortably within the 180-second virtual budget.
    let seed = seed_from_env(11);
    let (connected, states) = run(seed, 0.01);
    assert_eq!(
        connected,
        usize::from(PAIRS) * 2,
        "seed {seed}: only {connected} of {} connected at 1% loss, states {states:?}",
        PAIRS * 2
    );
}
