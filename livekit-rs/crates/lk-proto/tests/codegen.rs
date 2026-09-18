//! Acceptance tests for the codegen (issue #980).
//!
//! These assert the two things a Phase-1 consumer depends on: that every
//! vendored proto produced Rust types, and that JSON frames follow protojson
//! semantics rather than serde's defaults.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use lk_proto::livekit;

/// Every `.proto` file under `protobufs/`, so the count below is derived and
/// not a number someone has to remember to update.
fn vendored_proto_count() -> usize {
    fn walk(dir: &std::path::Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, count);
            } else if path.extension().is_some_and(|e| e == "proto") {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("protobufs"),
        &mut count,
    );
    count
}

#[test]
fn every_vendored_proto_is_in_the_descriptor_set() {
    use prost::Message as _;
    let set = prost_types::FileDescriptorSet::decode(lk_proto::FILE_DESCRIPTOR_SET)
        .expect("the embedded descriptor set decodes");

    // `google/protobuf/*.proto` come in as imports, so the set is a superset of
    // what is vendored; what matters is that nothing vendored is missing.
    let generated: std::collections::BTreeSet<String> = set
        .file
        .iter()
        .filter_map(|f| f.name.clone())
        .filter(|name| !name.starts_with("google/protobuf/"))
        .collect();

    assert_eq!(
        generated.len(),
        vendored_proto_count(),
        "generated {} of {} vendored protos: {generated:?}",
        generated.len(),
        vendored_proto_count()
    );
    assert_eq!(generated.len(), 43, "the plan's count of 43 has moved");
}

#[test]
fn the_core_message_protos_produced_types() {
    // One representative type from each of the protos the port depends on
    // most, so a codegen regression fails here rather than in Phase 2.
    let _ = livekit::SignalRequest::default(); // livekit_rtc.proto
    let _ = livekit::Room::default(); // livekit_models.proto
    let _ = livekit::CreateRoomRequest::default(); // livekit_room.proto
    let _ = livekit::EgressInfo::default(); // livekit_egress.proto
    let _ = livekit::IngressInfo::default(); // livekit_ingress.proto
    let _ = livekit::WebhookEvent::default(); // livekit_webhook.proto
    let _ = livekit::MetricsBatch::default(); // livekit_metrics.proto
    let _ = livekit::AnalyticsStat::default(); // livekit_analytics.proto
    let _ = livekit::Job::default(); // livekit_agent.proto
}

#[test]
fn the_internal_and_rpc_protos_produced_types() {
    // These are exactly what `livekit-protocol` 0.7.13 does not generate, and
    // what a node needs to join a Go cluster.
    let _ = livekit::Node::default(); // livekit_internal.proto
    let _ = livekit::NodeStats::default(); // livekit_internal.proto
    let _ = livekit::StartSession::default(); // rpc/signal.proto
    let _ = lk_proto::rpc::KeepalivePing::default(); // rpc/keepalive.proto
}

#[test]
fn the_five_twirp_services_are_generated_with_their_routes() {
    use lk_proto::twirp;
    assert_eq!(twirp::SERVICE_COUNT, 5);

    assert_eq!(twirp::room_service::SERVICE_NAME, "livekit.RoomService");
    assert_eq!(twirp::room_service::METHODS.len(), 14);

    let create = twirp::room_service::METHODS
        .iter()
        .find(|m| m.name == "CreateRoom")
        .expect("RoomService has CreateRoom");
    assert_eq!(create.path, "/twirp/livekit.RoomService/CreateRoom");

    // Every route is `/twirp/<service>/<method>` and unique.
    let mut seen = std::collections::BTreeSet::new();
    for service in [
        twirp::room_service::METHODS,
        twirp::agent_dispatch_service::METHODS,
        twirp::egress::METHODS,
        twirp::ingress::METHODS,
        twirp::sip::METHODS,
    ] {
        for method in service {
            assert!(method.path.starts_with("/twirp/"), "{}", method.path);
            assert!(method.path.ends_with(method.name), "{}", method.path);
            assert!(seen.insert(method.path), "duplicate route {}", method.path);
        }
    }
    assert_eq!(seen.len(), twirp::METHOD_COUNT);
}

#[test]
fn psrpc_options_survive_codegen() {
    use lk_proto::psrpc;

    // The whole point of decoding the descriptor a second time: the
    // `psrpc.options` extension on each method. If it were being dropped, every
    // method here would read `topics: false` with an empty topic, and the
    // generated node would subscribe to the wrong Redis channels.
    let delete = psrpc::PsrpcMethod::find(psrpc::room::METHODS, "DeleteRoom")
        .expect("rpc.Room has DeleteRoom");
    assert!(
        delete.topics,
        "DeleteRoom is topic-routed in rpc/room.proto"
    );
    assert_eq!(delete.topic_params.names, ["room"]);
    assert_eq!(delete.topic_params.group, "room");
    assert!(delete.topic_params.typed);

    assert_eq!(psrpc::room::SERVICE_NAME, "Room");
    assert_eq!(psrpc::room::room_topic("room1"), vec!["room1".to_owned()]);
}

#[test]
fn psrpc_channel_names_match_the_go_implementation() {
    use lk_proto::psrpc::{ChannelNames, room};

    let method = lk_proto::psrpc::PsrpcMethod::find(room::METHODS, "DeleteRoom").unwrap();
    let topic = room::room_topic("room1");
    let names = ChannelNames {
        service: room::SERVICE_NAME,
        method: method.name,
        topic: &topic,
        queue: matches!(method.routing, lk_proto::psrpc::Routing::Queue),
    };
    assert_eq!(names.rpc().legacy, "Room|DeleteRoom|room1|REQ");
    assert_eq!(names.rpc().server, "SRV.Room.room1.Q");
}

#[test]
fn json_frames_round_trip_with_protojson_semantics() {
    // A `SignalResponse` as the Go server emits it, with protojson's
    // lowerCamelCase field names and its string encoding of 64-bit integers.
    let go_frame = r#"{
        "join": {
            "room": {
                "sid": "RM_abc123",
                "name": "test-room",
                "emptyTimeout": 300,
                "creationTime": "1700000000",
                "numParticipants": 2
            },
            "participant": {
                "sid": "PA_def456",
                "identity": "alice",
                "state": "ACTIVE"
            },
            "serverVersion": "1.9.0",
            "subscriberPrimary": true
        }
    }"#;

    let decoded: livekit::SignalResponse =
        serde_json::from_str(go_frame).expect("a Go-produced frame decodes");

    let livekit::signal_response::Message::Join(join) = decoded
        .message
        .as_ref()
        .expect("the frame carries a message")
    else {
        panic!("expected a join response");
    };
    let room = join.room.as_ref().expect("join carries a room");
    assert_eq!(room.sid, "RM_abc123");
    assert_eq!(room.empty_timeout, 300);
    assert_eq!(room.creation_time, 1_700_000_000);
    assert!(join.subscriber_primary);

    // Re-encode and decode again: the value is stable through a round trip,
    // which is the property the signal path actually relies on.
    let re_encoded = serde_json::to_string(&decoded).expect("re-encodes");
    let round_tripped: livekit::SignalResponse =
        serde_json::from_str(&re_encoded).expect("re-decodes");
    assert_eq!(decoded, round_tripped);
}

#[test]
fn unknown_json_fields_are_discarded_not_rejected() {
    // protojson's DiscardUnknown. A newer Go node sending a field this build
    // has never heard of must not take the session down; `prost`'s own JSON
    // support would reject it, which is why `pbjson` is a `cargo deny` rule
    // and not a preference.
    let frame = r#"{"sid":"RM_abc","name":"r","fieldFromTheFuture":{"nested":1}}"#;
    let room: livekit::Room = serde_json::from_str(frame).expect("unknown fields are ignored");
    assert_eq!(room.sid, "RM_abc");
    assert_eq!(room.name, "r");
}

#[test]
fn enums_use_their_protojson_names() {
    let frame = r#"{"state":"ACTIVE","kind":"AGENT"}"#;
    let participant: livekit::ParticipantInfo =
        serde_json::from_str(frame).expect("named enum values decode");
    assert_eq!(
        participant.state(),
        livekit::participant_info::State::Active
    );
    assert_eq!(participant.kind(), livekit::participant_info::Kind::Agent);

    let re_encoded = serde_json::to_value(&participant).expect("re-encodes");
    assert_eq!(re_encoded["state"], "ACTIVE");
}

#[test]
fn the_vendored_revision_is_recorded() {
    let pin = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("protobufs/PIN"),
    )
    .expect("the pin file exists");
    assert!(
        pin.contains(lk_proto::PROTOCOL_REVISION),
        "PROTOCOL_REVISION does not match protobufs/PIN"
    );
}
