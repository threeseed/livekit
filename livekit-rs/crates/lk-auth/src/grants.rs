//! `ClaimGrants` and the grant sub-objects, field for field with
//! `protocol/auth/grants.go`.
//!
//! The JSON names are the contract: every LiveKit SDK mints tokens with these
//! keys, so each field carries its Go `json:` tag verbatim, and `omitempty` is
//! reproduced with `skip_serializing_if` so a token minted here is byte-wise
//! the same shape as one minted by the Go server.
//!
//! The tri-state permissions (`canPublish` and friends) are `Option<bool>` for
//! the same reason they are `*bool` in Go: "unset" is not "false". Unset
//! `canPublish` means publish is allowed, unset `canUpdateOwnMetadata` means it
//! is not, and collapsing either to a plain `bool` changes what existing tokens
//! authorise.

use std::collections::BTreeMap;

use lk_proto::livekit::{ParticipantPermission, RoomConfiguration, TrackSource, participant_info};
use serde::{Deserialize, Serialize};

/// The LiveKit claims carried inside an access token.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaimGrants {
    /// Participant identity. Also the JWT `sub`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub identity: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Participant kind, lower-cased: `standard`, `ingress`, `egress`, `sip`
    /// or `agent`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Kind details, lower-cased.
    #[serde(rename = "kindDetails", default, skip_serializing_if = "Vec::is_empty")]
    pub kind_details: Vec<String>,
    /// Room and media permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoGrant>,
    /// SIP permissions.
    #[serde(rename = "sip", default, skip_serializing_if = "Option::is_none")]
    pub sip: Option<SipGrant>,
    /// Agent permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentGrant>,
    /// Inference permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference: Option<InferenceGrant>,
    /// Observability permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observability: Option<ObservabilityGrant>,
    /// Room configuration applied if this participant creates the room.
    ///
    /// Encoded with protojson, as `RoomConfiguration.MarshalJSON` does.
    #[serde(
        rename = "roomConfig",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub room_config: Option<RoomConfiguration>,
    /// Cloud-only: a named preset. Keys set in `roomConfig` win over it.
    #[serde(
        rename = "roomPreset",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub room_preset: String,
    /// SHA-256 of a message body, for integrity checks.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    /// Participant metadata.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub metadata: String,
    /// Participant attributes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

impl ClaimGrants {
    /// Sets [`Self::kind`] from the proto enum.
    pub fn set_participant_kind(&mut self, kind: participant_info::Kind) {
        self.kind = kind_to_string(kind);
    }

    /// The participant kind, defaulting to `STANDARD` for an unset or
    /// unrecognised value, as `kindToProto` does.
    #[must_use]
    pub fn participant_kind(&self) -> participant_info::Kind {
        kind_from_string(&self.kind)
    }

    /// Sets [`Self::kind_details`] from the proto enums.
    pub fn set_kind_details(&mut self, details: &[participant_info::KindDetail]) {
        self.kind_details = details.iter().map(|d| lower(d.as_str_name())).collect();
    }

    /// The kind details, dropping values this build does not recognise, as
    /// `kindDetailsToProto` does.
    #[must_use]
    pub fn kind_details(&self) -> Vec<participant_info::KindDetail> {
        self.kind_details
            .iter()
            .filter_map(|d| participant_info::KindDetail::from_str_name(&d.to_uppercase()))
            .collect()
    }
}

/// Room and media permissions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoGrant {
    /// May create rooms.
    #[serde(rename = "roomCreate", default, skip_serializing_if = "is_false")]
    pub room_create: bool,
    /// May list rooms.
    #[serde(rename = "roomList", default, skip_serializing_if = "is_false")]
    pub room_list: bool,
    /// May start recordings.
    #[serde(rename = "roomRecord", default, skip_serializing_if = "is_false")]
    pub room_record: bool,

    /// May administer the room named by [`Self::room`].
    #[serde(rename = "roomAdmin", default, skip_serializing_if = "is_false")]
    pub room_admin: bool,
    /// May join the room named by [`Self::room`].
    #[serde(rename = "roomJoin", default, skip_serializing_if = "is_false")]
    pub room_join: bool,
    /// The room this grant applies to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub room: String,

    /// May publish media. Unset means yes.
    #[serde(
        rename = "canPublish",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub can_publish: Option<bool>,
    /// May subscribe to media. Unset means yes.
    #[serde(
        rename = "canSubscribe",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub can_subscribe: Option<bool>,
    /// May publish data. Unset follows [`Self::can_publish`].
    #[serde(
        rename = "canPublishData",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub can_publish_data: Option<bool>,
    /// Track sources that may be published, lower-cased. When non-empty it
    /// supersedes [`Self::can_publish`].
    #[serde(
        rename = "canPublishSources",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub can_publish_sources: Vec<String>,
    /// May update its own metadata. Unset means no.
    #[serde(
        rename = "canUpdateOwnMetadata",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub can_update_own_metadata: Option<bool>,

    /// May administer every ingress.
    #[serde(rename = "ingressAdmin", default, skip_serializing_if = "is_false")]
    pub ingress_admin: bool,

    /// Invisible to other participants.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// Joins as a recorder.
    #[serde(default, skip_serializing_if = "is_false")]
    pub recorder: bool,
    /// May register as an agent framework worker.
    #[serde(default, skip_serializing_if = "is_false")]
    pub agent: bool,

    /// May subscribe to metrics. Unset means no.
    #[serde(
        rename = "canSubscribeMetrics",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub can_subscribe_metrics: Option<bool>,

    /// May manage an agent session over `RemoteSession`. Unset means no.
    #[serde(
        rename = "canManageAgentSession",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub can_manage_agent_session: Option<bool>,

    /// Room this participant may be forwarded to.
    #[serde(
        rename = "destinationRoom",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub destination_room: String,
}

impl VideoGrant {
    /// Whether media may be published. Unset means yes.
    #[must_use]
    pub fn can_publish(&self) -> bool {
        self.can_publish.unwrap_or(true)
    }

    /// Whether a given source may be published.
    ///
    /// An empty source list is not distinguished from an absent one, because
    /// that distinction does not survive JSON.
    #[must_use]
    pub fn can_publish_source(&self, source: TrackSource) -> bool {
        if !self.can_publish() {
            return false;
        }
        if self.can_publish_sources.is_empty() {
            return true;
        }
        let name = lower(source.as_str_name());
        self.can_publish_sources.contains(&name)
    }

    /// The publishable sources as proto enums, empty when unrestricted.
    #[must_use]
    pub fn can_publish_sources(&self) -> Vec<TrackSource> {
        self.can_publish_sources
            .iter()
            .map(|s| TrackSource::from_str_name(&s.to_uppercase()).unwrap_or(TrackSource::Unknown))
            .collect()
    }

    /// Replaces the publishable sources.
    pub fn set_can_publish_sources(&mut self, sources: &[TrackSource]) {
        self.can_publish_sources = sources.iter().map(|s| lower(s.as_str_name())).collect();
    }

    /// Whether data may be published. Unset follows [`Self::can_publish`].
    #[must_use]
    pub fn can_publish_data(&self) -> bool {
        self.can_publish_data.unwrap_or_else(|| self.can_publish())
    }

    /// Whether media may be subscribed to. Unset means yes.
    #[must_use]
    pub fn can_subscribe(&self) -> bool {
        self.can_subscribe.unwrap_or(true)
    }

    /// Whether the participant may update its own metadata. Unset means no.
    #[must_use]
    pub fn can_update_own_metadata(&self) -> bool {
        self.can_update_own_metadata.unwrap_or(false)
    }

    /// Whether the participant may subscribe to metrics. Unset means no.
    #[must_use]
    pub fn can_subscribe_metrics(&self) -> bool {
        self.can_subscribe_metrics.unwrap_or(false)
    }

    /// Whether the participant may manage an agent session. Unset means no.
    #[must_use]
    pub fn can_manage_agent_session(&self) -> bool {
        self.can_manage_agent_session.unwrap_or(false)
    }

    /// Whether this grant already says exactly what `permission` says, so a
    /// `SetPermission` that would not change anything can be skipped.
    #[must_use]
    // `recorder` and `agent` are deprecated in the proto but still carried by
    // the Go server and by clients in the field, so the grant keeps mapping
    // them rather than silently dropping a permission.
    #[allow(deprecated)]
    pub fn matches_permission(&self, permission: &ParticipantPermission) -> bool {
        self.can_publish() == permission.can_publish
            && self.can_publish_data() == permission.can_publish_data
            && self.can_subscribe() == permission.can_subscribe
            && self.can_update_own_metadata() == permission.can_update_metadata
            && self.hidden == permission.hidden
            && self.recorder == permission.recorder
            && self.agent == permission.agent
            && self
                .can_publish_sources()
                .iter()
                .map(|s| *s as i32)
                .eq(permission.can_publish_sources.iter().copied())
            && self.can_subscribe_metrics() == permission.can_subscribe_metrics
            && self.can_manage_agent_session() == permission.can_manage_agent_session
    }

    /// Overwrites the grant from a runtime permission change, as
    /// `UpdateFromPermission` does. Every tri-state becomes explicit, because
    /// the permission message has no "unset".
    // `recorder` and `agent` are deprecated in the proto but still carried by
    // the Go server and by clients in the field, so the grant keeps mapping
    // them rather than silently dropping a permission.
    #[allow(deprecated)]
    pub fn update_from_permission(&mut self, permission: &ParticipantPermission) {
        self.can_publish = Some(permission.can_publish);
        self.can_publish_data = Some(permission.can_publish_data);
        self.can_publish_sources = permission
            .can_publish_sources
            .iter()
            .map(|s| {
                lower(
                    TrackSource::try_from(*s)
                        .unwrap_or(TrackSource::Unknown)
                        .as_str_name(),
                )
            })
            .collect();
        self.can_subscribe = Some(permission.can_subscribe);
        self.can_update_own_metadata = Some(permission.can_update_metadata);
        self.hidden = permission.hidden;
        self.recorder = permission.recorder;
        self.agent = permission.agent;
        self.can_subscribe_metrics = Some(permission.can_subscribe_metrics);
        self.can_manage_agent_session = Some(permission.can_manage_agent_session);
    }

    /// The grant as a `ParticipantPermission`, resolving every tri-state.
    #[must_use]
    // `recorder` and `agent` are deprecated in the proto but still carried by
    // the Go server and by clients in the field, so the grant keeps mapping
    // them rather than silently dropping a permission.
    #[allow(deprecated)]
    pub fn to_permission(&self) -> ParticipantPermission {
        ParticipantPermission {
            can_publish: self.can_publish(),
            can_publish_data: self.can_publish_data(),
            can_subscribe: self.can_subscribe(),
            can_publish_sources: self
                .can_publish_sources()
                .into_iter()
                .map(|s| s as i32)
                .collect(),
            can_update_metadata: self.can_update_own_metadata(),
            hidden: self.hidden,
            recorder: self.recorder,
            agent: self.agent,
            can_subscribe_metrics: self.can_subscribe_metrics(),
            can_manage_agent_session: self.can_manage_agent_session(),
        }
    }
}

/// SIP permissions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SipGrant {
    /// Grants every SIP feature.
    #[serde(default, skip_serializing_if = "is_false")]
    pub admin: bool,
    /// May place outbound SIP calls.
    #[serde(default, skip_serializing_if = "is_false")]
    pub call: bool,
}

/// Agent permissions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentGrant {
    /// May create, update and delete cloud agents.
    #[serde(default, skip_serializing_if = "is_false")]
    pub admin: bool,
    /// May manage simulations and scenarios.
    #[serde(rename = "simulationAdmin", default, skip_serializing_if = "is_false")]
    pub simulation_admin: bool,
    /// May access the project's agent databases.
    #[serde(rename = "databaseAdmin", default, skip_serializing_if = "is_false")]
    pub database_admin: bool,
    /// May manage the project's agent dispatch queue.
    #[serde(rename = "dispatchAdmin", default, skip_serializing_if = "is_false")]
    pub dispatch_admin: bool,
}

/// Inference permissions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceGrant {
    /// Grants LLM, STT and TTS inference.
    #[serde(default, skip_serializing_if = "is_false")]
    pub perform: bool,
}

/// Observability permissions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservabilityGrant {
    /// May publish observability data.
    #[serde(default, skip_serializing_if = "is_false")]
    pub write: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

fn lower(name: &str) -> String {
    name.to_lowercase()
}

fn kind_to_string(kind: participant_info::Kind) -> String {
    lower(kind.as_str_name())
}

fn kind_from_string(kind: &str) -> participant_info::Kind {
    participant_info::Kind::from_str_name(&kind.to_uppercase())
        .unwrap_or(participant_info::Kind::Standard)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, deprecated)]
mod tests {
    use super::*;

    #[test]
    fn unset_permissions_keep_their_go_meaning() {
        let grant = VideoGrant::default();
        // publish and subscribe default to allowed, metadata updates do not
        assert!(grant.can_publish());
        assert!(grant.can_subscribe());
        assert!(grant.can_publish_data());
        assert!(!grant.can_update_own_metadata());
        assert!(!grant.can_subscribe_metrics());
        assert!(!grant.can_manage_agent_session());
    }

    #[test]
    fn can_publish_data_follows_can_publish_when_unset() {
        let grant = VideoGrant {
            can_publish: Some(false),
            ..VideoGrant::default()
        };
        assert!(!grant.can_publish_data());

        let grant = VideoGrant {
            can_publish: Some(false),
            can_publish_data: Some(true),
            ..VideoGrant::default()
        };
        assert!(grant.can_publish_data());
    }

    #[test]
    fn an_empty_source_list_allows_every_source() {
        let grant = VideoGrant::default();
        assert!(grant.can_publish_source(TrackSource::Camera));
        assert!(grant.can_publish_source(TrackSource::ScreenShare));

        let mut grant = VideoGrant::default();
        grant.set_can_publish_sources(&[TrackSource::Microphone]);
        assert_eq!(grant.can_publish_sources, vec!["microphone".to_owned()]);
        assert!(grant.can_publish_source(TrackSource::Microphone));
        assert!(!grant.can_publish_source(TrackSource::Camera));
    }

    #[test]
    fn permissions_round_trip_through_the_grant() {
        let permission = ParticipantPermission {
            can_publish: false,
            can_publish_data: true,
            can_subscribe: true,
            can_publish_sources: vec![TrackSource::Camera as i32],
            can_update_metadata: true,
            hidden: true,
            recorder: false,
            agent: true,
            can_subscribe_metrics: true,
            can_manage_agent_session: false,
        };

        let mut grant = VideoGrant::default();
        grant.update_from_permission(&permission);
        assert!(grant.matches_permission(&permission));
        assert_eq!(grant.to_permission(), permission);

        // every tri-state is explicit afterwards, since the permission message
        // has no "unset"
        assert_eq!(grant.can_publish, Some(false));
        assert_eq!(grant.can_update_own_metadata, Some(true));
    }

    #[test]
    fn a_default_grant_does_not_match_a_restrictive_permission() {
        let permission = ParticipantPermission {
            can_publish: false,
            ..ParticipantPermission::default()
        };
        assert!(!VideoGrant::default().matches_permission(&permission));
    }

    #[test]
    fn unknown_kinds_fall_back_to_standard() {
        let mut grants = ClaimGrants {
            kind: "wizard".to_owned(),
            ..ClaimGrants::default()
        };
        assert_eq!(grants.participant_kind(), participant_info::Kind::Standard);

        grants.set_participant_kind(participant_info::Kind::Egress);
        assert_eq!(grants.kind, "egress");
        assert_eq!(grants.participant_kind(), participant_info::Kind::Egress);
    }

    #[test]
    fn empty_grants_serialise_to_an_empty_object() {
        // `omitempty` on every field means a grant with nothing set adds
        // nothing to the token.
        let json = serde_json::to_string(&ClaimGrants::default()).unwrap();
        assert_eq!(json, "{}");
        let json = serde_json::to_string(&VideoGrant::default()).unwrap();
        assert_eq!(json, "{}");
    }
}
