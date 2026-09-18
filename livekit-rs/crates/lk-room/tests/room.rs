//! Room and participant actor tests.
//!
//! The behaviour under test is what a client sees: who appears in a join
//! response, who is told about a join or a leave, which changes are refused,
//! and when an idle room closes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::time::Duration;

use lk_auth::{ClaimGrants, VideoGrant};
use lk_config::service::LimitConfig;
use lk_proto::livekit::{
    ParticipantInfo, ParticipantPermission, Room as RoomProto, RoomInternal, ServerInfo,
    SignalRequest, SignalResponse, UpdateParticipantMetadata, participant_info, request_response,
    signal_request, signal_response,
};
use lk_room::client_info::ClientInfoExt;
use lk_room::participant::{CloseReason, ParticipantHandle, ParticipantParams};
use lk_room::protocol_version::ProtocolVersion;
use lk_room::room::{self, JoinParams, RoomCloseReason, RoomHandle, RoomParams};
use tokio::sync::mpsc;

fn room_proto(name: &str) -> RoomProto {
    RoomProto {
        sid: room::new_room_sid(),
        name: name.to_owned(),
        creation_time: now_seconds(),
        empty_timeout: 300,
        departure_timeout: 20,
        ..RoomProto::default()
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn start_room(proto: RoomProto) -> RoomHandle {
    room::spawn(RoomParams {
        proto,
        internal: RoomInternal::default(),
        server_info: ServerInfo {
            version: "1.0.0-rust".to_owned(),
            region: "test".to_owned(),
            ..ServerInfo::default()
        },
    })
}

fn grants(identity: &str, video: VideoGrant) -> ClaimGrants {
    ClaimGrants {
        identity: identity.to_owned(),
        name: identity.to_owned(),
        video: Some(video),
        ..ClaimGrants::default()
    }
}

struct Client {
    participant: ParticipantHandle,
    responses: mpsc::Receiver<SignalResponse>,
    join_response: Box<lk_proto::livekit::JoinResponse>,
}

async fn join(room: &RoomHandle, identity: &str, video: VideoGrant) -> Client {
    join_with_limits(room, identity, video, LimitConfig::default()).await
}

async fn join_with_limits(
    room: &RoomHandle,
    identity: &str,
    video: VideoGrant,
    limits: LimitConfig,
) -> Client {
    let (responses, receiver) = mpsc::channel(64);
    let joined = room
        .join(JoinParams {
            participant: ParticipantParams {
                identity: identity.to_owned(),
                name: identity.to_owned(),
                sid: room::new_participant_sid(),
                grants: grants(identity, video),
                token_expires_at: None,
                protocol: ProtocolVersion(17),
                client: ClientInfoExt::default(),
                client_configuration: None,
                region: "test".to_owned(),
                adaptive_stream: false,
                limits,
                responses,
                // replaced by the room with its own channel
                events: mpsc::channel(1).0,
            },
            ice_servers: Vec::new(),
        })
        .await
        .expect("join must succeed");

    Client {
        participant: joined.participant,
        responses: receiver,
        join_response: joined.join_response,
    }
}

/// The next participant update a client receives, or `None` within the
/// timeout.
///
/// A participant hears about its own state changes too, as it does in Go, so a
/// test looking for another participant filters rather than taking the first.
async fn next_update(client: &mut Client) -> Option<Vec<ParticipantInfo>> {
    let response = tokio::time::timeout(Duration::from_secs(2), client.responses.recv())
        .await
        .ok()??;
    match response.message {
        Some(signal_response::Message::Update(update)) => Some(update.participants),
        _ => None,
    }
}

/// The next update mentioning `identity`, ignoring updates about anyone else.
async fn next_update_about(client: &mut Client, identity: &str) -> Option<ParticipantInfo> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let Some(update) = next_update(client).await else {
            continue;
        };
        if let Some(info) = update.into_iter().find(|info| info.identity == identity) {
            return Some(info);
        }
    }
    None
}

/// Every update a client has received so far, drained without waiting.
fn drain_updates(client: &mut Client) -> Vec<ParticipantInfo> {
    let mut out = Vec::new();
    while let Ok(response) = client.responses.try_recv() {
        if let Some(signal_response::Message::Update(update)) = response.message {
            out.extend(update.participants);
        }
    }
    out
}

#[tokio::test]
async fn a_join_response_carries_the_room_and_the_other_participants() {
    let room = start_room(room_proto("my-room"));

    let first = join(&room, "alice", VideoGrant::default()).await;
    assert_eq!(first.join_response.room.as_ref().unwrap().name, "my-room");
    assert_eq!(
        first.join_response.participant.as_ref().unwrap().identity,
        "alice"
    );
    assert!(first.join_response.other_participants.is_empty());
    assert_eq!(first.join_response.ping_interval, 5);
    assert_eq!(first.join_response.ping_timeout, 15);
    assert_eq!(
        first.join_response.server_info.as_ref().unwrap().version,
        "1.0.0-rust"
    );

    let second = join(&room, "bob", VideoGrant::default()).await;
    let others: Vec<&str> = second
        .join_response
        .other_participants
        .iter()
        .map(|p| p.identity.as_str())
        .collect();
    assert_eq!(others, vec!["alice"]);
}

#[tokio::test]
async fn joining_and_leaving_are_broadcast_to_everyone_else() {
    let room = start_room(room_proto("my-room"));
    let mut alice = join(&room, "alice", VideoGrant::default()).await;
    let bob = join(&room, "bob", VideoGrant::default()).await;

    // alice hears about bob joining
    let info = next_update_about(&mut alice, "bob")
        .await
        .expect("a join update about bob");
    assert!(info.state <= participant_info::State::Joined as i32);

    // and about bob leaving
    room.remove_participant(
        bob.participant.identity(),
        bob.participant.sid(),
        CloseReason::ClientRequestLeave,
    )
    .await;
    let info = loop {
        let info = next_update_about(&mut alice, "bob")
            .await
            .expect("a leave update about bob");
        if info.state == participant_info::State::Disconnected as i32 {
            break info;
        }
    };
    assert_eq!(info.identity, "bob");

    assert_eq!(room.participants().await.len(), 1);
}

#[tokio::test]
async fn a_hidden_participant_is_not_announced_to_others() {
    let room = start_room(room_proto("my-room"));
    let mut alice = join(&room, "alice", VideoGrant::default()).await;

    let hidden = VideoGrant {
        hidden: true,
        ..VideoGrant::default()
    };
    let _recorder = join(&room, "recorder", hidden).await;

    // alice hears about her own state changes, but never about the hidden
    // participant
    tokio::time::sleep(Duration::from_millis(50)).await;
    let seen: Vec<String> = drain_updates(&mut alice)
        .into_iter()
        .map(|info| info.identity)
        .collect();
    assert!(
        !seen.iter().any(|identity| identity == "recorder"),
        "{seen:?}"
    );

    // and does not see it in a later join response either
    let third = join(&room, "carol", VideoGrant::default()).await;
    let others: Vec<&str> = third
        .join_response
        .other_participants
        .iter()
        .map(|p| p.identity.as_str())
        .collect();
    assert_eq!(others, vec!["alice"]);
}

#[tokio::test]
async fn a_duplicate_identity_is_refused() {
    let room = start_room(room_proto("my-room"));
    let _alice = join(&room, "alice", VideoGrant::default()).await;

    let (responses, _receiver) = mpsc::channel(4);
    let result = room
        .join(JoinParams {
            participant: ParticipantParams {
                identity: "alice".to_owned(),
                name: "alice".to_owned(),
                sid: room::new_participant_sid(),
                grants: grants("alice", VideoGrant::default()),
                token_expires_at: None,
                protocol: ProtocolVersion(17),
                client: ClientInfoExt::default(),
                client_configuration: None,
                region: String::new(),
                adaptive_stream: false,
                limits: LimitConfig::default(),
                responses,
                events: mpsc::channel(1).0,
            },
            ice_servers: Vec::new(),
        })
        .await;
    assert_eq!(result.err(), Some(lk_room::room::JoinError::AlreadyJoined));
}

#[tokio::test]
async fn a_full_room_refuses_the_next_participant() {
    let mut proto = room_proto("my-room");
    proto.max_participants = 1;
    let room = start_room(proto);

    let _alice = join(&room, "alice", VideoGrant::default()).await;

    let (responses, _receiver) = mpsc::channel(4);
    let result = room
        .join(JoinParams {
            participant: ParticipantParams {
                identity: "bob".to_owned(),
                name: "bob".to_owned(),
                sid: room::new_participant_sid(),
                grants: grants("bob", VideoGrant::default()),
                token_expires_at: None,
                protocol: ProtocolVersion(17),
                client: ClientInfoExt::default(),
                client_configuration: None,
                region: String::new(),
                adaptive_stream: false,
                limits: LimitConfig::default(),
                responses,
                events: mpsc::channel(1).0,
            },
            ice_servers: Vec::new(),
        })
        .await;
    assert_eq!(
        result.err(),
        Some(lk_room::room::JoinError::MaxParticipantsExceeded)
    );
}

#[tokio::test]
async fn a_participant_may_change_its_own_metadata_only_with_the_grant() {
    let room = start_room(room_proto("my-room"));
    let mut alice = join(&room, "alice", VideoGrant::default()).await;

    // no canUpdateOwnMetadata: the request is refused, and the client is told
    alice
        .participant
        .handle_signal(SignalRequest {
            message: Some(signal_request::Message::UpdateMetadata(
                UpdateParticipantMetadata {
                    metadata: "nope".to_owned(),
                    request_id: 7,
                    ..UpdateParticipantMetadata::default()
                },
            )),
        })
        .await;

    let answer = loop {
        let response = tokio::time::timeout(Duration::from_secs(2), alice.responses.recv())
            .await
            .expect("an answer must arrive")
            .expect("the channel must stay open");
        if let Some(signal_response::Message::RequestResponse(answer)) = response.message {
            break answer;
        }
    };
    assert_eq!(answer.request_id, 7);
    assert_eq!(answer.reason, request_response::Reason::NotAllowed as i32);

    // with the grant it goes through, and the room broadcasts it
    let allowed = VideoGrant {
        can_update_own_metadata: Some(true),
        ..VideoGrant::default()
    };
    let bob = join(&room, "bob", allowed).await;
    bob.participant
        .handle_signal(SignalRequest {
            message: Some(signal_request::Message::UpdateMetadata(
                UpdateParticipantMetadata {
                    metadata: "hello".to_owned(),
                    attributes: BTreeMap::from([("seat".to_owned(), "12A".to_owned())])
                        .into_iter()
                        .collect(),
                    request_id: 8,
                    ..UpdateParticipantMetadata::default()
                },
            )),
        })
        .await;

    let info = wait_for(|| async {
        room.participants()
            .await
            .into_iter()
            .find(|p| p.identity == "bob" && p.metadata == "hello")
    })
    .await;
    assert_eq!(info.metadata, "hello");
    assert_eq!(info.attributes.get("seat").map(String::as_str), Some("12A"));
}

#[tokio::test]
async fn oversized_metadata_is_refused() {
    let room = start_room(room_proto("my-room"));
    let limits = LimitConfig {
        max_metadata_size: 4,
        ..LimitConfig::default()
    };
    let allowed = VideoGrant {
        can_update_own_metadata: Some(true),
        ..VideoGrant::default()
    };
    let alice = join_with_limits(&room, "alice", allowed, limits).await;

    let result = alice
        .participant
        .update_metadata(None, Some("far too long".to_owned()), BTreeMap::new())
        .await;
    assert_eq!(
        result.err(),
        Some(lk_room::participant::UpdateError::MetadataExceedsLimits)
    );
}

#[tokio::test]
async fn a_permission_change_reaches_the_participant_and_the_room() {
    let room = start_room(room_proto("my-room"));
    let alice = join(&room, "alice", VideoGrant::default()).await;

    let permission = ParticipantPermission {
        can_publish: false,
        can_subscribe: true,
        can_publish_data: false,
        ..ParticipantPermission::default()
    };
    let info = room
        .update_permission("alice", permission.clone())
        .await
        .expect("the participant must exist");
    assert_eq!(info.permission, Some(permission));

    let current = alice.participant.info().await.unwrap();
    assert!(!current.permission.unwrap().can_publish);
}

#[tokio::test]
async fn a_leave_request_closes_the_participant() {
    let room = start_room(room_proto("my-room"));
    let alice = join(&room, "alice", VideoGrant::default()).await;

    alice
        .participant
        .handle_signal(SignalRequest {
            message: Some(signal_request::Message::Leave(
                lk_proto::livekit::LeaveRequest::default(),
            )),
        })
        .await;

    wait_for(|| async { room.participants().await.is_empty().then_some(()) }).await;
}

#[tokio::test]
async fn room_metadata_changes_reach_every_participant() {
    let room = start_room(room_proto("my-room"));
    let mut alice = join(&room, "alice", VideoGrant::default()).await;

    room.set_metadata("season 2".to_owned()).await;

    let update = loop {
        let response = tokio::time::timeout(Duration::from_secs(2), alice.responses.recv())
            .await
            .unwrap()
            .unwrap();
        if let Some(signal_response::Message::RoomUpdate(update)) = response.message {
            break update;
        }
    };
    assert_eq!(update.room.unwrap().metadata, "season 2");
}

#[tokio::test(start_paused = true)]
async fn an_empty_room_closes_after_its_timeout() {
    let mut proto = room_proto("my-room");
    proto.empty_timeout = 10;
    proto.departure_timeout = 5;
    let room = start_room(proto);

    // let the actor arm its timer before the clock moves
    tokio::task::yield_now().await;

    // nobody joins: the empty timeout applies
    tokio::time::advance(Duration::from_secs(11)).await;
    wait_for(|| async { room.to_proto().await.is_none().then_some(()) }).await;
}

#[tokio::test(start_paused = true)]
async fn a_room_everyone_left_closes_after_the_departure_timeout() {
    let mut proto = room_proto("my-room");
    proto.empty_timeout = 3_600;
    proto.departure_timeout = 20;
    let room = start_room(proto);

    let alice = join(&room, "alice", VideoGrant::default()).await;
    alice
        .participant
        .close(CloseReason::ClientRequestLeave)
        .await;
    // let the close event reach the room
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    // the departure timeout is the shorter one, and it is what applies
    tokio::time::advance(Duration::from_secs(21)).await;
    wait_for(|| async { room.to_proto().await.is_none().then_some(()) }).await;
}

#[tokio::test]
async fn closing_a_room_closes_its_participants() {
    let room = start_room(room_proto("my-room"));
    let alice = join(&room, "alice", VideoGrant::default()).await;

    room.close(RoomCloseReason::ServiceRequestDeleteRoom).await;

    // the participant actor is gone, so its handle answers nothing
    wait_for(|| async { alice.participant.info().await.is_none().then_some(()) }).await;
    assert!(!room.is_open());
}

/// Polls until `f` yields a value, with a deadline, for the places where a
/// change travels through more than one actor.
async fn wait_for<F, Fut, T>(mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = f().await {
            return value;
        }
        assert!(std::time::Instant::now() < deadline, "timed out waiting");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
