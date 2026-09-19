//! End-to-end tests for the signal endpoints.
//!
//! These run a real server on a real socket and talk to it with a real
//! WebSocket client, because the parts most likely to break compatibility are
//! the ones a unit test cannot see: the status a bad token gets *before* the
//! upgrade, the framing of the first response after it, and whether a ping is
//! answered without the room being involved.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine as _;
use lk_auth::{AccessToken, FileBasedKeyProvider, VideoGrant};
use lk_config::service::LimitConfig;
use lk_proto::livekit::{
    JoinRequest, JoinResponse, ParticipantInfo, Ping, Room, SignalRequest, SignalResponse,
    WrappedJoinRequest, signal_request, signal_response, wrapped_join_request,
};
use lk_service::connect::{ParticipantInit, RoomAllocator};
use lk_service::rtc_ws::{RtcState, SessionStarter, StartedSession};
use lk_service::server::ServerConfig;
use lk_service::{Error, NodeStats, Result, build_router};
use prost::Message as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

const API_KEY: &str = "devkey";
const API_SECRET: &str = "a-secret-that-is-at-least-32-characters";

// ---------------------------------------------------------------- test doubles

/// A room allocator that accepts everything, or refuses one named room.
struct TestAllocator {
    missing_room: Option<String>,
}

impl RoomAllocator for TestAllocator {
    fn validate_create_room<'a>(
        &'a self,
        room_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        let missing = self.missing_room.as_deref() == Some(room_name);
        Box::pin(async move {
            if missing {
                return Err(Error::RoomNotFound);
            }
            Ok(())
        })
    }

    fn select_room_node<'a>(
        &'a self,
        _room_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn region(&self) -> String {
        "test-region".to_owned()
    }
}

/// A session starter that answers with a join response and hands the test the
/// two channels, so a test can push a response at the client and watch the
/// requests the client sent arrive.
struct TestSessions {
    failures_before_success: AtomicUsize,
    observed: Arc<tokio::sync::Mutex<Vec<ParticipantInit>>>,
    channels: Arc<tokio::sync::Mutex<Option<TestChannels>>>,
}

struct TestChannels {
    requests: mpsc::Receiver<SignalRequest>,
    responses: mpsc::Sender<SignalResponse>,
}

impl TestSessions {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            failures_before_success: AtomicUsize::new(0),
            observed: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            channels: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }
}

impl SessionStarter for TestSessions {
    fn start_session<'a>(
        &'a self,
        room_name: String,
        init: ParticipantInit,
        _deadline: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<StartedSession>> + Send + 'a>> {
        Box::pin(async move {
            if self.failures_before_success.load(Ordering::SeqCst) > 0 {
                self.failures_before_success.fetch_sub(1, Ordering::SeqCst);
                return Err(Error::SessionStart("not yet".to_owned()));
            }

            self.observed.lock().await.push(init.clone());

            let (request_tx, request_rx) = mpsc::channel(16);
            let (response_tx, response_rx) = mpsc::channel(16);
            *self.channels.lock().await = Some(TestChannels {
                requests: request_rx,
                responses: response_tx,
            });

            Ok(StartedSession {
                connection_id: "CO_test".to_owned(),
                initial_response: SignalResponse {
                    message: Some(signal_response::Message::Join(JoinResponse {
                        room: Some(Room {
                            sid: "RM_test".to_owned(),
                            name: room_name,
                            ..Room::default()
                        }),
                        participant: Some(ParticipantInfo {
                            sid: "PA_test".to_owned(),
                            identity: init.identity.clone(),
                            ..ParticipantInfo::default()
                        }),
                        ..JoinResponse::default()
                    })),
                },
                requests: request_tx,
                responses: response_rx,
            })
        })
    }
}

// ------------------------------------------------------------------ harness

struct Harness {
    address: std::net::SocketAddr,
    sessions: Arc<TestSessions>,
    stats: NodeStats,
}

impl Harness {
    async fn start() -> Self {
        Self::start_with(None, LimitConfig::default()).await
    }

    async fn start_with(missing_room: Option<String>, limits: LimitConfig) -> Self {
        let sessions = TestSessions::new();
        let stats = NodeStats::new();
        stats.set_updated_at(now_seconds());

        let provider = FileBasedKeyProvider::from_map(BTreeMap::from([(
            API_KEY.to_owned(),
            API_SECRET.to_owned(),
        )]));

        let router = build_router(ServerConfig {
            rtc: RtcState {
                allocator: Arc::new(TestAllocator { missing_room }),
                sessions: sessions.clone(),
                limits,
                connect_attempts: 3,
            },
            key_provider: Arc::new(provider),
            node_stats: stats.clone(),
            max_request_body_size: 1024,
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Self {
            address,
            sessions,
            stats,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("ws://{}{path}", self.address)
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn join_token(room: &str, identity: &str) -> String {
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

/// A minimal HTTP/1.1 GET, so the tests need no HTTP client dependency.
async fn http_get(
    address: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();

    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    (status, body.to_owned())
}

// -------------------------------------------------------------------- tests

#[tokio::test]
async fn the_health_endpoint_reports_stale_node_stats() {
    let harness = Harness::start().await;

    let (status, body) = http_get(harness.address, "/", &[]).await;
    assert_eq!(status, 200);
    assert!(body.contains("OK"), "{body}");

    // four seconds is the cutoff, so five is stale
    harness.stats.set_updated_at(now_seconds() - 5);
    let (status, body) = http_get(harness.address, "/", &[]).await;
    assert_eq!(status, 406);
    assert!(body.contains("Not Ready"), "{body}");
}

#[tokio::test]
async fn validate_accepts_a_good_token_from_either_place() {
    let harness = Harness::start().await;
    let token = join_token("my-room", "alice");

    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/validate?access_token={token}"),
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "success");

    let (status, body) = http_get(
        harness.address,
        "/rtc/validate",
        &[("Authorization", &format!("Bearer {token}"))],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "success");
}

#[tokio::test]
async fn validate_refuses_what_the_go_server_refuses() {
    let harness = Harness::start().await;

    // no token at all: the request reaches the handler without grants
    let (status, body) = http_get(harness.address, "/rtc/validate", &[]).await;
    assert_eq!(status, 401);
    assert!(body.contains("permissions denied"), "{body}");

    // a token that does not parse
    let (status, _) = http_get(harness.address, "/rtc/validate?access_token=nonsense", &[]).await;
    assert_eq!(status, 401);

    // a token signed by a key this server does not know
    let other = AccessToken::new("otherkey", API_SECRET)
        .with_identity("alice")
        .with_video_grant(VideoGrant {
            room_join: true,
            room: "my-room".to_owned(),
            ..VideoGrant::default()
        })
        .to_jwt()
        .unwrap();
    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/validate?access_token={other}"),
        &[],
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("invalid API key"), "{body}");

    // a non-bearer Authorization header is refused rather than ignored
    let (status, _) = http_get(
        harness.address,
        "/rtc/validate",
        &[("Authorization", "Basic abc")],
    )
    .await;
    assert_eq!(status, 401);

    // a valid token without roomJoin
    let no_join = AccessToken::new(API_KEY, API_SECRET)
        .with_identity("alice")
        .with_video_grant(VideoGrant {
            room: "my-room".to_owned(),
            ..VideoGrant::default()
        })
        .to_jwt()
        .unwrap();
    let (status, _) = http_get(
        harness.address,
        &format!("/rtc/validate?access_token={no_join}"),
        &[],
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn validate_enforces_the_configured_limits() {
    let limits = LimitConfig {
        max_room_name_length: 8,
        max_participant_identity_length: 8,
        ..LimitConfig::default()
    };
    let harness = Harness::start_with(None, limits).await;

    let token = join_token("a-very-long-room-name", "alice");
    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/validate?access_token={token}"),
        &[],
    )
    .await;
    assert_eq!(status, 400);
    assert!(body.contains("room name exceeds limits"), "{body}");

    let token = join_token("room", "an-identity-that-is-too-long");
    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/validate?access_token={token}"),
        &[],
    )
    .await;
    assert_eq!(status, 400);
    assert!(body.contains("identity exceeds limits"), "{body}");
}

#[tokio::test]
async fn a_missing_room_is_a_404() {
    let harness = Harness::start_with(Some("gone".to_owned()), LimitConfig::default()).await;
    let token = join_token("gone", "alice");
    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/validate?access_token={token}"),
        &[],
    )
    .await;
    assert_eq!(status, 404);
    assert!(body.contains("does not exist"), "{body}");
}

#[tokio::test]
async fn a_plain_get_on_the_signal_endpoint_is_a_404() {
    let harness = Harness::start().await;
    let token = join_token("my-room", "alice");
    let (status, _) = http_get(
        harness.address,
        &format!("/rtc?access_token={token}&protocol=17&sdk=js"),
        &[],
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_leading_double_slash_still_routes() {
    let harness = Harness::start().await;
    let token = join_token("my-room", "alice");
    let (status, body) = http_get(
        harness.address,
        &format!("//rtc/validate?access_token={token}"),
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "success");
}

#[tokio::test]
async fn v1_requires_a_join_request_and_v0_does_not() {
    let harness = Harness::start().await;
    let token = join_token("my-room", "alice");

    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/v1/validate?access_token={token}"),
        &[],
    )
    .await;
    assert_eq!(status, 400);
    assert!(body.contains("join_request is required"), "{body}");

    let join_request = encode_join_request(
        &JoinRequest {
            metadata: "from-join-request".to_owned(),
            ..JoinRequest::default()
        },
        false,
    );
    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/v1/validate?access_token={token}&join_request={join_request}"),
        &[],
    )
    .await;
    // metadata needs canUpdateOwnMetadata, which this token does not have
    assert_eq!(status, 401, "{body}");

    let token = AccessToken::new(API_KEY, API_SECRET)
        .with_identity("alice")
        .with_video_grant(VideoGrant {
            room_join: true,
            room: "my-room".to_owned(),
            can_update_own_metadata: Some(true),
            ..VideoGrant::default()
        })
        .to_jwt()
        .unwrap();
    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/v1/validate?access_token={token}&join_request={join_request}"),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn a_gzipped_join_request_is_accepted_and_a_bomb_is_not() {
    let harness = Harness::start().await;
    let token = join_token("my-room", "alice");

    let gzipped = encode_join_request(
        &JoinRequest {
            participant_sid: "PA_resume".to_owned(),
            reconnect: true,
            ..JoinRequest::default()
        },
        true,
    );
    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/v1/validate?access_token={token}&join_request={gzipped}"),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{body}");

    // a small payload that expands past the 1 MiB decompressed limit
    let bomb = {
        use std::io::Write as _;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&vec![0u8; 4 << 20]).unwrap();
        let compressed = encoder.finish().unwrap();
        let wrapped = WrappedJoinRequest {
            compression: wrapped_join_request::Compression::Gzip as i32,
            join_request: compressed,
        };
        base64::engine::general_purpose::URL_SAFE.encode(wrapped.encode_to_vec())
    };
    let (status, body) = http_get(
        harness.address,
        &format!("/rtc/v1/validate?access_token={token}&join_request={bomb}"),
        &[],
    )
    .await;
    assert_eq!(status, 400);
    assert!(body.contains("join request too large"), "{body}");
}

#[tokio::test]
async fn a_signal_connection_joins_pings_and_carries_responses() {
    let harness = Harness::start().await;
    let token = join_token("my-room", "alice");

    let url = harness.url(&format!(
        "/rtc?access_token={token}&protocol=17&sdk=js&version=2.7.0&adaptive_stream=true"
    ));
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();

    // the first frame is the join response, in protobuf
    let initial = next_response(&mut socket).await;
    let Some(signal_response::Message::Join(join)) = initial.message else {
        panic!("the first response must be a join");
    };
    assert_eq!(join.room.as_ref().unwrap().name, "my-room");
    assert_eq!(join.participant.as_ref().unwrap().identity, "alice");

    // the parameters reached the room manager
    let observed = harness.sessions.observed.lock().await;
    let init = observed.first().expect("a session must have started");
    assert!(init.adaptive_stream);
    assert!(init.auto_subscribe, "auto_subscribe defaults to on");
    assert_eq!(init.region, "test-region");
    assert_eq!(init.client.as_ref().unwrap().protocol, 17);
    assert!(!init.use_single_peer_connection);
    drop(observed);

    // a ping is answered on the socket, without the room being involved
    send_request(
        &mut socket,
        SignalRequest {
            message: Some(signal_request::Message::Ping(1_234)),
        },
    )
    .await;
    let response = next_response(&mut socket).await;
    assert!(matches!(
        response.message,
        Some(signal_response::Message::Pong(_))
    ));

    send_request(
        &mut socket,
        SignalRequest {
            message: Some(signal_request::Message::PingReq(Ping {
                timestamp: 99,
                rtt: 0,
            })),
        },
    )
    .await;
    let response = next_response(&mut socket).await;
    let Some(signal_response::Message::PongResp(pong)) = response.message else {
        panic!("a ping request must be answered with a pong response");
    };
    assert_eq!(pong.last_ping_timestamp, 99);

    // requests still reach the room, and responses come back
    let mut guard = harness.sessions.channels.lock().await;
    let channels = guard.as_mut().expect("the session must have channels");
    let forwarded = channels.requests.recv().await.expect("ping forwarded");
    assert!(matches!(
        forwarded.message,
        Some(signal_request::Message::Ping(1_234))
    ));

    channels
        .responses
        .send(SignalResponse {
            message: Some(signal_response::Message::Pong(7)),
        })
        .await
        .unwrap();
    drop(guard);

    let response = next_response(&mut socket).await;
    assert_eq!(response.message, Some(signal_response::Message::Pong(7)));
}

#[tokio::test]
async fn a_closed_response_source_closes_the_socket() {
    let harness = Harness::start().await;
    let token = join_token("my-room", "alice");
    let url = harness.url(&format!("/rtc?access_token={token}&protocol=17&sdk=js"));
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let _ = next_response(&mut socket).await;

    // the participant closing ends the signal connection
    harness.sessions.channels.lock().await.take();

    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        use futures_util::StreamExt as _;
        loop {
            match socket.next().await {
                None => return true,
                Some(Ok(tungstenite::Message::Close(_))) => return true,
                Some(Ok(_)) => {}
                Some(Err(_)) => return true,
            }
        }
    })
    .await
    .expect("the socket must close when the participant goes away");
    assert!(closed);
}

#[tokio::test]
async fn a_session_start_is_retried_before_the_connection_fails() {
    let harness = Harness::start().await;
    harness
        .sessions
        .failures_before_success
        .store(2, Ordering::SeqCst);

    let token = join_token("my-room", "alice");
    let url = harness.url(&format!("/rtc?access_token={token}&protocol=17&sdk=js"));
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();

    // two attempts failed, the third produced the join response
    let initial = next_response(&mut socket).await;
    assert!(matches!(
        initial.message,
        Some(signal_response::Message::Join(_))
    ));
}

#[tokio::test]
async fn a_session_that_never_starts_fails_the_request_before_the_upgrade() {
    let harness = Harness::start().await;
    harness
        .sessions
        .failures_before_success
        .store(100, Ordering::SeqCst);

    let token = join_token("my-room", "alice");
    let url = harness.url(&format!("/rtc?access_token={token}&protocol=17&sdk=js"));
    let error = tokio_tungstenite::connect_async(url).await.unwrap_err();
    match error {
        tungstenite::Error::Http(response) => assert_eq!(response.status().as_u16(), 500),
        other => panic!("expected an HTTP error before the upgrade, got {other:?}"),
    }
}

#[tokio::test]
async fn a_json_client_is_answered_in_json() {
    let harness = Harness::start().await;
    let token = join_token("my-room", "alice");
    let url = harness.url(&format!("/rtc?access_token={token}&protocol=17&sdk=js"));
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let _ = next_response(&mut socket).await;

    use futures_util::SinkExt as _;
    let request = SignalRequest {
        message: Some(signal_request::Message::Ping(5)),
    };
    socket
        .send(tungstenite::Message::Text(
            serde_json::to_string(&request).unwrap().into(),
        ))
        .await
        .unwrap();

    // the answer comes back as text, because the client spoke text
    use futures_util::StreamExt as _;
    let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let tungstenite::Message::Text(text) = frame else {
        panic!("a json client must be answered with text frames");
    };
    let response: SignalResponse = serde_json::from_str(&text).unwrap();
    assert!(matches!(
        response.message,
        Some(signal_response::Message::Pong(_))
    ));
}

// --------------------------------------------------------------- ws helpers

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn next_response(socket: &mut Socket) -> SignalResponse {
    use futures_util::StreamExt as _;

    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a response must arrive")
            .expect("the socket must stay open")
            .expect("the frame must be readable");
        match frame {
            tungstenite::Message::Binary(payload) => {
                return SignalResponse::decode(payload.as_ref()).expect("a protobuf response");
            }
            tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

async fn send_request(socket: &mut Socket, request: SignalRequest) {
    use futures_util::SinkExt as _;
    socket
        .send(tungstenite::Message::Binary(request.encode_to_vec().into()))
        .await
        .unwrap();
}

fn encode_join_request(join: &JoinRequest, gzip: bool) -> String {
    let payload = join.encode_to_vec();
    let wrapped = if gzip {
        use std::io::Write as _;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&payload).unwrap();
        WrappedJoinRequest {
            compression: wrapped_join_request::Compression::Gzip as i32,
            join_request: encoder.finish().unwrap(),
        }
    } else {
        WrappedJoinRequest {
            compression: wrapped_join_request::Compression::None as i32,
            join_request: payload,
        }
    };
    base64::engine::general_purpose::URL_SAFE.encode(wrapped.encode_to_vec())
}
