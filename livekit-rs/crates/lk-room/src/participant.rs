//! The participant actor.
//!
//! Ports the control-plane half of `pkg/rtc/participant*.go`. The Go
//! `ParticipantImpl` carries around 25 atomics, a listener lock and a typed
//! operations queue because everything that touches a participant can arrive
//! from a different goroutine. Here one task owns the state and everything
//! arrives as a message, which is the design the port plan calls for
//! (section 6): no lock ordering to get wrong, and no callback can re-enter the
//! participant while it holds its own state.
//!
//! # What is here
//!
//! Identity, grants, the state machine, metadata and permission changes, and
//! the signal requests that need none of the media plane. Publishing,
//! subscribing and negotiation arrive with the transport in the `PCTransport`
//! issue; a media request is logged and dropped rather than half-answered,
//! because a wrong answer would leave a client waiting on a track that will
//! never come.

use std::collections::BTreeMap;

use lk_auth::ClaimGrants;
use lk_config::service::LimitConfig;
use lk_proto::livekit::{
    ClientConfiguration, DisconnectReason, ParticipantInfo, ParticipantPermission, RequestResponse,
    SignalRequest, SignalResponse, TimedVersion, participant_info, request_response,
    signal_request, signal_response,
};
use lk_proto::utils::TimedVersionGenerator;
use tokio::sync::{mpsc, oneshot};

use crate::client_info::ClientInfoExt;
use crate::protocol_version::ProtocolVersion;

/// How many commands a participant queues before its sender waits.
const COMMAND_QUEUE_DEPTH: usize = 128;

/// How many events a participant queues towards its room.
pub const EVENT_QUEUE_DEPTH: usize = 64;

/// Why a participant was closed.
///
/// Ports the `ParticipantCloseReason` table. The reason decides what the client
/// is told, and whether it should reconnect, so it is an enum rather than a
/// string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The client asked to leave.
    ClientRequestLeave,
    /// Another connection took this identity.
    DuplicateIdentity,
    /// The room closed.
    RoomClosed,
    /// The room manager removed the participant.
    ServiceRequestRemoveParticipant,
    /// The signal connection went away.
    SignalClosed,
    /// The participant never finished joining.
    JoinTimeout,
    /// The participant's state did not match the server's.
    StateMismatch,
}

impl CloseReason {
    /// The disconnect reason the client is given.
    #[must_use]
    pub const fn disconnect_reason(self) -> DisconnectReason {
        match self {
            Self::ClientRequestLeave => DisconnectReason::ClientInitiated,
            Self::DuplicateIdentity => DisconnectReason::DuplicateIdentity,
            Self::RoomClosed => DisconnectReason::RoomClosed,
            Self::ServiceRequestRemoveParticipant => DisconnectReason::ParticipantRemoved,
            Self::SignalClosed | Self::JoinTimeout => DisconnectReason::ClientInitiated,
            Self::StateMismatch => DisconnectReason::StateMismatch,
        }
    }
}

/// What a participant needs to exist.
pub struct ParticipantParams {
    /// The participant's identity, from the token.
    pub identity: String,
    /// The participant's display name.
    pub name: String,
    /// The generated participant sid.
    pub sid: String,
    /// The verified grants, which carry the permissions.
    pub grants: ClaimGrants,
    /// The token's expiry, as seconds since the Unix epoch.
    pub token_expires_at: Option<i64>,
    /// The client's protocol version.
    pub protocol: ProtocolVersion,
    /// What the client said about itself.
    pub client: ClientInfoExt,
    /// The codec restrictions this client gets.
    pub client_configuration: Option<ClientConfiguration>,
    /// The region this node is in.
    pub region: String,
    /// Whether the client adapts its subscriptions to what is on screen.
    pub adaptive_stream: bool,
    /// Size limits.
    pub limits: LimitConfig,
    /// Where responses to this participant go.
    pub responses: mpsc::Sender<SignalResponse>,
    /// Where the participant reports changes.
    pub events: mpsc::Sender<ParticipantEvent>,
}

/// Something the room needs to know about a participant.
#[derive(Clone, Debug)]
pub enum ParticipantEvent {
    /// The participant's info changed and the room should broadcast it.
    Changed {
        /// The participant's identity.
        identity: String,
        /// The new info.
        info: Box<ParticipantInfo>,
    },
    /// The participant closed.
    Closed {
        /// The participant's identity.
        identity: String,
        /// The participant's sid, so a stale close cannot evict the session
        /// that replaced it.
        sid: String,
        /// Why it closed.
        reason: CloseReason,
    },
}

/// What the room needs from a participant to build its join response and to
/// decide how to treat it.
#[derive(Clone, Debug)]
pub struct JoinContext {
    /// The participant's info, as other participants will see it.
    pub info: ParticipantInfo,
    /// The codec restrictions this client gets, from the client-configuration
    /// rules.
    pub client_configuration: Option<ClientConfiguration>,
    /// Whether the client drives the subscriber connection as primary.
    pub subscriber_as_primary: bool,
    /// Whether the participant may publish at all.
    pub can_publish: bool,
    /// Whether the client adapts its subscriptions to what is on screen.
    pub adaptive_stream: bool,
    /// The region this node is in.
    pub region: String,
    /// The token's expiry, as seconds since the Unix epoch.
    pub token_expires_at: Option<i64>,
    /// The client's protocol version.
    pub protocol: ProtocolVersion,
    /// Whether the participant is hidden from others.
    pub hidden: bool,
    /// Whether the participant joined as a recorder.
    pub recorder: bool,
}

/// A message to a participant.
enum Command {
    Signal(Box<SignalRequest>),
    JoinContext(oneshot::Sender<Box<JoinContext>>),
    Send(Box<SignalResponse>),
    Info(oneshot::Sender<ParticipantInfo>),
    SetState(participant_info::State),
    UpdateMetadata {
        name: Option<String>,
        metadata: Option<String>,
        attributes: BTreeMap<String, String>,
        reply: oneshot::Sender<Result<ParticipantInfo, UpdateError>>,
    },
    UpdatePermission {
        permission: Box<ParticipantPermission>,
        reply: oneshot::Sender<ParticipantInfo>,
    },
    RefreshToken(String),
    Close(CloseReason),
}

/// Why an update was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// The participant may not change its own metadata.
    #[error("permissions denied")]
    PermissionDenied,
    /// The metadata is larger than `limit.max_metadata_size`.
    #[error("metadata exceeds limits")]
    MetadataExceedsLimits,
    /// The attributes are larger than `limit.max_attributes_size`.
    #[error("attributes exceed limits")]
    AttributesExceedLimits,
    /// The name is longer than `limit.max_participant_name_length`.
    #[error("participant name exceeds limits")]
    NameExceedsLimits,
    /// The participant has closed.
    #[error("participant is gone")]
    Gone,
}

/// A handle to a participant actor.
#[derive(Clone, Debug)]
pub struct ParticipantHandle {
    identity: String,
    sid: String,
    commands: mpsc::Sender<Command>,
}

impl ParticipantHandle {
    /// The participant's identity.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The participant's sid.
    #[must_use]
    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// Hands the participant a signal request from its client.
    pub async fn handle_signal(&self, request: SignalRequest) -> bool {
        self.commands
            .send(Command::Signal(Box::new(request)))
            .await
            .is_ok()
    }

    /// Sends a response to the participant's client.
    pub async fn send(&self, response: SignalResponse) -> bool {
        self.commands
            .send(Command::Send(Box::new(response)))
            .await
            .is_ok()
    }

    /// What the room needs to build this participant's join response.
    pub async fn join_context(&self) -> Option<JoinContext> {
        let (reply, answer) = oneshot::channel();
        self.commands.send(Command::JoinContext(reply)).await.ok()?;
        answer.await.ok().map(|context| *context)
    }

    /// The participant's current info, or `None` once it has closed.
    pub async fn info(&self) -> Option<ParticipantInfo> {
        let (reply, answer) = oneshot::channel();
        self.commands.send(Command::Info(reply)).await.ok()?;
        answer.await.ok()
    }

    /// Moves the participant's state machine forward.
    pub async fn set_state(&self, state: participant_info::State) {
        let _ = self.commands.send(Command::SetState(state)).await;
    }

    /// Applies a metadata, name or attribute change the participant asked for.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateError`] when the participant may not make the change or
    /// the change is over a limit.
    pub async fn update_metadata(
        &self,
        name: Option<String>,
        metadata: Option<String>,
        attributes: BTreeMap<String, String>,
    ) -> Result<ParticipantInfo, UpdateError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::UpdateMetadata {
                name,
                metadata,
                attributes,
                reply,
            })
            .await
            .map_err(|_| UpdateError::Gone)?;
        answer.await.map_err(|_| UpdateError::Gone)?
    }

    /// Applies a permission change from the room service.
    pub async fn update_permission(
        &self,
        permission: ParticipantPermission,
    ) -> Option<ParticipantInfo> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::UpdatePermission {
                permission: Box::new(permission),
                reply,
            })
            .await
            .ok()?;
        answer.await.ok()
    }

    /// Hands the participant a refreshed token, as the room manager does
    /// before the current one expires.
    pub async fn refresh_token(&self, token: String) {
        let _ = self.commands.send(Command::RefreshToken(token)).await;
    }

    /// Closes the participant.
    pub async fn close(&self, reason: CloseReason) {
        let _ = self.commands.send(Command::Close(reason)).await;
    }
}

/// Starts a participant actor and returns its handle.
#[must_use]
pub fn spawn(params: ParticipantParams) -> ParticipantHandle {
    let (commands, receiver) = mpsc::channel(COMMAND_QUEUE_DEPTH);
    let handle = ParticipantHandle {
        identity: params.identity.clone(),
        sid: params.sid.clone(),
        commands,
    };
    tokio::spawn(async move { Participant::new(params).run(receiver).await });
    handle
}

struct Participant {
    info: ParticipantInfo,
    grants: ClaimGrants,
    token_expires_at: Option<i64>,
    protocol: ProtocolVersion,
    client: ClientInfoExt,
    client_configuration: Option<ClientConfiguration>,
    region: String,
    adaptive_stream: bool,
    limits: LimitConfig,
    responses: mpsc::Sender<SignalResponse>,
    events: mpsc::Sender<ParticipantEvent>,
    versions: TimedVersionGenerator,
    closed: bool,
}

impl Participant {
    fn new(params: ParticipantParams) -> Self {
        let versions = TimedVersionGenerator::new();
        let permission = params
            .grants
            .video
            .as_ref()
            .map(lk_auth::VideoGrant::to_permission);
        let version = versions.next();
        let info = ParticipantInfo {
            sid: params.sid,
            identity: params.identity,
            name: params.name,
            state: participant_info::State::Joining as i32,
            metadata: params.grants.metadata.clone(),
            attributes: params
                .grants
                .attributes
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            joined_at: unix_seconds(),
            joined_at_ms: unix_millis(),
            permission,
            region: params.region.clone(),
            kind: params.grants.participant_kind() as i32,
            kind_details: params
                .grants
                .kind_details()
                .into_iter()
                .map(|detail| detail as i32)
                .collect(),
            version: version_as_u32(version),
            ..ParticipantInfo::default()
        };

        Self {
            info,
            grants: params.grants,
            token_expires_at: params.token_expires_at,
            protocol: params.protocol,
            client: params.client,
            client_configuration: params.client_configuration,
            region: params.region,
            adaptive_stream: params.adaptive_stream,
            limits: params.limits,
            responses: params.responses,
            events: params.events,
            versions,
            closed: false,
        }
    }

    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        while let Some(command) = commands.recv().await {
            match command {
                Command::Signal(request) => self.handle_signal(*request).await,
                Command::JoinContext(reply) => {
                    let _ = reply.send(Box::new(self.join_context()));
                }
                Command::Send(response) => {
                    let _ = self.responses.send(*response).await;
                }
                Command::Info(reply) => {
                    let _ = reply.send(self.info.clone());
                }
                Command::SetState(state) => self.set_state(state).await,
                Command::UpdateMetadata {
                    name,
                    metadata,
                    attributes,
                    reply,
                } => {
                    let result = self
                        .update_metadata(name, metadata, attributes, false)
                        .await;
                    let _ = reply.send(result);
                }
                Command::UpdatePermission { permission, reply } => {
                    self.update_permission(*permission).await;
                    let _ = reply.send(self.info.clone());
                }
                Command::RefreshToken(token) => {
                    let _ = self
                        .responses
                        .send(SignalResponse {
                            message: Some(signal_response::Message::RefreshToken(token)),
                        })
                        .await;
                }
                Command::Close(reason) => {
                    self.close(reason).await;
                    break;
                }
            }
        }

        if !self.closed {
            // the command channel went away with the signal connection
            self.close(CloseReason::SignalClosed).await;
        }
    }

    fn join_context(&self) -> JoinContext {
        let video = self.grants.video.as_ref();
        JoinContext {
            info: self.info.clone(),
            client_configuration: self.client_configuration.clone(),
            // the client drives the subscriber connection when it is new
            // enough to understand that shape
            subscriber_as_primary: self.protocol.subscriber_as_primary(),
            can_publish: video.is_some_and(lk_auth::VideoGrant::can_publish),
            adaptive_stream: self.adaptive_stream,
            region: self.region.clone(),
            token_expires_at: self.token_expires_at,
            protocol: self.protocol,
            hidden: video.is_some_and(|video| video.hidden),
            recorder: video.is_some_and(|video| video.recorder),
        }
    }

    async fn handle_signal(&mut self, request: SignalRequest) {
        match request.message {
            Some(signal_request::Message::Leave(_)) => {
                self.close(CloseReason::ClientRequestLeave).await;
            }
            Some(signal_request::Message::UpdateMetadata(update)) => {
                let request_id = update.request_id;
                let attributes = update
                    .attributes
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                let result = self
                    .update_metadata(Some(update.name), Some(update.metadata), attributes, true)
                    .await;
                self.answer_request(request_id, result.err()).await;
            }
            // Ping and PingReq never reach the participant: the signal layer
            // answers them so a busy room cannot make a live connection look
            // dead.
            Some(signal_request::Message::Ping(_) | signal_request::Message::PingReq(_)) => {}
            other => {
                // Everything else needs the transport, which lands with the
                // PCTransport work. Dropping is deliberate: a made-up answer
                // would leave the client waiting on a track that never comes.
                tracing::debug!(
                    participant = %self.info.identity,
                    request = ?other.as_ref().map(std::mem::discriminant),
                    "signal request needs the media plane, which is not wired up yet"
                );
            }
        }
    }

    async fn set_state(&mut self, state: participant_info::State) {
        // the state machine only moves forward, as in Go: a late JOINED after
        // a DISCONNECTED would resurrect a participant that is gone
        if state as i32 <= self.info.state {
            return;
        }
        self.info.state = state as i32;
        self.bump_version();
        self.notify_changed().await;
    }

    async fn update_metadata(
        &mut self,
        name: Option<String>,
        metadata: Option<String>,
        attributes: BTreeMap<String, String>,
        from_client: bool,
    ) -> Result<ParticipantInfo, UpdateError> {
        if from_client && !self.can_update_own_metadata() {
            return Err(UpdateError::PermissionDenied);
        }

        if let Some(name) = &name
            && !name.is_empty()
            && !self.limits.check_participant_name_length(name)
        {
            return Err(UpdateError::NameExceedsLimits);
        }
        if let Some(metadata) = &metadata
            && !self.limits.check_metadata_size(metadata)
        {
            return Err(UpdateError::MetadataExceedsLimits);
        }

        let mut merged = self.info.attributes.clone();
        for (key, value) in attributes {
            if value.is_empty() {
                // an empty value deletes the attribute, which is how the
                // client removes one
                merged.remove(&key);
            } else {
                merged.insert(key, value);
            }
        }
        if !self
            .limits
            .check_attributes_size(merged.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        {
            return Err(UpdateError::AttributesExceedLimits);
        }

        if let Some(name) = name
            && !name.is_empty()
        {
            self.info.name.clone_from(&name);
            self.grants.name = name;
        }
        if let Some(metadata) = metadata {
            self.info.metadata.clone_from(&metadata);
            self.grants.metadata = metadata;
        }
        self.info.attributes = merged.clone();
        self.grants.attributes = merged.into_iter().collect();

        self.bump_version();
        self.notify_changed().await;
        Ok(self.info.clone())
    }

    async fn update_permission(&mut self, permission: ParticipantPermission) {
        if let Some(video) = self.grants.video.as_mut() {
            video.update_from_permission(&permission);
        }
        self.info.permission = Some(permission);
        self.bump_version();
        self.notify_changed().await;
    }

    async fn answer_request(&self, request_id: u32, error: Option<UpdateError>) {
        if request_id == 0 || !self.client.supports_request_response() {
            return;
        }
        let (reason, message) = match error {
            None => (request_response::Reason::Ok, String::new()),
            Some(UpdateError::PermissionDenied) => (
                request_response::Reason::NotAllowed,
                UpdateError::PermissionDenied.to_string(),
            ),
            Some(err) => (request_response::Reason::LimitExceeded, err.to_string()),
        };
        let _ = self
            .responses
            .send(SignalResponse {
                message: Some(signal_response::Message::RequestResponse(RequestResponse {
                    request_id,
                    reason: reason as i32,
                    message,
                    // the echoed request is optional, and the client keys off
                    // request_id
                    request: None,
                })),
            })
            .await;
    }

    async fn close(&mut self, reason: CloseReason) {
        if self.closed {
            return;
        }
        self.closed = true;

        self.info.state = participant_info::State::Disconnected as i32;
        self.info.disconnect_reason = reason.disconnect_reason() as i32;
        self.bump_version();

        let _ = self
            .events
            .send(ParticipantEvent::Closed {
                identity: self.info.identity.clone(),
                sid: self.info.sid.clone(),
                reason,
            })
            .await;
    }

    async fn notify_changed(&self) {
        let _ = self
            .events
            .send(ParticipantEvent::Changed {
                identity: self.info.identity.clone(),
                info: Box::new(self.info.clone()),
            })
            .await;
    }

    fn bump_version(&mut self) {
        self.info.version = version_as_u32(self.versions.next());
    }

    fn can_update_own_metadata(&self) -> bool {
        self.grants
            .video
            .as_ref()
            .is_some_and(lk_auth::VideoGrant::can_update_own_metadata)
    }
}

/// The `version` field on `ParticipantInfo` is a `uint32` while the stamps are
/// 64-bit, so the low bits are what clients compare, exactly as in Go.
fn version_as_u32(version: lk_proto::utils::TimedVersion) -> u32 {
    version.0 as u32
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The proto `TimedVersion` a participant's info would carry, for callers that
/// need the full stamp rather than the truncated `version` field.
#[must_use]
pub fn timed_version_proto(version: lk_proto::utils::TimedVersion) -> TimedVersion {
    version.to_proto()
}
