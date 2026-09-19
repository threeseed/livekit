//! Creating rooms and placing them on a node.
//!
//! Ports `pkg/service/roomallocator.go` for the single-node case. Node
//! selection is what a cluster does with this; on one node it is a no-op, and
//! the shape is kept so the Redis router can fill it in without changing
//! callers.

use std::sync::Arc;
use std::time::Duration;

use lk_config::Config;
use lk_proto::livekit::{CreateRoomRequest, PlayoutDelay, Room, RoomConfiguration, RoomInternal};
use lk_proto::utils::guid;

use crate::connect::{RoomAllocator, apply_room_configuration};
use crate::error::{Error, Result};
use crate::store::ObjectStore;

/// How long a create-or-join waits for the room lock.
pub const ROOM_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// The single-node room allocator.
pub struct StandardRoomAllocator {
    config: Arc<Config>,
    store: Arc<dyn ObjectStore>,
    region: String,
}

impl StandardRoomAllocator {
    /// An allocator over `store`.
    #[must_use]
    pub fn new(config: Arc<Config>, store: Arc<dyn ObjectStore>) -> Self {
        let region = config.region.clone();
        Self {
            config,
            store,
            region,
        }
    }

    /// Whether a room may be created on first join.
    #[must_use]
    pub fn auto_create_enabled(&self) -> bool {
        self.config.room.auto_create
    }

    /// Creates a room, or updates the existing one from the request.
    ///
    /// The room lock is held across the load and the store: two participants
    /// joining an unknown room at the same moment must not each create it, and
    /// each end up with a different sid.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BadRequest`] when the request names a preset this
    /// server does not have, and whatever the store returns.
    pub async fn create_room(
        &self,
        request: &CreateRoomRequest,
    ) -> Result<(Room, RoomInternal, bool)> {
        let _lock = self
            .store
            .lock_room(&request.name, ROOM_LOCK_TIMEOUT)
            .await?;

        let (mut room, mut internal, created) =
            match self.store.load_room(&request.name, true).await {
                Ok((room, internal)) => (room, internal.unwrap_or_default(), false),
                Err(Error::RoomNotFound) => {
                    let now = unix_seconds();
                    let mut room = Room {
                        sid: guid::new_guid(guid::ROOM_PREFIX),
                        name: request.name.clone(),
                        creation_time: now,
                        creation_time_ms: unix_millis(),
                        turn_password: random_secret(),
                        ..Room::default()
                    };
                    let mut internal = RoomInternal::default();
                    self.apply_defaults(&mut room, &mut internal);
                    (room, internal, true)
                }
                Err(err) => return Err(err),
            };

        let request = self.apply_named_configuration(request)?;

        // Only non-zero fields of the request override what the room has: a
        // join that carries no room configuration must not reset the room the
        // API created.
        if request.empty_timeout > 0 {
            room.empty_timeout = request.empty_timeout;
        }
        if request.departure_timeout > 0 {
            room.departure_timeout = request.departure_timeout;
        }
        if request.max_participants > 0 {
            room.max_participants = request.max_participants;
        }
        if !request.metadata.is_empty() {
            room.metadata.clone_from(&request.metadata);
        }
        if let Some(egress) = &request.egress {
            if let Some(participant) = &egress.participant {
                internal.participant_egress = Some(participant.clone());
            }
            if let Some(tracks) = &egress.tracks {
                internal.track_egress = Some(tracks.clone());
            }
        }
        if !request.agents.is_empty() {
            internal.agent_dispatches = request.agents.clone();
        }
        if request.min_playout_delay > 0 || request.max_playout_delay > 0 {
            internal.playout_delay = Some(PlayoutDelay {
                enabled: true,
                min: request.min_playout_delay,
                max: request.max_playout_delay,
            });
        }
        if request.sync_streams {
            internal.sync_streams = true;
        }

        self.store
            .store_room(room.clone(), internal.clone())
            .await?;
        Ok((room, internal, created))
    }

    fn apply_defaults(&self, room: &mut Room, internal: &mut RoomInternal) {
        let conf = &self.config.room;
        room.empty_timeout = conf.empty_timeout;
        room.departure_timeout = conf.departure_timeout;
        room.max_participants = conf.max_participants;
        room.enabled_codecs = conf
            .enabled_codecs
            .iter()
            .map(|codec| lk_proto::livekit::Codec {
                mime: codec.mime.clone(),
                fmtp_line: codec.fmtp_line.clone(),
            })
            .collect();
        internal.playout_delay = Some(PlayoutDelay {
            enabled: conf.playout_delay.enabled,
            min: conf.playout_delay.min as u32,
            max: conf.playout_delay.max as u32,
        });
        internal.sync_streams = conf.sync_streams;
    }

    /// Fills the request's unset fields from a named preset.
    ///
    /// The preset never overrides what the request set, which is what lets a
    /// token carry a preset and a caller still override one field of it.
    fn apply_named_configuration(&self, request: &CreateRoomRequest) -> Result<CreateRoomRequest> {
        if request.room_preset.is_empty() {
            return Ok(request.clone());
        }
        let Some(preset) = self
            .config
            .room
            .room_configurations
            .get(&request.room_preset)
        else {
            return Err(Error::BadRequest(
                "unknown room configuration in create room request",
            ));
        };
        let preset: RoomConfiguration = serde_yaml::from_value(preset.clone())
            .map_err(|_| Error::BadRequest("invalid room configuration preset"))?;

        let mut clone = request.clone();
        // start from the preset and let the request's own fields win
        let mut merged = CreateRoomRequest::default();
        apply_room_configuration(&mut merged, &preset);

        if clone.empty_timeout == 0 {
            clone.empty_timeout = merged.empty_timeout;
        }
        if clone.departure_timeout == 0 {
            clone.departure_timeout = merged.departure_timeout;
        }
        if clone.max_participants == 0 {
            clone.max_participants = merged.max_participants;
        }
        if clone.egress.is_none() {
            clone.egress = merged.egress;
        }
        if clone.agents.is_empty() {
            clone.agents = merged.agents;
        }
        if clone.min_playout_delay == 0 {
            clone.min_playout_delay = merged.min_playout_delay;
        }
        if clone.max_playout_delay == 0 {
            clone.max_playout_delay = merged.max_playout_delay;
        }
        if !clone.sync_streams {
            clone.sync_streams = merged.sync_streams;
        }
        if clone.metadata.is_empty() {
            clone.metadata = merged.metadata;
        }
        Ok(clone)
    }
}

impl RoomAllocator for StandardRoomAllocator {
    fn validate_create_room<'a>(
        &'a self,
        room_name: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // With auto-create off, a participant may only join a room the API
            // already created.
            if self.config.room.auto_create {
                return Ok(());
            }
            self.store.load_room(room_name, false).await.map(|_| ())
        })
    }

    fn select_room_node<'a>(
        &'a self,
        _room_name: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        // One node: the room is already where it is going to be.
        Box::pin(async { Ok(()) })
    }

    fn region(&self) -> String {
        self.region.clone()
    }
}

/// The room's TURN password, which the embedded TURN server checks a
/// participant's credentials against.
fn random_secret() -> String {
    use rand::Rng as _;
    let mut rng = rand::rng();
    (0..32)
        .filter_map(|_| {
            let index = rng.random_range(0..guid::ALPHABET.len());
            guid::ALPHABET.get(index).copied().map(char::from)
        })
        .collect()
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
