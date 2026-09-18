//! Cross-implementation token tests.
//!
//! The fixtures in `testdata/go_tokens.json` were minted by the Go server's own
//! `protocol/auth` package, through the program in `testdata/gotools`. Testing
//! against them rather than against tokens this crate minted itself is the
//! point: a second reading of the spec that agrees with the first proves
//! nothing.
//!
//! The reverse direction (a Rust token verified by Go) is checked by
//! `gotools verify`, which is why the round-trip test below also asserts the
//! exact claim JSON: if it matches what Go produces byte for byte, Go's
//! verifier accepts it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;

use lk_auth::{
    AccessToken, AgentGrant, ApiKeyTokenVerifier, FileBasedKeyProvider, InferenceGrant,
    ObservabilityGrant, SipGrant, VideoGrant,
};
use lk_proto::livekit::{TrackSource, participant_info};
use serde::Deserialize;

const API_KEY: &str = "devkey";
const API_SECRET: &str = "secret-that-is-at-least-32-characters";

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    token: String,
}

fn fixtures() -> Vec<Fixture> {
    let raw = include_str!("../testdata/go_tokens.json");
    serde_json::from_str(raw).expect("fixtures must parse")
}

fn fixture(name: &str) -> String {
    fixtures()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no fixture named {name}"))
        .token
}

#[test]
fn verifies_a_join_token_minted_by_the_go_server() {
    let token = fixture("join");

    let verifier = ApiKeyTokenVerifier::parse(&token).unwrap();
    assert_eq!(verifier.api_key(), API_KEY);
    assert_eq!(verifier.identity(), "alice");

    let claims = verifier.verify(API_SECRET).unwrap();
    assert_eq!(claims.iss, API_KEY);
    assert_eq!(claims.sub, "alice");
    assert!(claims.exp.is_some());

    let grants = claims.grants;
    assert_eq!(grants.identity, "alice");
    assert_eq!(grants.name, "Alice");
    let video = grants.video.unwrap();
    assert!(video.room_join);
    assert_eq!(video.room, "my-room");
    // unset tri-states keep their Go meaning
    assert!(video.can_publish());
    assert!(video.can_subscribe());
    assert!(video.can_publish_data());
    assert!(!video.can_update_own_metadata());
}

#[test]
fn reads_every_grant_the_go_server_writes() {
    let claims = ApiKeyTokenVerifier::parse(fixture("full_grants"))
        .unwrap()
        .verify(API_SECRET)
        .unwrap();
    let grants = claims.grants;

    assert_eq!(grants.identity, "bob");
    assert_eq!(grants.name, "Bob");
    assert_eq!(grants.kind, "agent");
    assert_eq!(grants.participant_kind(), participant_info::Kind::Agent);
    assert_eq!(grants.kind_details, vec!["forwarded".to_owned()]);
    assert_eq!(grants.metadata, "some-metadata");
    assert_eq!(grants.sha256, "abc123");
    assert_eq!(grants.room_preset, "preset-1");
    assert_eq!(
        grants.attributes,
        BTreeMap::from([
            ("seat".to_owned(), "12A".to_owned()),
            ("tier".to_owned(), "gold".to_owned()),
        ])
    );

    let video = grants.video.unwrap();
    assert!(video.room_create);
    assert!(video.room_list);
    assert!(video.room_record);
    assert!(video.room_admin);
    assert!(video.room_join);
    assert_eq!(video.room, "my-room");
    assert!(video.ingress_admin);
    assert!(video.hidden);
    assert!(video.recorder);
    assert!(video.agent);
    assert_eq!(video.destination_room, "other-room");
    assert_eq!(video.can_publish, Some(false));
    assert_eq!(video.can_subscribe, Some(true));
    assert_eq!(video.can_publish_data, Some(true));
    assert_eq!(video.can_update_own_metadata, Some(true));
    assert_eq!(video.can_subscribe_metrics, Some(true));
    assert_eq!(video.can_manage_agent_session, Some(true));
    assert_eq!(
        video.can_publish_sources,
        vec!["camera".to_owned(), "screen_share".to_owned()]
    );
    // can_publish is false, so no source may be published whatever the list says
    assert!(!video.can_publish_source(TrackSource::Camera));

    assert_eq!(
        grants.sip,
        Some(SipGrant {
            admin: true,
            call: true
        })
    );
    assert_eq!(
        grants.agent,
        Some(AgentGrant {
            admin: true,
            simulation_admin: true,
            database_admin: true,
            dispatch_admin: true,
        })
    );
    assert_eq!(grants.inference, Some(InferenceGrant { perform: true }));
    assert_eq!(
        grants.observability,
        Some(ObservabilityGrant { write: true })
    );
}

#[test]
fn reads_the_room_configuration_sub_message() {
    let claims = ApiKeyTokenVerifier::parse(fixture("room_config"))
        .unwrap()
        .verify(API_SECRET)
        .unwrap();

    let config = claims.grants.room_config.expect("roomConfig must decode");
    assert_eq!(config.name, "configured-room");
    assert_eq!(config.empty_timeout, 120);
    assert_eq!(config.departure_timeout, 30);
    assert_eq!(config.max_participants, 4);
    assert_eq!(config.agents.len(), 1);
    assert_eq!(config.agents[0].agent_name, "assistant");
}

#[test]
fn a_rust_token_carries_the_claims_the_go_server_writes() {
    // The Go join fixture's payload, minus the time claims, is the shape a
    // Rust-minted token must have for Go's verifier to read it the same way.
    let mint = AccessToken::new(API_KEY, API_SECRET)
        .with_identity("alice")
        .with_name("Alice")
        .with_video_grant(VideoGrant {
            room_join: true,
            room: "my-room".to_owned(),
            ..VideoGrant::default()
        })
        .to_jwt()
        .unwrap();

    assert_eq!(
        payload_without_times(&mint),
        payload_without_times(&fixture("join"))
    );

    // and it verifies with the Go-minted fixture's key
    let claims = ApiKeyTokenVerifier::parse(&mint)
        .unwrap()
        .verify(API_SECRET)
        .unwrap();
    assert_eq!(claims.grants.identity, "alice");
}

#[test]
fn a_token_signed_with_another_secret_is_rejected() {
    let verifier = ApiKeyTokenVerifier::parse(fixture("join")).unwrap();
    assert!(verifier.verify("wrong-secret").is_err());
}

#[test]
fn verification_goes_through_the_key_provider() {
    let provider = FileBasedKeyProvider::from_map(BTreeMap::from([(
        API_KEY.to_owned(),
        API_SECRET.to_owned(),
    )]));
    let grants = ApiKeyTokenVerifier::parse(fixture("join"))
        .unwrap()
        .verify_with(&provider)
        .unwrap();
    assert_eq!(grants.identity, "alice");

    let empty = FileBasedKeyProvider::default();
    let err = ApiKeyTokenVerifier::parse(fixture("join"))
        .unwrap()
        .verify_with(&empty)
        .unwrap_err();
    assert!(matches!(err, lk_auth::Error::UnknownApiKey(_)));
}

/// The token's payload with the time claims removed, so two tokens minted
/// seconds apart can be compared.
fn payload_without_times(token: &str) -> serde_json::Value {
    use base64::Engine as _;

    let payload = token.split('.').nth(1).expect("three segments");
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("base64url payload");
    let mut value: serde_json::Value = serde_json::from_slice(&decoded).expect("json payload");
    let object = value.as_object_mut().expect("json object");
    for claim in ["iat", "nbf", "exp"] {
        object.remove(claim);
    }
    value
}
