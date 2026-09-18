//! JWT access tokens, `ClaimGrants`, `KeyProvider` and API-key middleware.
//!
//! Replaces the Go packages: `protocol/auth`
//!
//! See `docs/RUST_PORT_PLAN.md` for the component tables behind this mapping.
//!
//! # Wire compatibility
//!
//! Tokens are the one thing every LiveKit SDK produces and this server must
//! accept, so the format is fixed on both sides: HS256, the registered claims
//! `iss`, `sub`, `iat`, `nbf` and `exp`, and the LiveKit grants flattened into
//! the same object under their camelCase names. A token minted by
//! `livekit-cli create-join-token` verifies here, and one minted here verifies
//! against the Go server.
//!
//! ```
//! use lk_auth::{AccessToken, ApiKeyTokenVerifier, VideoGrant};
//!
//! let jwt = AccessToken::new("devkey", "secret")
//!     .with_identity("alice")
//!     .with_video_grant(VideoGrant {
//!         room_join: true,
//!         room: "my-room".to_owned(),
//!         ..VideoGrant::default()
//!     })
//!     .to_jwt()?;
//!
//! let verifier = ApiKeyTokenVerifier::parse(&jwt)?;
//! assert_eq!(verifier.api_key(), "devkey");
//! let claims = verifier.verify("secret")?;
//! assert_eq!(claims.grants.identity, "alice");
//! # Ok::<(), lk_auth::Error>(())
//! ```

pub mod credentials;
pub mod error;
pub mod grants;
pub mod provider;
pub mod token;
pub mod verifier;

pub use crate::error::{Error, Result};
pub use crate::grants::{
    AgentGrant, ClaimGrants, InferenceGrant, ObservabilityGrant, SipGrant, VideoGrant,
};
pub use crate::provider::{FileBasedKeyProvider, KeyProvider, SimpleKeyProvider};
pub use crate::token::{AccessToken, DEFAULT_VALID_DURATION, TokenClaims};
pub use crate::verifier::{ApiKeyTokenVerifier, TOKEN_LEEWAY_SECONDS};
