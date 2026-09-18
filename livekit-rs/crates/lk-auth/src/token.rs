//! Minting access tokens.
//!
//! Ports `protocol/auth/accesstoken.go`. HS256 only, as in Go: the algorithm is
//! not negotiable on either side, which is what keeps `alg: none` and
//! algorithm-confusion attacks out.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use lk_proto::livekit::{RoomAgentDispatch, RoomConfiguration, participant_info};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::grants::{
    AgentGrant, ClaimGrants, InferenceGrant, ObservabilityGrant, SipGrant, VideoGrant,
};

/// How long a token is valid when the caller does not say.
pub const DEFAULT_VALID_DURATION: Duration = Duration::from_secs(6 * 60 * 60);

/// The JWT payload: the registered claims plus the LiveKit grants, flattened
/// into one object exactly as Go's embedded `jwt.RegisteredClaims` produces.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenClaims {
    /// `iss`: the API key the token was signed with.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub iss: String,
    /// `sub`: the participant identity.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sub: String,
    /// `jti`: an older LiveKit token carries the identity here.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub jti: String,
    /// `iat`: seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iat: Option<i64>,
    /// `nbf`: seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<i64>,
    /// `exp`: seconds since the Unix epoch. Required by the verifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
    /// The LiveKit grants.
    #[serde(flatten)]
    pub grants: ClaimGrants,
}

/// Builds and signs an access token.
///
/// ```no_run
/// # use lk_auth::{AccessToken, VideoGrant};
/// let mut grant = VideoGrant::default();
/// grant.room_join = true;
/// grant.room = "my-room".to_owned();
///
/// let jwt = AccessToken::new("devkey", "secret")
///     .with_identity("alice")
///     .with_video_grant(grant)
///     .to_jwt()?;
/// # Ok::<(), lk_auth::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct AccessToken {
    api_key: String,
    secret: String,
    grants: ClaimGrants,
    valid_for: Option<Duration>,
    allow_sensitive_credentials: bool,
}

impl AccessToken {
    /// A token signed with `api_key` and `secret`.
    #[must_use]
    pub fn new(api_key: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            secret: secret.into(),
            grants: ClaimGrants::default(),
            valid_for: None,
            allow_sensitive_credentials: false,
        }
    }

    /// Sets the participant identity, which is also the JWT `sub`.
    #[must_use]
    pub fn with_identity(mut self, identity: impl Into<String>) -> Self {
        self.grants.identity = identity.into();
        self
    }

    /// Sets the display name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.grants.name = name.into();
        self
    }

    /// Sets how long the token is valid for.
    #[must_use]
    pub fn with_valid_for(mut self, valid_for: Duration) -> Self {
        self.valid_for = Some(valid_for);
        self
    }

    /// Sets the participant kind.
    #[must_use]
    pub fn with_kind(mut self, kind: participant_info::Kind) -> Self {
        self.grants.set_participant_kind(kind);
        self
    }

    /// Sets the kind details.
    #[must_use]
    pub fn with_kind_details(mut self, details: &[participant_info::KindDetail]) -> Self {
        self.grants.set_kind_details(details);
        self
    }

    /// Sets the video grant.
    #[must_use]
    pub fn with_video_grant(mut self, grant: VideoGrant) -> Self {
        self.grants.video = Some(grant);
        self
    }

    /// Sets the SIP grant.
    #[must_use]
    pub fn with_sip_grant(mut self, grant: SipGrant) -> Self {
        self.grants.sip = Some(grant);
        self
    }

    /// Sets the agent grant.
    #[must_use]
    pub fn with_agent_grant(mut self, grant: AgentGrant) -> Self {
        self.grants.agent = Some(grant);
        self
    }

    /// Sets the inference grant.
    #[must_use]
    pub fn with_inference_grant(mut self, grant: InferenceGrant) -> Self {
        self.grants.inference = Some(grant);
        self
    }

    /// Sets the observability grant.
    #[must_use]
    pub fn with_observability_grant(mut self, grant: ObservabilityGrant) -> Self {
        self.grants.observability = Some(grant);
        self
    }

    /// Sets the participant metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: impl Into<String>) -> Self {
        self.grants.metadata = metadata.into();
        self
    }

    /// Sets the participant attributes.
    #[must_use]
    pub fn with_attributes<I, K, V>(mut self, attributes: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.grants.attributes = attributes
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        self
    }

    /// Sets the body hash used for integrity checks.
    #[must_use]
    pub fn with_sha256(mut self, sha256: impl Into<String>) -> Self {
        self.grants.sha256 = sha256.into();
        self
    }

    /// Sets the named room preset.
    #[must_use]
    pub fn with_room_preset(mut self, preset: impl Into<String>) -> Self {
        self.grants.room_preset = preset.into();
        self
    }

    /// Sets the room configuration applied if this participant creates the
    /// room.
    #[must_use]
    pub fn with_room_config(mut self, config: RoomConfiguration) -> Self {
        self.grants.room_config = Some(config);
        self
    }

    /// Sets the agent dispatches on the room configuration, creating one if the
    /// token does not carry it yet.
    #[must_use]
    pub fn with_agents(mut self, agents: Vec<RoomAgentDispatch>) -> Self {
        let config = self.grants.room_config.get_or_insert_with(Default::default);
        config.agents = agents;
        self
    }

    /// Allows a room configuration that carries storage credentials.
    ///
    /// Off by default: a token is client-visible, so an egress output holding
    /// an S3 secret or a stream key would be handed to every participant that
    /// receives it.
    #[must_use]
    pub fn allow_sensitive_credentials(mut self, allow: bool) -> Self {
        self.allow_sensitive_credentials = allow;
        self
    }

    /// The grants this token will carry.
    #[must_use]
    pub fn grants(&self) -> &ClaimGrants {
        &self.grants
    }

    /// Mutable access to the grants, for callers that build them in place.
    pub fn grants_mut(&mut self) -> &mut ClaimGrants {
        &mut self.grants
    }

    /// Signs the token.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeysMissing`] when the key or secret is empty,
    /// [`Error::SensitiveCredentials`] when the room configuration carries
    /// credentials and they were not explicitly allowed, and [`Error::Jwt`]
    /// when signing fails.
    pub fn to_jwt(&self) -> Result<String> {
        if self.api_key.is_empty() || self.secret.is_empty() {
            return Err(Error::KeysMissing);
        }
        if let Some(config) = &self.grants.room_config
            && !self.allow_sensitive_credentials
        {
            crate::credentials::check_room_configuration(config)?;
        }

        let valid_for = self.valid_for.unwrap_or(DEFAULT_VALID_DURATION);
        let now = unix_now();
        let claims = TokenClaims {
            iss: self.api_key.clone(),
            sub: self.grants.identity.clone(),
            jti: String::new(),
            iat: Some(now),
            nbf: Some(now),
            exp: Some(now.saturating_add(valid_for.as_secs() as i64)),
            grants: self.grants.clone(),
        };

        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )?;
        Ok(token)
    }
}

/// Seconds since the Unix epoch. A clock before the epoch yields zero rather
/// than panicking, since a token minted against such a clock fails validation
/// anyway.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use lk_proto::livekit::{
        AutoParticipantEgress, EncodedFileOutput, RoomEgress, S3Upload, encoded_file_output,
    };

    use super::*;
    use crate::verifier::ApiKeyTokenVerifier;

    #[test]
    fn a_token_needs_a_key_and_a_secret() {
        assert!(matches!(
            AccessToken::new("", "secret").to_jwt(),
            Err(Error::KeysMissing)
        ));
        assert!(matches!(
            AccessToken::new("devkey", "").to_jwt(),
            Err(Error::KeysMissing)
        ));
    }

    #[test]
    fn the_default_validity_is_six_hours() {
        let token = AccessToken::new("devkey", "secret")
            .with_identity("alice")
            .to_jwt()
            .unwrap();
        let claims = ApiKeyTokenVerifier::parse(&token)
            .unwrap()
            .verify("secret")
            .unwrap();
        let (Some(iat), Some(exp)) = (claims.iat, claims.exp) else {
            panic!("iat and exp must be set");
        };
        assert_eq!(exp - iat, DEFAULT_VALID_DURATION.as_secs() as i64);
    }

    #[test]
    fn an_expired_token_is_rejected() {
        // one hour in the past, well beyond the one minute of leeway
        let token = AccessToken::new("devkey", "secret")
            .with_identity("alice")
            .with_valid_for(Duration::from_secs(0))
            .to_jwt()
            .unwrap();
        let claims: TokenClaims = serde_json::from_slice(&{
            use base64::Engine as _;
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(token.split('.').nth(1).unwrap())
                .unwrap()
        })
        .unwrap();
        // exp == iat, so the token is already at its boundary; leeway keeps it
        // valid for another minute, which is the documented Go behaviour
        assert_eq!(claims.exp, claims.iat);
        assert!(
            ApiKeyTokenVerifier::parse(&token)
                .unwrap()
                .verify("secret")
                .is_ok()
        );
    }

    #[test]
    fn a_room_config_with_credentials_is_refused_unless_allowed() {
        let egress = RoomEgress {
            participant: Some(AutoParticipantEgress {
                file_outputs: vec![EncodedFileOutput {
                    output: Some(encoded_file_output::Output::S3(S3Upload {
                        secret: "shhh".to_owned(),
                        ..S3Upload::default()
                    })),
                    ..EncodedFileOutput::default()
                }],
                ..AutoParticipantEgress::default()
            }),
            ..RoomEgress::default()
        };
        let config = RoomConfiguration {
            egress: Some(egress),
            ..RoomConfiguration::default()
        };

        let token = AccessToken::new("devkey", "secret")
            .with_identity("alice")
            .with_room_config(config.clone());
        assert!(matches!(token.to_jwt(), Err(Error::SensitiveCredentials)));

        let token = AccessToken::new("devkey", "secret")
            .with_identity("alice")
            .with_room_config(config)
            .allow_sensitive_credentials(true);
        assert!(token.to_jwt().is_ok());
    }

    #[test]
    fn agents_land_on_the_room_configuration() {
        let token = AccessToken::new("devkey", "secret")
            .with_identity("alice")
            .with_agents(vec![RoomAgentDispatch {
                agent_name: "assistant".to_owned(),
                ..RoomAgentDispatch::default()
            }]);
        let config = token.grants().room_config.as_ref().unwrap();
        assert_eq!(config.agents.len(), 1);
    }
}
