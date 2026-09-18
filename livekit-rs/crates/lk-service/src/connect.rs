//! Validating a connection request.
//!
//! Ports `ValidateConnectRequest` in `pkg/service/utils.go` and the parameter
//! handling in `RTCService.validateInternal`. Two shapes arrive here:
//!
//! - `/rtc`, where every setting is a query parameter, and
//! - `/rtc/v1`, where a base64url `WrappedJoinRequest` carries a protobuf
//!   `JoinRequest`, optionally gzipped.
//!
//! Both end in the same [`ParticipantInit`], which is what the room manager
//! starts a session from.

use std::collections::BTreeMap;
use std::io::Read as _;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE;
use lk_config::service::LimitConfig;
use lk_proto::livekit::{
    AddTrackRequest, ClientInfo, CreateRoomRequest, JoinRequest, ReconnectReason,
    RoomConfiguration, SessionDescription, WrappedJoinRequest, wrapped_join_request,
};
use prost::Message as _;

use crate::auth::Grants;
use crate::error::{Error, Result};

/// The decompressed size a join request may reach.
///
/// Go bounds it with `http.DefaultMaxHeaderBytes`, which is 1 MiB. The value is
/// part of the contract rather than an implementation detail: a client that
/// gzips a larger join request is refused by both servers at the same size.
pub const MAX_JOIN_REQUEST_SIZE: usize = 1 << 20;

/// Everything the server needs to start a participant's session.
///
/// Ports `routing.ParticipantInit`. It lives here while the server is
/// single-node; the multi-node router in phase 4 owns the same struct.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParticipantInit {
    /// The participant's identity, from the token.
    pub identity: String,
    /// The participant's display name, from the token.
    pub name: String,
    /// The verified grants.
    pub grants: Option<Grants>,
    /// The token's expiry, as seconds since the Unix epoch.
    pub token_expires_at: Option<i64>,
    /// The region this node is in.
    pub region: String,
    /// The room to create or join.
    pub create_room: Option<CreateRoomRequest>,
    /// Whether the client uses one peer connection for both directions, which
    /// is what `/rtc/v1` means.
    pub use_single_peer_connection: bool,
    /// Whether this is a reconnection rather than a new session.
    pub reconnect: bool,
    /// Why the client reconnected.
    pub reconnect_reason: i32,
    /// The participant sid being resumed, when reconnecting.
    pub id: String,
    /// What the client said about itself.
    pub client: Option<ClientInfo>,
    /// Subscribe the participant to every track automatically.
    pub auto_subscribe: bool,
    /// Subscribe to data tracks automatically. Unset means the room default.
    pub auto_subscribe_data_track: Option<bool>,
    /// The client adapts its subscriptions to what is on screen.
    pub adaptive_stream: bool,
    /// The client asks the server not to use ICE lite.
    pub disable_ice_lite: bool,
    /// The client allows the server to pause its subscriptions under
    /// congestion. Unset means the server default.
    pub subscriber_allow_pause: Option<bool>,
    /// Tracks the client wants to publish, replayed from the join request.
    pub add_track_requests: Vec<AddTrackRequest>,
    /// The publisher offer carried in the join request.
    pub publisher_offer: Option<SessionDescription>,
}

/// The query parameters `/rtc` accepts.
///
/// The set is the Go server's: anything else on the query string is ignored, as
/// it is there.
#[derive(Clone, Debug, Default)]
pub struct ConnectParams {
    /// Every parameter, so client info can be read from the same map.
    pub raw: BTreeMap<String, String>,
}

impl ConnectParams {
    /// Parses a query string, percent-decoding keys and values.
    #[must_use]
    pub fn parse(query: &str) -> Self {
        let mut raw = BTreeMap::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            raw.insert(percent_decode(key), percent_decode(value));
        }
        Self { raw }
    }

    /// One parameter, or an empty string.
    #[must_use]
    pub fn get(&self, key: &str) -> &str {
        self.raw.get(key).map_or("", String::as_str)
    }

    /// Whether a parameter is set at all.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.raw.contains_key(key)
    }

    /// A boolean parameter, with Go's `boolValue` rule: `1` and `true` are
    /// true, everything else is false.
    #[must_use]
    pub fn bool(&self, key: &str) -> bool {
        matches!(self.get(key), "1" | "true")
    }
}

/// Percent-decoding, plus `+` for space as a form-encoded query uses.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes.get(index) {
            Some(b'%') => {
                let hex = input
                    .get(index + 1..index + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    None => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            Some(b'+') => {
                out.push(b' ');
                index += 1;
            }
            Some(byte) => {
                out.push(*byte);
                index += 1;
            }
            None => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decodes the `join_request` parameter: base64url, then a `WrappedJoinRequest`
/// whose payload is a `JoinRequest`, plain or gzipped.
///
/// # Errors
///
/// Returns [`Error::BadRequest`] when a layer does not decode and
/// [`Error::JoinRequestTooLarge`] when the payload exceeds
/// [`MAX_JOIN_REQUEST_SIZE`], compressed or not.
pub fn decode_join_request(encoded: &str) -> Result<JoinRequest> {
    let wrapped_bytes = URL_SAFE
        .decode(encoded.as_bytes())
        .map_err(|_| Error::BadRequest("cannot base64 decode wrapped join request"))?;
    let wrapped = WrappedJoinRequest::decode(wrapped_bytes.as_slice())
        .map_err(|_| Error::BadRequest("cannot unmarshal wrapped join request"))?;

    let payload = match wrapped.compression() {
        wrapped_join_request::Compression::None => {
            if wrapped.join_request.len() > MAX_JOIN_REQUEST_SIZE {
                return Err(Error::JoinRequestTooLarge);
            }
            wrapped.join_request
        }
        wrapped_join_request::Compression::Gzip => decompress_gzip(&wrapped.join_request)?,
    };

    JoinRequest::decode(payload.as_slice())
        .map_err(|_| Error::BadRequest("cannot unmarshal join request"))
}

/// Gunzips a payload, refusing one that expands past
/// [`MAX_JOIN_REQUEST_SIZE`].
///
/// The bound is on the decompressed size, not the compressed one: a few
/// kilobytes of gzip expands into gigabytes if nothing stops it.
///
/// # Errors
///
/// Returns [`Error::JoinRequestTooLarge`] when the payload is too large and
/// [`Error::BadRequest`] when it is not valid gzip.
pub fn decompress_gzip(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoder =
        flate2::read::GzDecoder::new(compressed).take(MAX_JOIN_REQUEST_SIZE as u64 + 1);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|_| Error::BadRequest("cannot read decompressed join request"))?;
    if out.len() > MAX_JOIN_REQUEST_SIZE {
        return Err(Error::JoinRequestTooLarge);
    }
    Ok(out)
}

/// Decodes the `attributes` parameter: base64url JSON of a string map.
///
/// # Errors
///
/// Returns [`Error::BadRequest`] when it does not decode. The caller decides
/// whether that is fatal: `/rtc` ignores a bad value on a real connection and
/// rejects it on `/rtc/validate`, which is what `strict` means in Go.
pub fn decode_attributes(encoded: &str) -> Result<BTreeMap<String, String>> {
    let bytes = URL_SAFE
        .decode(encoded.as_bytes())
        .map_err(|_| Error::BadRequest("cannot decode attributes"))?;
    serde_json::from_slice(&bytes).map_err(|_| Error::BadRequest("cannot decode attributes"))
}

/// What a room allocator must answer before a session starts.
///
/// The Go server's `RoomAllocator` also creates the room; the two questions the
/// connection path asks are split out so the WS layer can be tested without
/// one, and so phase 1's room manager can implement them over a local store.
pub trait RoomAllocator: Send + Sync + 'static {
    /// Whether this room may be created or joined.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RoomNotFound`] when the room does not exist and
    /// `room.auto_create` is off.
    fn validate_create_room(&self, room_name: &str) -> Result<()>;

    /// Places the room on a node. A no-op on a single node.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LimitExceeded`] when no node can take the room.
    fn select_room_node(&self, room_name: &str) -> Result<()>;

    /// The region this node is in, for the client's candidate ordering.
    fn region(&self) -> String {
        String::new()
    }
}

/// The result of validating a connection request.
#[derive(Clone, Debug)]
pub struct ValidatedConnect {
    /// The room to join.
    pub room_name: String,
    /// The grants, after the request's own changes (publish suffix, metadata,
    /// attributes) have been applied.
    pub grants: Grants,
    /// The room-creation request the session will carry.
    pub create_room: CreateRoomRequest,
    /// The region this node is in.
    pub region: String,
}

/// The parts of a connection request that change the grants.
#[derive(Clone, Debug, Default)]
pub struct ConnectRequestParams {
    /// The room name from the request, used when the token does not name one.
    pub room_name: String,
    /// The `publish` parameter: a publish-only identity suffix.
    pub publish: String,
    /// Metadata the participant asks to set on itself.
    pub metadata: String,
    /// Attributes the participant asks to set on itself.
    pub attributes: BTreeMap<String, String>,
}

/// Validates a connection request against the token's grants and the
/// configured limits.
///
/// # Errors
///
/// Returns the failure the Go handler pairs with a status: 401 for a missing
/// or insufficient grant, 400 for an empty or oversized identity or room name,
/// 404 for an unknown room.
pub fn validate_connect_request(
    grants: Option<&Grants>,
    limits: &LimitConfig,
    params: &ConnectRequestParams,
    allocator: &dyn RoomAllocator,
) -> Result<ValidatedConnect> {
    let grants = grants.ok_or(Error::PermissionDenied)?;
    if grants.claims.video.is_none() {
        return Err(Error::PermissionDenied);
    }
    let mut grants = grants.clone();

    let room_in_token = grants.ensure_join_permission()?;

    if grants.claims.identity.is_empty() {
        return Err(Error::IdentityEmpty);
    }
    if !limits.check_participant_identity_length(&grants.claims.identity) {
        return Err(Error::ParticipantIdentityExceedsLimits(
            limits.max_participant_identity_length,
        ));
    }

    let room_name = if room_in_token.is_empty() {
        params.room_name.clone()
    } else {
        room_in_token
    };
    if room_name.is_empty() {
        return Err(Error::NoRoomName);
    }
    if !limits.check_room_name_length(&room_name) {
        return Err(Error::RoomNameExceedsLimits(limits.max_room_name_length));
    }

    // A publish-only connection is a second connection for a participant that
    // is already in the room, so it gets its own identity and no subscriptions.
    if !params.publish.is_empty() {
        let video = grants
            .claims
            .video
            .as_mut()
            .ok_or(Error::PermissionDenied)?;
        if !video.can_publish() {
            return Err(Error::PermissionDenied);
        }
        video.can_subscribe = Some(false);
        grants.claims.identity = format!("{}#{}", grants.claims.identity, params.publish);
    }

    allocator.validate_create_room(&room_name)?;

    let mut create_room = CreateRoomRequest {
        name: room_name.clone(),
        room_preset: grants.claims.room_preset.clone(),
        ..CreateRoomRequest::default()
    };
    if let Some(configuration) = grants.claims.room_config.clone() {
        apply_room_configuration(&mut create_room, &configuration);
    }

    if !params.metadata.is_empty() {
        if !can_update_own_metadata(&grants) {
            return Err(Error::PermissionDenied);
        }
        grants.claims.metadata = params.metadata.clone();
    }

    if !params.attributes.is_empty() {
        if !can_update_own_metadata(&grants) {
            return Err(Error::PermissionDenied);
        }
        for (key, value) in &params.attributes {
            // an empty value would delete an attribute the token set, so it is
            // dropped rather than applied
            if value.is_empty() {
                continue;
            }
            grants.claims.attributes.insert(key.clone(), value.clone());
        }
    }

    Ok(ValidatedConnect {
        room_name,
        create_room,
        region: allocator.region(),
        grants,
    })
}

fn can_update_own_metadata(grants: &Grants) -> bool {
    grants
        .claims
        .video
        .as_ref()
        .is_some_and(|video| video.can_update_own_metadata())
}

/// Copies the token's room configuration onto the create-room request, as
/// `SetRoomConfiguration` does.
pub fn apply_room_configuration(
    create_room: &mut CreateRoomRequest,
    configuration: &RoomConfiguration,
) {
    create_room.agents = configuration.agents.clone();
    create_room.egress = configuration.egress.clone();
    create_room.empty_timeout = configuration.empty_timeout;
    create_room.departure_timeout = configuration.departure_timeout;
    create_room.max_participants = configuration.max_participants;
    create_room.min_playout_delay = configuration.min_playout_delay;
    create_room.max_playout_delay = configuration.max_playout_delay;
    create_room.sync_streams = configuration.sync_streams;
    create_room.metadata = configuration.metadata.clone();
    create_room.tags = configuration.tags.clone();
}

/// Builds a [`ParticipantInit`] from the `/rtc` query parameters.
#[must_use]
pub fn participant_init_from_params(
    params: &ConnectParams,
    validated: &ValidatedConnect,
    client: ClientInfo,
) -> ParticipantInit {
    let reconnect = params.bool("reconnect");
    ParticipantInit {
        identity: validated.grants.claims.identity.clone(),
        name: validated.grants.claims.name.clone(),
        grants: Some(validated.grants.clone()),
        token_expires_at: validated.grants.expires_at,
        region: validated.region.clone(),
        create_room: Some(validated.create_room.clone()),
        use_single_peer_connection: false,
        reconnect,
        reconnect_reason: params
            .get("reconnect_reason")
            .parse()
            .unwrap_or(ReconnectReason::RrUnknown as i32),
        id: if reconnect {
            params.get("sid").to_owned()
        } else {
            String::new()
        },
        client: Some(client),
        // absent means on, which is not the same as a present "false"
        auto_subscribe: if params.has("auto_subscribe") {
            params.bool("auto_subscribe")
        } else {
            true
        },
        auto_subscribe_data_track: params
            .has("auto_subscribe_data_track")
            .then(|| params.bool("auto_subscribe_data_track")),
        adaptive_stream: params.bool("adaptive_stream"),
        disable_ice_lite: params.bool("disable_ice_lite"),
        subscriber_allow_pause: params
            .has("subscriber_allow_pause")
            .then(|| params.bool("subscriber_allow_pause")),
        add_track_requests: Vec::new(),
        publisher_offer: None,
    }
}

/// Builds a [`ParticipantInit`] from a `/rtc/v1` join request.
#[must_use]
pub fn participant_init_from_join_request(
    join: JoinRequest,
    validated: &ValidatedConnect,
    client: ClientInfo,
) -> ParticipantInit {
    let settings = join.connection_settings.unwrap_or_default();
    ParticipantInit {
        identity: validated.grants.claims.identity.clone(),
        name: validated.grants.claims.name.clone(),
        grants: Some(validated.grants.clone()),
        token_expires_at: validated.grants.expires_at,
        region: validated.region.clone(),
        create_room: Some(validated.create_room.clone()),
        use_single_peer_connection: true,
        reconnect: join.reconnect,
        reconnect_reason: join.reconnect_reason,
        id: join.participant_sid.clone(),
        client: Some(client),
        auto_subscribe: settings.auto_subscribe,
        auto_subscribe_data_track: settings.auto_subscribe_data_track,
        adaptive_stream: settings.adaptive_stream,
        disable_ice_lite: settings.disable_ice_lite,
        subscriber_allow_pause: settings.subscriber_allow_pause,
        add_track_requests: join.add_track_requests,
        publisher_offer: join.publisher_offer,
    }
}
