//! Single-node session tests.
//!
//! These wire the real pieces together — HTTP server, authentication, signal
//! endpoints, room manager, room allocator and the local store — and drive them
//! with a WebSocket client. What they check is what a client experiences:
//! joining, seeing other participants, being evicted by a second connection
//! with the same identity, resuming, and being told to start over when the
//! server has never heard of the session it is resuming.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use lk_auth::{AccessToken, FileBasedKeyProvider, VideoGrant};
use lk_config::Config;
use lk_proto::livekit::{
    AddTrackRequest, JoinRequest, SignalRequest, SignalResponse, TrackType, WrappedJoinRequest,
    signal_request, signal_response, wrapped_join_request,
};
use lk_service::room_allocator::StandardRoomAllocator;
use lk_service::rtc_ws::RtcState;
use lk_service::server::ServerConfig;
use lk_service::store::LocalStore;
use lk_service::{NodeStats, RoomManager, build_router};
use prost::Message as _;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite;

const API_KEY: &str = "devkey";
const API_SECRET: &str = "a-secret-that-is-at-least-32-characters";

struct Harness {
    address: std::net::SocketAddr,
    manager: Arc<RoomManager>,
}

impl Harness {
    async fn start() -> Self {
        Self::start_with(Config::default()).await
    }

    async fn start_with(mut config: Config) -> Self {
        config
            .keys
            .insert(API_KEY.to_owned(), API_SECRET.to_owned());
        config.finalize();
        let config = Arc::new(config);

        let store = Arc::new(LocalStore::new());
        let allocator = Arc::new(StandardRoomAllocator::new(config.clone(), store.clone()));
        let manager = Arc::new(RoomManager::new(
            config.clone(),
            store.clone(),
            allocator.clone(),
        ));

        let provider = FileBasedKeyProvider::from_map(BTreeMap::from([(
            API_KEY.to_owned(),
            API_SECRET.to_owned(),
        )]));

        let stats = NodeStats::new();
        stats.set_updated_at(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64),
        );

        let router = build_router(ServerConfig {
            rtc: RtcState {
                allocator,
                sessions: manager.clone(),
                limits: config.limit.clone(),
                connect_attempts: config.signal_relay.connect_attempts,
            },
            key_provider: Arc::new(provider),
            node_stats: stats,
            max_request_body_size: config.limit.max_api_request_body_size,
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Self { address, manager }
    }

    fn url(&self, query: &str) -> String {
        format!("ws://{}/rtc?{query}", self.address)
    }

    fn v1_url(&self, query: &str) -> String {
        format!("ws://{}/rtc/v1?{query}", self.address)
    }
}

fn token(room: &str, identity: &str) -> String {
    AccessToken::new(API_KEY, API_SECRET)
        .with_identity(identity)
        .with_video_grant(VideoGrant {
            room_join: true,
            room: room.to_owned(),
            ..VideoGrant::default()
        })
        .to_jwt()
        .unwrap()
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn connect(url: String) -> Socket {
    let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    socket
}

async fn next_response(socket: &mut Socket) -> Option<SignalResponse> {
    use futures_util::StreamExt as _;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(deadline, socket.next())
            .await
            .ok()??;
        match frame.ok()? {
            tungstenite::Message::Binary(payload) => {
                return SignalResponse::decode(payload.as_ref()).ok();
            }
            tungstenite::Message::Close(_) => return None,
            _ => {}
        }
    }
}

async fn next_of<F, T>(socket: &mut Socket, mut want: F) -> Option<T>
where
    F: FnMut(SignalResponse) -> Option<T>,
{
    for _ in 0..32 {
        let response = next_response(socket).await?;
        if let Some(value) = want(response) {
            return Some(value);
        }
    }
    None
}

#[tokio::test]
async fn two_participants_join_and_see_each_other() {
    let harness = Harness::start().await;

    let mut alice = connect(harness.url(&format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "alice")
    )))
    .await;

    let join = next_of(&mut alice, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("alice must get a join response");
    assert_eq!(join.room.as_ref().unwrap().name, "my-room");
    assert_eq!(join.participant.as_ref().unwrap().identity, "alice");
    assert!(join.other_participants.is_empty());
    assert!(!join.room.as_ref().unwrap().sid.is_empty());

    let mut bob = connect(harness.url(&format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "bob")
    )))
    .await;
    let join = next_of(&mut bob, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("bob must get a join response");
    let others: Vec<&str> = join
        .other_participants
        .iter()
        .map(|p| p.identity.as_str())
        .collect();
    assert_eq!(others, vec!["alice"]);

    // alice is told about bob
    let update = next_of(&mut alice, |response| match response.message {
        Some(signal_response::Message::Update(update)) => update
            .participants
            .into_iter()
            .find(|info| info.identity == "bob"),
        _ => None,
    })
    .await;
    assert!(update.is_some(), "alice must hear about bob joining");

    // and the room knows about both
    let room = harness.manager.room("my-room").await.expect("the room");
    let identities: Vec<String> = room
        .participants()
        .await
        .into_iter()
        .map(|info| info.identity)
        .collect();
    assert_eq!(identities, vec!["alice".to_owned(), "bob".to_owned()]);
}

#[tokio::test]
async fn a_second_connection_with_the_same_identity_evicts_the_first() {
    let harness = Harness::start().await;
    let credentials = format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "alice")
    );

    let mut first = connect(harness.url(&credentials)).await;
    let first_join = next_of(&mut first, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("the first connection must join");
    let first_sid = first_join.participant.as_ref().unwrap().sid.clone();

    let mut second = connect(harness.url(&credentials)).await;
    let second_join = next_of(&mut second, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("the second connection must join");
    let second_sid = second_join.participant.as_ref().unwrap().sid.clone();
    assert_ne!(
        first_sid, second_sid,
        "the eviction must mint a new session"
    );

    // the room holds exactly one alice, the new one
    let room = harness.manager.room("my-room").await.expect("the room");
    let infos = room.participants().await;
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].sid, second_sid);
}

#[tokio::test]
async fn a_reconnect_for_an_unknown_session_is_told_to_start_over() {
    let harness = Harness::start().await;

    let mut socket = connect(harness.url(&format!(
        "access_token={}&protocol=17&sdk=js&reconnect=1&sid=PA_nosuchthing",
        token("my-room", "alice")
    )))
    .await;

    let leave = next_of(&mut socket, |response| match response.message {
        Some(signal_response::Message::Leave(leave)) => Some(leave),
        _ => None,
    })
    .await
    .expect("an unknown session must be told to reconnect");
    assert_eq!(
        leave.reason,
        lk_proto::livekit::DisconnectReason::StateMismatch as i32
    );
    assert_eq!(
        leave.action,
        lk_proto::livekit::leave_request::Action::Reconnect as i32
    );
}

#[tokio::test]
async fn a_reconnect_resumes_the_existing_session() {
    let harness = Harness::start().await;
    let credentials = format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "alice")
    );

    let mut first = connect(harness.url(&credentials)).await;
    let join = next_of(&mut first, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("the first connection must join");
    let sid = join.participant.as_ref().unwrap().sid.clone();

    let mut resumed = connect(harness.url(&format!("{credentials}&reconnect=1&sid={sid}"))).await;
    let reconnect = next_of(&mut resumed, |response| match response.message {
        Some(signal_response::Message::Reconnect(reconnect)) => Some(reconnect),
        _ => None,
    })
    .await
    .expect("a resume must be answered with a reconnect response");
    assert!(reconnect.server_info.is_some());

    // the same session is still in the room, with the same sid
    let room = harness.manager.room("my-room").await.expect("the room");
    let infos = room.participants().await;
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].sid, sid);
}

#[tokio::test]
async fn the_replaced_connection_does_not_evict_the_resumed_session() {
    // The connection a resume replaces ends right after the resume, and the
    // participant it was serving must survive that: otherwise every resume
    // races its own predecessor's teardown.
    let harness = Harness::start().await;
    let credentials = format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "alice")
    );

    let mut first = connect(harness.url(&credentials)).await;
    let join = next_of(&mut first, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("the first connection must join");
    let sid = join.participant.as_ref().unwrap().sid.clone();

    let mut resumed = connect(harness.url(&format!("{credentials}&reconnect=1&sid={sid}"))).await;
    next_of(&mut resumed, |response| match response.message {
        Some(signal_response::Message::Reconnect(reconnect)) => Some(reconnect),
        _ => None,
    })
    .await
    .expect("the resume must be answered");

    // the old connection goes away, as it does in the field
    drop(first);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let room = harness.manager.room("my-room").await.expect("the room");
    let infos = room.participants().await;
    assert_eq!(infos.len(), 1, "the resumed session must still be here");
    assert_eq!(infos[0].sid, sid);

    // and the resumed connection still receives responses
    let participant = room.participant("alice").await.expect("the participant");
    participant
        .send(SignalResponse {
            message: Some(signal_response::Message::Pong(11)),
        })
        .await;
    let pong = next_of(&mut resumed, |response| match response.message {
        Some(signal_response::Message::Pong(value)) => Some(value),
        _ => None,
    })
    .await;
    assert_eq!(pong, Some(11));
}

#[tokio::test]
async fn tracks_sent_with_the_join_request_are_kept_for_the_transport() {
    let harness = Harness::start().await;

    let join_request = JoinRequest {
        add_track_requests: vec![AddTrackRequest {
            cid: "camera-cid".to_owned(),
            name: "camera".to_owned(),
            r#type: TrackType::Video as i32,
            ..AddTrackRequest::default()
        }],
        ..JoinRequest::default()
    };
    let wrapped = WrappedJoinRequest {
        compression: wrapped_join_request::Compression::None as i32,
        join_request: join_request.encode_to_vec(),
    };
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::URL_SAFE.encode(wrapped.encode_to_vec());

    let mut socket = connect(harness.v1_url(&format!(
        "access_token={}&join_request={encoded}",
        token("my-room", "alice")
    )))
    .await;
    let join = next_of(&mut socket, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("the v1 connection must join");
    assert_eq!(join.participant.as_ref().unwrap().identity, "alice");

    // the add-track request is held, not dropped: the client is waiting for a
    // published track and will keep waiting until the transport answers
    let room = harness.manager.room("my-room").await.expect("the room");
    let participant = room.participant("alice").await.expect("the participant");
    let pending = participant.pending_requests().await;
    assert_eq!(pending.len(), 1);
    assert!(matches!(
        pending[0].message,
        Some(signal_request::Message::AddTrack(_))
    ));
}

#[tokio::test]
async fn a_media_request_over_the_socket_is_kept_too() {
    let harness = Harness::start().await;
    let mut socket = connect(harness.url(&format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "alice")
    )))
    .await;
    let _join = next_of(&mut socket, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("the connection must join");

    use futures_util::SinkExt as _;
    let request = SignalRequest {
        message: Some(signal_request::Message::AddTrack(AddTrackRequest {
            cid: "mic-cid".to_owned(),
            r#type: TrackType::Audio as i32,
            ..AddTrackRequest::default()
        })),
    };
    socket
        .send(tungstenite::Message::Binary(request.encode_to_vec().into()))
        .await
        .unwrap();

    let room = harness.manager.room("my-room").await.expect("the room");
    let participant = room.participant("alice").await.expect("the participant");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let pending = participant.pending_requests().await;
        if !pending.is_empty() {
            assert!(matches!(
                pending[0].message,
                Some(signal_request::Message::AddTrack(_))
            ));
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the request was lost");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_closed_signal_connection_removes_the_participant() {
    let harness = Harness::start().await;
    let mut socket = connect(harness.url(&format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "alice")
    )))
    .await;
    let _join = next_of(&mut socket, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("the connection must join");

    drop(socket);

    let room = harness.manager.room("my-room").await.expect("the room");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if room.participants().await.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the participant must be removed when its signal connection goes"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_room_that_does_not_exist_is_refused_when_auto_create_is_off() {
    let mut config = Config::default();
    config.room.auto_create = false;
    let harness = Harness::start_with(config).await;

    let error = tokio_tungstenite::connect_async(harness.url(&format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "alice")
    )))
    .await
    .unwrap_err();
    match error {
        tungstenite::Error::Http(response) => assert_eq!(response.status().as_u16(), 404),
        other => panic!("expected a 404 before the upgrade, got {other:?}"),
    }
}

#[tokio::test]
async fn room_settings_come_from_the_config() {
    let mut config = Config::default();
    config.room.empty_timeout = 42;
    config.room.departure_timeout = 7;
    config.room.max_participants = 3;
    let harness = Harness::start_with(config).await;

    let mut socket = connect(harness.url(&format!(
        "access_token={}&protocol=17&sdk=js",
        token("my-room", "alice")
    )))
    .await;
    let join = next_of(&mut socket, |response| match response.message {
        Some(signal_response::Message::Join(join)) => Some(join),
        _ => None,
    })
    .await
    .expect("the connection must join");

    let room = join.room.unwrap();
    assert_eq!(room.empty_timeout, 42);
    assert_eq!(room.departure_timeout, 7);
    assert_eq!(room.max_participants, 3);
    // the enabled codec list is the configured one
    assert_eq!(room.enabled_codecs.len(), 10);
}
