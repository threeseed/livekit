//! The `room` section: lifecycle, codecs and per-room presets.
//!
//! Ports `pkg/config.RoomConfig`, `CodecSpec` and `PlayoutDelayConfig`.

use std::collections::BTreeMap;

use lk_config_derive::ConfigSchema;
use serde::{Deserialize, Serialize};

use crate::duration::GoDuration;

/// The `room` section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct RoomConfig {
    /// Create a room on first join rather than requiring `CreateRoom`.
    pub auto_create: bool,
    /// Codecs offered to publishers, in preference order.
    pub enabled_codecs: Vec<CodecSpec>,
    /// Participant cap. Zero means no cap.
    pub max_participants: u32,
    /// Seconds an empty room is kept before it is closed.
    pub empty_timeout: u32,
    /// Seconds a room is kept after the last participant leaves.
    pub departure_timeout: u32,
    /// Let other participants unmute a participant's track.
    pub enable_remote_unmute: bool,
    /// Playout delay advertised to subscribers.
    pub playout_delay: PlayoutDelayConfig,
    /// Ask clients to synchronise streams of the same participant.
    pub sync_streams: bool,
    /// Deadline for a room-creation round trip.
    pub create_room_timeout: GoDuration,
    /// Attempts made to create a room before giving up.
    pub create_room_attempts: i32,
    /// Send the room's metadata in track webhooks, not just SID and name.
    pub enable_full_room_in_webhooks: bool,
    /// Target size in bytes of a batched participant update.
    pub update_batch_target_size: i32,
    /// Deprecated: moved to `limit.max_metadata_size`.
    pub max_metadata_size: u32,
    /// Deprecated: moved to `limit.max_room_name_length`.
    pub max_room_name_length: i32,
    /// Deprecated: moved to `limit.max_participant_identity_length`.
    pub max_participant_identity_length: i32,
    /// Named presets a token can select with its `RoomConfiguration`.
    ///
    /// The values are `livekit.RoomConfiguration` protobuf messages. They are
    /// carried as YAML here and decoded by `lk-service`, which owns the proto
    /// types; the config layer only guarantees the key survives a round trip.
    #[config(opaque)]
    pub room_configurations: BTreeMap<String, serde_yaml::Value>,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            auto_create: true,
            enabled_codecs: CodecSpec::default_enabled(),
            max_participants: 0,
            empty_timeout: 5 * 60,
            departure_timeout: 20,
            enable_remote_unmute: false,
            playout_delay: PlayoutDelayConfig::default(),
            sync_streams: false,
            create_room_timeout: GoDuration::from_secs(10),
            create_room_attempts: 3,
            enable_full_room_in_webhooks: false,
            update_batch_target_size: 128 * 1024,
            max_metadata_size: 0,
            max_room_name_length: 0,
            max_participant_identity_length: 0,
            room_configurations: BTreeMap::new(),
        }
    }
}

/// One codec offered to publishers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct CodecSpec {
    /// The MIME type, such as `audio/opus`.
    pub mime: String,
    /// An `a=fmtp` line appended to the codec in the offer.
    pub fmtp_line: String,
}

impl CodecSpec {
    /// The codec list in `DefaultConfig.Room.EnabledCodecs`, in order.
    #[must_use]
    pub fn default_enabled() -> Vec<Self> {
        [
            "audio/PCMU",
            "audio/PCMA",
            "audio/opus",
            "audio/red",
            "video/VP8",
            "video/H264",
            "video/VP9",
            "video/AV1",
            "video/H265",
            "video/rtx",
        ]
        .into_iter()
        .map(|mime| Self {
            mime: mime.to_owned(),
            fmtp_line: String::new(),
        })
        .collect()
    }
}

/// Playout delay advertised to subscribers, in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct PlayoutDelayConfig {
    /// Whether the extension is offered at all.
    pub enabled: bool,
    /// Lower bound.
    pub min: i32,
    /// Upper bound.
    pub max: i32,
}
