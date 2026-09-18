//! The `/rtc` and `/rtc/v1` signal endpoints.
//!
//! Ports `pkg/service/rtcservice.go`. The shape of the handshake is the part
//! that matters for compatibility:
//!
//! 1. The request is validated **before** the WebSocket upgrade, so a client
//!    with a bad token gets an HTTP status it can act on rather than a socket
//!    that closes.
//! 2. The session is started with a deadline of `3 + attempt` seconds, retried
//!    `signal_relay.connect_attempts` times. The client gives up at its own
//!    deadline, so a slower server-side retry would look like a hang.
//! 3. The first response is written immediately after the upgrade. The SDKs
//!    wait for it before they consider the connection established.
//! 4. `Ping` and `PingReq` are answered on the socket without going near the
//!    room, so a busy room cannot make a healthy connection look dead.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, FromRequestParts as _, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use lk_config::service::LimitConfig;
use lk_proto::livekit::{Pong, SignalRequest, SignalResponse, signal_request, signal_response};
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval, timeout};

use crate::auth::Grants;
use crate::client_info::parse_client_info;
use crate::connect::{
    ConnectParams, ConnectRequestParams, ParticipantInit, RoomAllocator, ValidatedConnect,
    decode_attributes, decode_join_request, participant_init_from_join_request,
    participant_init_from_params, validate_connect_request,
};
use crate::error::{Error, Result};
use crate::ws::{PING_FREQUENCY_SECONDS, SignalCodec};

/// A session the room manager started for a participant.
pub struct StartedSession {
    /// The connection id, for log correlation across nodes.
    pub connection_id: String,
    /// The response the client is waiting for: a `JoinResponse`, or a
    /// `ReconnectResponse` when resuming.
    pub initial_response: SignalResponse,
    /// Where the client's requests go.
    pub requests: mpsc::Sender<SignalRequest>,
    /// Where the participant's responses come from.
    pub responses: mpsc::Receiver<SignalResponse>,
}

/// Starts a participant's session. Implemented by the room manager.
pub trait SessionStarter: Send + Sync + 'static {
    /// Starts a session, or fails within `deadline`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SessionStart`] when the room or the participant could
    /// not be created; the handler retries on a fresh attempt.
    fn start_session<'a>(
        &'a self,
        room_name: String,
        init: ParticipantInit,
        deadline: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<StartedSession>> + Send + 'a>>;
}

/// What the signal endpoints need to serve a connection.
#[derive(Clone)]
pub struct RtcState {
    /// Room placement and creation checks.
    pub allocator: Arc<dyn RoomAllocator>,
    /// The room manager.
    pub sessions: Arc<dyn SessionStarter>,
    /// Size limits, including the signalling frame limit.
    pub limits: LimitConfig,
    /// How many times a session start is retried, from
    /// `signal_relay.connect_attempts`.
    pub connect_attempts: i32,
}

/// `GET /rtc`: the signal WebSocket, with every setting as a query parameter.
pub async fn rtc_v0(State(state): State<RtcState>, request: Request) -> Response {
    let mut parts = RequestParts::extract(request).await;
    serve(state, &mut parts, false).await
}

/// `GET /rtc/v1`: the signal WebSocket, with the settings in a join request.
pub async fn rtc_v1(State(state): State<RtcState>, request: Request) -> Response {
    let mut parts = RequestParts::extract(request).await;
    serve(state, &mut parts, true).await
}

/// `GET /rtc/validate`: the same validation without the socket.
pub async fn rtc_v0_validate(State(state): State<RtcState>, request: Request) -> Response {
    let parts = RequestParts::extract(request).await;
    validate(&state, &parts, false)
}

/// `GET /rtc/v1/validate`: the same, for the join-request form.
pub async fn rtc_v1_validate(State(state): State<RtcState>, request: Request) -> Response {
    let parts = RequestParts::extract(request).await;
    validate(&state, &parts, true)
}

/// The pieces of a request the signal endpoints read.
///
/// They are pulled out by hand rather than through extractor arguments so the
/// upgrade stays optional: a plain `GET /rtc` must answer 404, not a rejection
/// from the extractor.
struct RequestParts {
    upgrade: Option<WebSocketUpgrade>,
    uri: Uri,
    headers: HeaderMap,
    peer: Option<String>,
    grants: Option<Grants>,
}

impl RequestParts {
    async fn extract(request: Request) -> Self {
        let (mut parts, _body) = request.into_parts();
        let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &())
            .await
            .ok();
        let grants = parts.extensions.get::<Grants>().cloned();
        let peer = parts
            .extensions
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map(|ConnectInfo(address)| address.to_string());
        Self {
            upgrade,
            uri: parts.uri.clone(),
            headers: parts.headers.clone(),
            peer,
            grants,
        }
    }
}

fn validate(state: &RtcState, parts: &RequestParts, needs_join_request: bool) -> Response {
    match validate_internal(state, parts, needs_join_request, true) {
        // the Go handler answers with this exact body, and the SDKs check it
        Ok(_) => (StatusCode::OK, "success").into_response(),
        Err(err) => err.into_response(),
    }
}

/// The validation both endpoints share.
///
/// `strict` rejects an undecodable `attributes` parameter rather than ignoring
/// it: a real connection should not fail over an attribute the participant can
/// set again, but `/rtc/validate` exists to tell a developer what is wrong.
fn validate_internal(
    state: &RtcState,
    parts: &RequestParts,
    needs_join_request: bool,
    strict: bool,
) -> Result<(String, ParticipantInit)> {
    let params = ConnectParams::parse(parts.uri.query().unwrap_or_default());
    let peer = parts.peer.clone();
    let headers = &parts.headers;
    let grants = parts.grants.clone();

    let encoded_join_request = params.get("join_request").to_owned();
    let mut request_params = ConnectRequestParams {
        room_name: params.get("room").to_owned(),
        ..ConnectRequestParams::default()
    };

    let join_request = if encoded_join_request.is_empty() {
        if needs_join_request {
            return Err(Error::BadRequest("join_request is required"));
        }
        request_params.publish = params.get("publish").to_owned();
        let attributes = params.get("attributes");
        if !attributes.is_empty() {
            match decode_attributes(attributes) {
                Ok(attributes) => request_params.attributes = attributes,
                Err(err) if strict => return Err(err),
                // a connection is not worth failing over an attribute the
                // participant can set again over the signal channel
                Err(_) => {}
            }
        }
        None
    } else {
        let join = decode_join_request(&encoded_join_request)?;
        request_params.metadata = join.metadata.clone();
        request_params.attributes = join
            .participant_attributes
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        Some(join)
    };

    let validated: ValidatedConnect = validate_connect_request(
        grants.as_ref(),
        &state.limits,
        &request_params,
        state.allocator.as_ref(),
    )?;

    let init = match join_request {
        None => {
            let client = parse_client_info(&params.raw, headers, peer.as_deref());
            participant_init_from_params(&params, &validated, client)
        }
        Some(join) => {
            let mut client = join.client_info.clone().unwrap_or_default();
            crate::client_info::augment_client_info(&mut client, headers, peer.as_deref());
            participant_init_from_join_request(join, &validated, client)
        }
    };

    Ok((validated.room_name.clone(), init))
}

async fn serve(state: RtcState, parts: &mut RequestParts, needs_join_request: bool) -> Response {
    // a plain GET on the signal endpoint is not an error worth explaining
    let Some(upgrade) = parts.upgrade.take() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let (room_name, init) = match validate_internal(&state, parts, needs_join_request, false) {
        Ok(validated) => validated,
        Err(err) => return err.into_response(),
    };

    let mut session = None;
    let mut last_error = None;
    for attempt in 0..state.connect_attempts.max(1) {
        // the client's own deadline grows the same way, so a longer wait here
        // would be a hang rather than a retry
        let deadline = Duration::from_secs(3 + attempt as u64);
        match timeout(
            deadline,
            state
                .sessions
                .start_session(room_name.clone(), init.clone(), deadline),
        )
        .await
        {
            Ok(Ok(started)) => {
                session = Some(started);
                break;
            }
            Ok(Err(err)) => last_error = Some(err),
            Err(_) => last_error = Some(Error::SessionStart("timed out".to_owned())),
        }
    }

    let Some(session) = session else {
        return last_error
            .unwrap_or_else(|| Error::SessionStart("no attempts made".to_owned()))
            .into_response();
    };

    let limit = state.limits.signal_message_size_limit;
    upgrade.on_upgrade(move |socket| serve_socket(socket, session, limit))
}

/// The connection's lifetime: write the initial response, then pump requests
/// one way and responses the other until either side stops.
async fn serve_socket(mut socket: WebSocket, session: StartedSession, message_size_limit: i64) {
    let StartedSession {
        connection_id,
        initial_response,
        requests,
        mut responses,
    } = session;

    let mut codec = SignalCodec::new(message_size_limit);

    let initial = match codec.encode_response(&initial_response) {
        Ok(frame) => frame,
        Err(err) => {
            tracing::error!(%connection_id, %err, "could not encode initial response");
            return;
        }
    };
    if socket.send(initial).await.is_err() {
        tracing::debug!(%connection_id, "client went away before the initial response");
        return;
    }

    let mut ping = interval(Duration::from_secs(PING_FREQUENCY_SECONDS));
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // the first tick fires immediately, and a ping before the client has read
    // the join response is noise
    ping.tick().await;

    loop {
        tokio::select! {
            frame = socket.recv() => {
                let Some(frame) = frame else {
                    tracing::debug!(%connection_id, "client closed the signal connection");
                    break;
                };
                let Ok(frame) = frame else {
                    tracing::debug!(%connection_id, "signal connection read failed");
                    break;
                };
                match codec.decode_request(&frame) {
                    Ok(Some(request)) => {
                        if !handle_request(&mut socket, &codec, &requests, request).await {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(%connection_id, %err, "dropping undecodable signal frame");
                    }
                }
            }

            response = responses.recv() => {
                let Some(response) = response else {
                    // the participant closed, which ends the signal connection
                    tracing::debug!(%connection_id, "response source closed");
                    break;
                };
                match codec.encode_response(&response) {
                    Ok(frame) => {
                        if socket.send(frame).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => tracing::error!(%connection_id, %err, "could not encode response"),
                }
            }

            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    let _ = socket.send(Message::Close(None)).await;
}

/// Answers a request, either on the socket or by handing it to the
/// participant. Returns false when the connection should end.
async fn handle_request(
    socket: &mut WebSocket,
    codec: &SignalCodec,
    requests: &mpsc::Sender<SignalRequest>,
    request: SignalRequest,
) -> bool {
    // Ping and PingReq are answered here rather than in the room: they exist to
    // tell the client the connection is alive, so routing them through a busy
    // participant would defeat them.
    let answer = match &request.message {
        Some(signal_request::Message::Ping(_)) => Some(SignalResponse {
            // milliseconds, not nanoseconds: some clients overflow on the
            // nanosecond value
            message: Some(signal_response::Message::Pong(unix_millis())),
        }),
        Some(signal_request::Message::PingReq(ping)) => Some(SignalResponse {
            message: Some(signal_response::Message::PongResp(Pong {
                last_ping_timestamp: ping.timestamp,
                timestamp: unix_millis(),
            })),
        }),
        _ => None,
    };

    if let Some(answer) = answer {
        match codec.encode_response(&answer) {
            Ok(frame) => {
                if socket.send(frame).await.is_err() {
                    return false;
                }
            }
            Err(err) => tracing::error!(%err, "could not encode pong"),
        }
    }

    requests.send(request).await.is_ok()
}

/// Milliseconds since the Unix epoch.
fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
