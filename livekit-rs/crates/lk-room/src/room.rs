//! The room actor.
//!
//! Ports the control plane of `pkg/rtc/room.go`. The Go `Room` is a map behind
//! a mutex that every participant, the room service and a one-second server-wide
//! sweep all reach into. Here it is one task: joins, leaves, metadata changes
//! and the close timers are messages, so there is no lock to order and the
//! close timer is armed on the transition that should arm it rather than
//! polled.
//!
//! # What is here
//!
//! Membership: join, leave, the participant-update broadcast, room metadata,
//! per-participant permission changes, and the empty and departure timeouts.
//! Media, subscriptions and data packets arrive with the transport work.

use std::collections::BTreeMap;
use std::time::Duration;

use lk_proto::livekit::{
    ParticipantInfo, ParticipantPermission, ParticipantUpdate, Room as RoomProto, RoomInternal,
    RoomUpdate, ServerInfo, SignalResponse, participant_info, signal_response,
};
use lk_proto::utils::guid;
use tokio::sync::{mpsc, oneshot};

use crate::participant::{
    self, CloseReason, JoinContext, ParticipantEvent, ParticipantHandle, ParticipantParams,
};

/// How many commands a room queues before its sender waits.
const COMMAND_QUEUE_DEPTH: usize = 256;

/// Ping interval advertised in the join response, in seconds. The client pings
/// on this cadence and the server expects a ping within the timeout.
pub const PING_INTERVAL_SECONDS: i32 = 5;

/// Ping timeout advertised in the join response, in seconds.
pub const PING_TIMEOUT_SECONDS: i32 = 15;

/// Why a room closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomCloseReason {
    /// Nobody ever joined and the empty timeout elapsed.
    Empty,
    /// Everyone left and the departure timeout elapsed.
    AllLeft,
    /// The room service deleted the room.
    ServiceRequestDeleteRoom,
    /// The node is shutting down.
    NodeShutdown,
}

/// Why a join was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum JoinError {
    /// The room has closed.
    #[error("room has closed")]
    RoomClosed,
    /// The room is at `max_participants`.
    #[error("room is at its participant limit")]
    MaxParticipantsExceeded,
    /// This identity is already in the room.
    #[error("participant already joined")]
    AlreadyJoined,
}

/// What a room needs to exist.
pub struct RoomParams {
    /// The room as clients see it.
    pub proto: RoomProto,
    /// The server-side settings that never reach a client.
    pub internal: RoomInternal,
    /// What the server tells clients about itself.
    pub server_info: ServerInfo,
}

/// A participant joining a room.
pub struct JoinParams {
    /// How to build the participant.
    pub participant: ParticipantParams,
    /// ICE servers offered to the client.
    pub ice_servers: Vec<lk_proto::livekit::IceServer>,
}

/// A participant that joined.
pub struct Joined {
    /// The participant's handle.
    pub participant: ParticipantHandle,
    /// The response the client is waiting for.
    pub join_response: Box<lk_proto::livekit::JoinResponse>,
}

enum Command {
    Join {
        params: Box<JoinParams>,
        reply: oneshot::Sender<Result<Joined, JoinError>>,
    },
    RemoveParticipant {
        identity: String,
        sid: String,
        reason: CloseReason,
    },
    ParticipantEvent(ParticipantEvent),
    Participants(oneshot::Sender<Vec<ParticipantInfo>>),
    Participant {
        identity: String,
        reply: oneshot::Sender<Option<ParticipantHandle>>,
    },
    ToProto(oneshot::Sender<Box<RoomProto>>),
    Internal(oneshot::Sender<Box<RoomInternal>>),
    SetMetadata(String),
    UpdatePermission {
        identity: String,
        permission: Box<ParticipantPermission>,
        reply: oneshot::Sender<Option<ParticipantInfo>>,
    },
    Close(RoomCloseReason),
}

/// A handle to a room actor.
#[derive(Clone, Debug)]
pub struct RoomHandle {
    name: String,
    sid: String,
    commands: mpsc::Sender<Command>,
}

impl RoomHandle {
    /// The room's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The room's sid.
    #[must_use]
    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// Whether the room's actor is still running.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.commands.is_closed()
    }

    /// Joins a participant.
    ///
    /// # Errors
    ///
    /// Returns [`JoinError`] when the room is closed, full, or already has this
    /// identity.
    pub async fn join(&self, params: JoinParams) -> Result<Joined, JoinError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Join {
                params: Box::new(params),
                reply,
            })
            .await
            .map_err(|_| JoinError::RoomClosed)?;
        answer.await.map_err(|_| JoinError::RoomClosed)?
    }

    /// Removes a participant.
    ///
    /// `sid` guards against a stale removal: a close for a session that has
    /// already been replaced must not evict its replacement.
    pub async fn remove_participant(&self, identity: &str, sid: &str, reason: CloseReason) {
        let _ = self
            .commands
            .send(Command::RemoveParticipant {
                identity: identity.to_owned(),
                sid: sid.to_owned(),
                reason,
            })
            .await;
    }

    /// Every participant's info.
    pub async fn participants(&self) -> Vec<ParticipantInfo> {
        let (reply, answer) = oneshot::channel();
        if self
            .commands
            .send(Command::Participants(reply))
            .await
            .is_err()
        {
            return Vec::new();
        }
        answer.await.unwrap_or_default()
    }

    /// One participant's handle.
    pub async fn participant(&self, identity: &str) -> Option<ParticipantHandle> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Participant {
                identity: identity.to_owned(),
                reply,
            })
            .await
            .ok()?;
        answer.await.ok().flatten()
    }

    /// The room as clients see it.
    pub async fn to_proto(&self) -> Option<RoomProto> {
        let (reply, answer) = oneshot::channel();
        self.commands.send(Command::ToProto(reply)).await.ok()?;
        answer.await.ok().map(|proto| *proto)
    }

    /// The server-side room settings that never reach a client.
    pub async fn internal(&self) -> Option<RoomInternal> {
        let (reply, answer) = oneshot::channel();
        self.commands.send(Command::Internal(reply)).await.ok()?;
        answer.await.ok().map(|internal| *internal)
    }

    /// Sets the room metadata and tells every participant.
    pub async fn set_metadata(&self, metadata: String) {
        let _ = self.commands.send(Command::SetMetadata(metadata)).await;
    }

    /// Changes a participant's permissions.
    pub async fn update_permission(
        &self,
        identity: &str,
        permission: ParticipantPermission,
    ) -> Option<ParticipantInfo> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::UpdatePermission {
                identity: identity.to_owned(),
                permission: Box::new(permission),
                reply,
            })
            .await
            .ok()?;
        answer.await.ok().flatten()
    }

    /// Closes the room and every participant in it.
    pub async fn close(&self, reason: RoomCloseReason) {
        let _ = self.commands.send(Command::Close(reason)).await;
    }
}

/// Starts a room actor and returns its handle.
#[must_use]
pub fn spawn(params: RoomParams) -> RoomHandle {
    let (commands, receiver) = mpsc::channel(COMMAND_QUEUE_DEPTH);
    let handle = RoomHandle {
        name: params.proto.name.clone(),
        sid: params.proto.sid.clone(),
        commands: commands.clone(),
    };
    tokio::spawn(async move { Room::new(params, commands).run(receiver).await });
    handle
}

struct Entry {
    handle: ParticipantHandle,
    info: ParticipantInfo,
    hidden: bool,
    recorder: bool,
}

struct Room {
    proto: RoomProto,
    internal: RoomInternal,
    server_info: ServerInfo,
    participants: BTreeMap<String, Entry>,
    events: mpsc::Sender<Command>,
    /// When the first participant joined, as seconds since the Unix epoch.
    first_joined_at: i64,
    /// When the last participant left, as seconds since the Unix epoch.
    last_left_at: i64,
    /// When the room should close if nothing else happens.
    close_at: Option<tokio::time::Instant>,
    closed: bool,
}

impl Room {
    fn new(params: RoomParams, commands: mpsc::Sender<Command>) -> Self {
        let mut room = Self {
            proto: params.proto,
            internal: params.internal,
            server_info: params.server_info,
            participants: BTreeMap::new(),
            events: commands,
            first_joined_at: 0,
            last_left_at: 0,
            close_at: None,
            closed: false,
        };
        room.arm_close_timer();
        room
    }

    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        loop {
            let tick = self.close_at;
            let command = match tick {
                Some(deadline) => tokio::select! {
                    command = commands.recv() => command,
                    () = tokio::time::sleep_until(deadline) => {
                        if self.participants.is_empty() {
                            let reason = if self.first_joined_at > 0 && self.last_left_at > 0 {
                                RoomCloseReason::AllLeft
                            } else {
                                RoomCloseReason::Empty
                            };
                            self.close(reason).await;
                            return;
                        }
                        self.arm_close_timer();
                        continue;
                    }
                },
                None => commands.recv().await,
            };

            let Some(command) = command else {
                return;
            };

            match command {
                Command::Join { params, reply } => {
                    let result = self.join(*params).await;
                    let _ = reply.send(result);
                }
                Command::RemoveParticipant {
                    identity,
                    sid,
                    reason,
                } => self.remove_participant(&identity, &sid, reason).await,
                Command::ParticipantEvent(event) => self.handle_participant_event(event).await,
                Command::Participants(reply) => {
                    let _ = reply.send(
                        self.participants
                            .values()
                            .map(|entry| entry.info.clone())
                            .collect(),
                    );
                }
                Command::Participant { identity, reply } => {
                    let _ = reply.send(
                        self.participants
                            .get(&identity)
                            .map(|entry| entry.handle.clone()),
                    );
                }
                Command::ToProto(reply) => {
                    let _ = reply.send(Box::new(self.proto.clone()));
                }
                Command::Internal(reply) => {
                    let _ = reply.send(Box::new(self.internal.clone()));
                }
                Command::SetMetadata(metadata) => self.set_metadata(metadata).await,
                Command::UpdatePermission {
                    identity,
                    permission,
                    reply,
                } => {
                    let info = match self.participants.get(&identity) {
                        Some(entry) => entry.handle.update_permission(*permission).await,
                        None => None,
                    };
                    let _ = reply.send(info);
                }
                Command::Close(reason) => {
                    self.close(reason).await;
                    return;
                }
            }
        }
    }

    async fn join(&mut self, params: JoinParams) -> Result<Joined, JoinError> {
        if self.closed {
            return Err(JoinError::RoomClosed);
        }
        let identity = params.participant.identity.clone();
        if self.participants.contains_key(&identity) {
            return Err(JoinError::AlreadyJoined);
        }
        if self.proto.max_participants > 0
            && self.participants.len() as u32 >= self.proto.max_participants
        {
            return Err(JoinError::MaxParticipantsExceeded);
        }

        let handle = participant::spawn(ParticipantParams {
            events: self.participant_events(),
            ..params.participant
        });
        let Some(context) = handle.join_context().await else {
            return Err(JoinError::RoomClosed);
        };

        let others: Vec<ParticipantInfo> = self
            .participants
            .values()
            .filter(|entry| !entry.hidden)
            .map(|entry| entry.info.clone())
            .collect();

        let join_response = self.build_join_response(&context, others, params.ice_servers);

        if context.recorder && !self.proto.active_recording {
            self.proto.active_recording = true;
        }
        if !context.hidden {
            self.proto.num_participants += 1;
        }
        if self.first_joined_at == 0 {
            self.first_joined_at = unix_seconds();
        }

        self.participants.insert(
            identity.clone(),
            Entry {
                handle: handle.clone(),
                info: context.info.clone(),
                hidden: context.hidden,
                recorder: context.recorder,
            },
        );
        self.arm_close_timer();

        // Everyone else learns about the new participant. The joiner does not
        // need this update: its own info and everyone else's are already in
        // the join response it is about to receive.
        self.broadcast_participants(&[context.info], Some(&identity))
            .await;

        handle.set_state(participant_info::State::Joined).await;

        Ok(Joined {
            participant: handle,
            join_response: Box::new(join_response),
        })
    }

    fn build_join_response(
        &self,
        context: &JoinContext,
        others: Vec<ParticipantInfo>,
        ice_servers: Vec<lk_proto::livekit::IceServer>,
    ) -> lk_proto::livekit::JoinResponse {
        lk_proto::livekit::JoinResponse {
            room: Some(self.proto.clone()),
            participant: Some(context.info.clone()),
            other_participants: others,
            ice_servers,
            subscriber_primary: context.subscriber_as_primary,
            client_configuration: context.client_configuration.clone(),
            ping_interval: PING_INTERVAL_SECONDS,
            ping_timeout: PING_TIMEOUT_SECONDS,
            server_info: Some(self.server_info.clone()),
            server_version: self.server_info.version.clone(),
            server_region: self.server_info.region.clone(),
            enabled_publish_codecs: self.proto.enabled_codecs.clone(),
            // fast publish lets the client send media before the first answer;
            // it is only safe when the client may publish at all
            fast_publish: context.can_publish,
            ..lk_proto::livekit::JoinResponse::default()
        }
    }

    async fn remove_participant(&mut self, identity: &str, sid: &str, reason: CloseReason) {
        let Some(entry) = self.participants.get(identity) else {
            return;
        };
        // a close for a session that was already replaced must not evict the
        // replacement
        if !sid.is_empty() && entry.info.sid != sid {
            return;
        }

        let handle = entry.handle.clone();
        handle.close(reason).await;
        self.finish_removal(identity).await;
    }

    async fn finish_removal(&mut self, identity: &str) {
        let Some(entry) = self.participants.remove(identity) else {
            return;
        };
        if !entry.hidden && self.proto.num_participants > 0 {
            self.proto.num_participants -= 1;
        }
        if entry.recorder {
            self.proto.active_recording = self.participants.values().any(|entry| entry.recorder);
        }

        let mut info = entry.info;
        info.state = participant_info::State::Disconnected as i32;
        self.broadcast_participants(&[info], None).await;

        if self.participants.is_empty() {
            self.last_left_at = unix_seconds();
        }
        self.arm_close_timer();
    }

    async fn handle_participant_event(&mut self, event: ParticipantEvent) {
        match event {
            ParticipantEvent::Changed { identity, info } => {
                let Some(entry) = self.participants.get_mut(&identity) else {
                    return;
                };
                if entry.info.sid != info.sid {
                    // a stale update from a replaced session
                    return;
                }
                entry.info = *info.clone();
                self.broadcast_participants(&[*info], None).await;
            }
            ParticipantEvent::Closed { identity, sid, .. } => {
                let is_current = self
                    .participants
                    .get(&identity)
                    .is_some_and(|entry| entry.info.sid == sid);
                if is_current {
                    self.finish_removal(&identity).await;
                }
            }
        }
    }

    /// Sends a participant update to everyone, optionally skipping one.
    ///
    /// A hidden participant is left out of everyone else's update, as in Go:
    /// it sees the room but the room does not see it.
    async fn broadcast_participants(&self, updates: &[ParticipantInfo], skip: Option<&str>) {
        let visible: Vec<ParticipantInfo> = updates
            .iter()
            .filter(|info| !self.is_hidden(&info.identity))
            .cloned()
            .collect();
        if visible.is_empty() {
            return;
        }

        let response = SignalResponse {
            message: Some(signal_response::Message::Update(ParticipantUpdate {
                participants: visible,
            })),
        };
        for (identity, entry) in &self.participants {
            if Some(identity.as_str()) == skip {
                continue;
            }
            entry.handle.send(response.clone()).await;
        }
    }

    fn is_hidden(&self, identity: &str) -> bool {
        self.participants
            .get(identity)
            .is_some_and(|entry| entry.hidden)
    }

    async fn set_metadata(&mut self, metadata: String) {
        self.proto.metadata = metadata;
        let response = SignalResponse {
            message: Some(signal_response::Message::RoomUpdate(RoomUpdate {
                room: Some(self.proto.clone()),
            })),
        };
        for entry in self.participants.values() {
            entry.handle.send(response.clone()).await;
        }
    }

    /// Arms the close timer for the state the room is now in.
    ///
    /// Ports `CloseIfEmpty`'s rule without its one-second server-wide sweep:
    /// a room that nobody has joined yet closes `empty_timeout` after it was
    /// created, and a room everyone has left closes `departure_timeout` after
    /// the last one left. The departure timeout is the longer grace period
    /// because a participant may be reconnecting.
    ///
    /// Computing the deadline when membership changes, rather than polling
    /// every room every second, is the same behaviour with no work on an idle
    /// node.
    fn arm_close_timer(&mut self) {
        if !self.participants.is_empty() {
            self.close_at = None;
            return;
        }

        let now = unix_seconds();
        let (timeout, since) = if self.first_joined_at > 0 && self.last_left_at > 0 {
            (self.proto.departure_timeout, self.last_left_at)
        } else {
            (self.proto.empty_timeout, self.proto.creation_time)
        };
        if timeout == 0 {
            self.close_at = None;
            return;
        }

        let elapsed = (now - since).max(0);
        let remaining = i64::from(timeout).saturating_sub(elapsed).max(0);
        self.close_at = Some(tokio::time::Instant::now() + Duration::from_secs(remaining as u64));
    }

    async fn close(&mut self, reason: RoomCloseReason) {
        if self.closed {
            return;
        }
        self.closed = true;
        tracing::info!(room = %self.proto.name, ?reason, "closing room");

        let handles: Vec<ParticipantHandle> = self
            .participants
            .values()
            .map(|entry| entry.handle.clone())
            .collect();
        for handle in handles {
            handle.close(CloseReason::RoomClosed).await;
        }
        self.participants.clear();
    }

    /// A sender participants report changes on, wrapped so the room's own
    /// command channel carries them.
    fn participant_events(&self) -> mpsc::Sender<ParticipantEvent> {
        let (sender, mut receiver) = mpsc::channel(participant::EVENT_QUEUE_DEPTH);
        let commands = self.events.clone();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if commands
                    .send(Command::ParticipantEvent(event))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        sender
    }
}

/// A room sid, for callers creating a room.
#[must_use]
pub fn new_room_sid() -> String {
    guid::new_guid(guid::ROOM_PREFIX)
}

/// A participant sid, for callers creating a participant.
#[must_use]
pub fn new_participant_sid() -> String {
    guid::new_guid(guid::PARTICIPANT_PREFIX)
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
