//! Starting and resuming participant sessions.
//!
//! Ports `RoomManager.StartSession` from `pkg/service/roommanager.go`, the Go
//! server's critical path: create-or-join the room, decide between a fresh
//! session, a resume and a duplicate-identity eviction, build the participant,
//! and keep the signal connection pumping into it.
//!
//! Scoped to one node with the local store. The Redis-backed path, the psrpc
//! topic registration and the analytics are phase 4 work; the media parameters
//! the Go constructor threads through arrive with the transport.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use lk_auth::AccessToken;
use lk_config::Config;
use lk_proto::livekit::{
    IceServer, LeaveRequest, ReconnectResponse, ServerInfo, SignalRequest, SignalResponse,
    leave_request, participant_info, server_info, signal_request, signal_response,
};
use lk_proto::utils::guid;
use lk_room::client_config::StaticClientConfigurationManager;
use lk_room::client_info::ClientInfoExt;
use lk_room::participant::{CloseReason, ParticipantHandle, ParticipantParams};
use lk_room::protocol_version::{CURRENT_PROTOCOL, ProtocolVersion};
use lk_room::room::{self, JoinError, JoinParams, RoomHandle, RoomParams};
use tokio::sync::{Mutex, mpsc};

use crate::connect::ParticipantInit;
use crate::error::{Error, Result};
use crate::room_allocator::StandardRoomAllocator;
use crate::rtc_ws::{SessionStarter, StartedSession};
use crate::store::ObjectStore;

/// How often a participant's token is refreshed, from the Go server's
/// `tokenRefreshInterval`.
pub const TOKEN_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How many responses a participant queues towards its signal connection.
const RESPONSE_QUEUE_DEPTH: usize = 256;

/// Runs the rooms on this node.
pub struct RoomManager {
    config: Arc<Config>,
    store: Arc<dyn ObjectStore>,
    allocator: Arc<StandardRoomAllocator>,
    client_config: StaticClientConfigurationManager,
    server_info: ServerInfo,
    rooms: Mutex<BTreeMap<String, RoomHandle>>,
}

impl RoomManager {
    /// A room manager over `store`.
    #[must_use]
    pub fn new(
        config: Arc<Config>,
        store: Arc<dyn ObjectStore>,
        allocator: Arc<StandardRoomAllocator>,
    ) -> Self {
        let server_info = ServerInfo {
            edition: server_info::Edition::Standard as i32,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: CURRENT_PROTOCOL,
            region: config.region.clone(),
            node_id: guid::new_guid(guid::NODE_PREFIX),
            ..ServerInfo::default()
        };
        Self {
            config,
            store,
            allocator,
            client_config: StaticClientConfigurationManager,
            server_info,
            rooms: Mutex::new(BTreeMap::new()),
        }
    }

    /// The rooms this node is running.
    pub async fn room(&self, name: &str) -> Option<RoomHandle> {
        let rooms = self.rooms.lock().await;
        rooms.get(name).cloned()
    }

    /// Creates the room if it does not exist, and starts its actor.
    ///
    /// # Errors
    ///
    /// Returns whatever the allocator or the store returns.
    pub async fn get_or_create_room(
        &self,
        create_room: &lk_proto::livekit::CreateRoomRequest,
    ) -> Result<RoomHandle> {
        {
            let rooms = self.rooms.lock().await;
            if let Some(handle) = rooms.get(&create_room.name)
                && handle.is_open()
            {
                return Ok(handle.clone());
            }
        }

        let (proto, internal, _created) = self.allocator.create_room(create_room).await?;

        let mut rooms = self.rooms.lock().await;
        // another task may have created it while this one waited on the store
        if let Some(handle) = rooms.get(&create_room.name)
            && handle.is_open()
        {
            return Ok(handle.clone());
        }

        let handle = room::spawn(RoomParams {
            proto,
            internal,
            server_info: self.server_info.clone(),
        });
        rooms.insert(create_room.name.clone(), handle.clone());
        Ok(handle)
    }

    /// Drops rooms whose actors have stopped.
    pub async fn reap_closed_rooms(&self) {
        let mut rooms = self.rooms.lock().await;
        let closed: Vec<String> = rooms
            .iter()
            .filter(|(_, handle)| !handle.is_open())
            .map(|(name, _)| name.clone())
            .collect();
        for name in closed {
            rooms.remove(&name);
            let _ = self.store.delete_room(&name).await;
        }
    }

    /// The first API key and secret, for minting refreshed tokens and TURN
    /// credentials.
    fn first_key_pair(&self) -> Option<(String, String)> {
        self.config
            .keys
            .iter()
            .next()
            .map(|(key, secret)| (key.clone(), secret.clone()))
    }

    async fn start_session_inner(
        &self,
        room_name: String,
        init: ParticipantInit,
    ) -> Result<StartedSession> {
        let grants = init.grants.clone().ok_or(Error::PermissionDenied)?;

        if !self
            .config
            .limit
            .check_metadata_size(&grants.claims.metadata)
        {
            return Err(Error::BadRequest("metadata exceeds limits"));
        }
        if !self.config.limit.check_attributes_size(
            grants
                .claims
                .attributes
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        ) {
            return Err(Error::BadRequest("attributes exceed limits"));
        }

        let create_room =
            init.create_room
                .clone()
                .unwrap_or(lk_proto::livekit::CreateRoomRequest {
                    name: room_name.clone(),
                    ..lk_proto::livekit::CreateRoomRequest::default()
                });
        let room = self.get_or_create_room(&create_room).await?;

        let (responses, response_rx) = mpsc::channel(RESPONSE_QUEUE_DEPTH);
        let (requests, request_rx) = mpsc::channel(RESPONSE_QUEUE_DEPTH);

        let existing = room.participant(&init.identity).await;
        let client = ClientInfoExt::new(init.client.clone());
        // A fresh session starts at epoch zero; a resume takes the epoch the
        // participant hands back when it accepts the new connection.
        let mut session_epoch = 0;
        let protocol = ProtocolVersion(init.client.as_ref().map_or(0, |info| info.protocol));

        let (participant, initial_response) = match existing {
            // A reconnect for a participant that is still here: keep the
            // session and point it at the new signal connection.
            Some(participant) if init.reconnect => {
                let Some(epoch) = participant.replace_responses(responses.clone()).await else {
                    // the participant closed while this was in flight, which
                    // is the state mismatch the client has to recover from
                    return Err(Error::SessionStart(
                        "cannot resume a closed participant".to_owned(),
                    ));
                };
                session_epoch = epoch;
                let response = SignalResponse {
                    message: Some(signal_response::Message::Reconnect(ReconnectResponse {
                        ice_servers: self.ice_servers(),
                        client_configuration: init
                            .client
                            .as_ref()
                            .and_then(|info| self.client_config.configuration(info)),
                        server_info: Some(self.server_info.clone()),
                        ..ReconnectResponse::default()
                    })),
                };
                (participant, response)
            }
            // A duplicate identity: the newcomer wins, as in Go, because the
            // old session is by definition the one that is no longer being
            // used by the client.
            Some(participant) => {
                room.remove_participant(
                    participant.identity(),
                    participant.sid(),
                    CloseReason::DuplicateIdentity,
                )
                .await;
                self.join_participant(&room, &init, &client, protocol, responses.clone())
                    .await?
            }
            // A reconnect for a participant this node has never heard of: the
            // client's state does not match the server's, so it is told to
            // start over rather than left waiting.
            None if init.reconnect => {
                let leave = if protocol.supports_regions_in_leave_request() {
                    LeaveRequest {
                        reason: lk_proto::livekit::DisconnectReason::StateMismatch as i32,
                        action: leave_request::Action::Reconnect as i32,
                        ..LeaveRequest::default()
                    }
                } else {
                    LeaveRequest {
                        can_reconnect: true,
                        reason: lk_proto::livekit::DisconnectReason::StateMismatch as i32,
                        ..LeaveRequest::default()
                    }
                };
                return Ok(StartedSession {
                    connection_id: guid::new_guid(guid::CONNECTION_PREFIX),
                    initial_response: SignalResponse {
                        message: Some(signal_response::Message::Leave(leave)),
                    },
                    requests,
                    responses: response_rx,
                });
            }
            None => {
                self.join_participant(&room, &init, &client, protocol, responses.clone())
                    .await?
            }
        };

        if let Some(info) = participant.info().await {
            let _ = self.store.store_participant(&room_name, info).await;
        }

        self.spawn_session_worker(
            room.clone(),
            participant.clone(),
            request_rx,
            grants.clone(),
            session_epoch,
        );

        Ok(StartedSession {
            connection_id: guid::new_guid(guid::CONNECTION_PREFIX),
            initial_response,
            requests,
            responses: response_rx,
        })
    }

    async fn join_participant(
        &self,
        room: &RoomHandle,
        init: &ParticipantInit,
        client: &ClientInfoExt,
        protocol: ProtocolVersion,
        responses: mpsc::Sender<SignalResponse>,
    ) -> Result<(ParticipantHandle, SignalResponse)> {
        let grants = init.grants.clone().ok_or(Error::PermissionDenied)?;
        let client_configuration = init
            .client
            .as_ref()
            .and_then(|info| self.client_config.configuration(info));

        // The v1 join request can carry tracks and a publisher offer that the
        // client sent before the session existed. They are handed to the
        // participant so they are answered once the transport is up, rather
        // than dropped.
        let mut pending_requests = Vec::new();
        for add_track in &init.add_track_requests {
            pending_requests.push(SignalRequest {
                message: Some(signal_request::Message::AddTrack(add_track.clone())),
            });
        }
        if let Some(offer) = &init.publisher_offer {
            pending_requests.push(SignalRequest {
                message: Some(signal_request::Message::Offer(offer.clone())),
            });
        }

        let joined = room
            .join(JoinParams {
                participant: ParticipantParams {
                    identity: init.identity.clone(),
                    name: init.name.clone(),
                    sid: room::new_participant_sid(),
                    grants: grants.claims.clone(),
                    token_expires_at: init.token_expires_at,
                    protocol,
                    client: client.clone(),
                    client_configuration,
                    region: init.region.clone(),
                    adaptive_stream: init.adaptive_stream,
                    limits: self.config.limit.clone(),
                    responses,
                    // replaced by the room
                    events: mpsc::channel(1).0,
                    pending_requests,
                },
                ice_servers: self.ice_servers(),
            })
            .await
            .map_err(|err| match err {
                JoinError::MaxParticipantsExceeded => Error::LimitExceeded,
                other => Error::SessionStart(other.to_string()),
            })?;

        Ok((
            joined.participant,
            SignalResponse {
                message: Some(signal_response::Message::Join(*joined.join_response)),
            },
        ))
    }

    /// The ICE servers a client is told about.
    ///
    /// Only the configured external servers for now: the embedded TURN server
    /// and its per-participant credentials are phase 4 work, and advertising a
    /// server this node does not run would cost the client a failed candidate.
    fn ice_servers(&self) -> Vec<IceServer> {
        self.config
            .rtc
            .turn_servers
            .iter()
            .filter(|server| !server.host.is_empty())
            .map(|server| IceServer {
                urls: vec![format!(
                    "turn:{}:{}?transport={}",
                    server.host,
                    server.port,
                    if server.protocol.is_empty() {
                        "udp"
                    } else {
                        &server.protocol
                    }
                )],
                username: server.username.clone(),
                credential: server.credential.clone(),
            })
            .collect()
    }

    /// Pumps the signal connection's requests into the participant, and
    /// refreshes its token while it is connected.
    fn spawn_session_worker(
        &self,
        room: RoomHandle,
        participant: ParticipantHandle,
        mut requests: mpsc::Receiver<SignalRequest>,
        grants: crate::auth::Grants,
        session_epoch: u64,
    ) {
        let key_pair = self.first_key_pair();
        tokio::spawn(async move {
            let mut refresh = tokio::time::interval(TOKEN_REFRESH_INTERVAL);
            refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            refresh.tick().await;

            loop {
                tokio::select! {
                    request = requests.recv() => {
                        let Some(request) = request else {
                            // the signal connection went away; the participant
                            // stays until the room removes it, so a resume can
                            // still find it
                            break;
                        };
                        if !participant.handle_signal(request).await {
                            break;
                        }
                    }
                    _ = refresh.tick() => {
                        if let Some((api_key, secret)) = &key_pair
                            && let Some(token) = refreshed_token(api_key, secret, &grants)
                        {
                            participant.refresh_token(token).await;
                        }
                    }
                }
            }

            // This connection is done. It only takes the participant with it
            // when it is still the current one: a resume replaces the
            // connection while keeping the session, and the connection being
            // replaced must not evict its own replacement.
            if participant.session_epoch().await != Some(session_epoch) {
                return;
            }

            participant
                .set_state(participant_info::State::Disconnected)
                .await;
            room.remove_participant(
                participant.identity(),
                participant.sid(),
                CloseReason::SignalClosed,
            )
            .await;
        });
    }
}

/// Mints a fresh token from the grants the participant already holds, so a
/// long session does not end when its original token expires.
fn refreshed_token(api_key: &str, secret: &str, grants: &crate::auth::Grants) -> Option<String> {
    let mut token = AccessToken::new(api_key, secret);
    *token.grants_mut() = grants.claims.clone();
    token.to_jwt().ok()
}

impl SessionStarter for RoomManager {
    fn start_session<'a>(
        &'a self,
        room_name: String,
        init: ParticipantInit,
        _deadline: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<StartedSession>> + Send + 'a>> {
        Box::pin(async move { self.start_session_inner(room_name, init).await })
    }
}
