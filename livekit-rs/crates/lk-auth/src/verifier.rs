//! Verifying access tokens.
//!
//! Ports `protocol/auth/verifier.go`. Two steps, as in Go: parse the token
//! without checking the signature to learn which API key signed it, look that
//! key's secret up, then verify.
//!
//! The validation rules are the ones the Go verifier sets, and each matters:
//!
//! - HS256 only, so a token cannot choose its own algorithm.
//! - The issuer must equal the API key the secret was looked up under, so a
//!   token signed by one tenant cannot be replayed at another.
//! - `exp` is required. Without that rule a token minted without `exp` verifies
//!   and never expires.
//! - One minute of leeway, matching the Go verifier's `tokenLeeway`.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};

use crate::error::{Error, Result};
use crate::grants::ClaimGrants;
use crate::token::TokenClaims;

/// Clock skew tolerated on `exp` and `nbf`, matching Go's `tokenLeeway`.
pub const TOKEN_LEEWAY_SECONDS: u64 = 60;

/// A parsed but not yet verified token.
#[derive(Clone, Debug)]
pub struct ApiKeyTokenVerifier {
    raw: String,
    api_key: String,
    identity: String,
}

impl ApiKeyTokenVerifier {
    /// Parses a token without checking its signature, to learn the API key and
    /// identity it claims. Nothing read here may be trusted until
    /// [`Self::verify`] succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] when the token is not three base64url
    /// segments, and [`Error::Jwt`] when the header names an algorithm other
    /// than HS256.
    pub fn parse(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();

        let header = jsonwebtoken::decode_header(&raw)?;
        if header.alg != Algorithm::HS256 {
            return Err(Error::UnsupportedAlgorithm(format!("{:?}", header.alg)));
        }

        let mut parts = raw.split('.');
        let (_, payload) = (parts.next(), parts.next());
        let payload = payload.ok_or(Error::Malformed)?;
        if parts.next().is_none() {
            return Err(Error::Malformed);
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(payload.as_bytes())
            .map_err(|_| Error::Malformed)?;
        let claims: TokenClaims = serde_json::from_slice(&decoded).map_err(|_| Error::Malformed)?;

        let identity = if claims.sub.is_empty() {
            claims.jti
        } else {
            claims.sub
        };
        Ok(Self {
            raw,
            api_key: claims.iss,
            identity,
        })
    }

    /// The API key the token claims to be signed with.
    #[must_use]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// The identity the token claims, from `sub` or, for older tokens, `jti`.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The raw encoded token.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Verifies the signature and the time claims, and returns the payload.
    ///
    /// The identity read during parsing is written back over the verified
    /// grants, as the Go verifier does, so a token that carries its identity
    /// only in `sub` still yields one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeysMissing`] when the secret is empty, and
    /// [`Error::Jwt`] when the signature, the issuer or a time claim does not
    /// check out.
    pub fn verify(&self, secret: &str) -> Result<TokenClaims> {
        if secret.is_empty() {
            return Err(Error::KeysMissing);
        }

        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = TOKEN_LEEWAY_SECONDS;
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.set_required_spec_claims(&["exp"]);
        validation.set_issuer(&[self.api_key.as_str()]);
        // The audience is not part of a LiveKit token; requiring one here
        // would reject every token the SDKs mint.
        validation.validate_aud = false;

        let data = jsonwebtoken::decode::<TokenClaims>(
            &self.raw,
            &DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        )?;

        let mut claims = data.claims;
        claims.grants.identity = self.identity.clone();
        Ok(claims)
    }

    /// Verifies against a key provider, looking the secret up by the token's
    /// own API key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownApiKey`] when the provider has no such key, plus
    /// the errors of [`Self::verify`].
    pub fn verify_with(&self, provider: &dyn crate::provider::KeyProvider) -> Result<ClaimGrants> {
        let secret = provider
            .secret(&self.api_key)
            .ok_or_else(|| Error::UnknownApiKey(self.api_key.clone()))?;
        Ok(self.verify(&secret)?.grants)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::token::AccessToken;

    #[test]
    fn a_token_with_another_issuer_is_rejected() {
        let token = AccessToken::new("devkey", "secret")
            .with_identity("alice")
            .to_jwt()
            .unwrap();
        let mut verifier = ApiKeyTokenVerifier::parse(&token).unwrap();
        // pretend the caller looked the secret up under a different key
        verifier.api_key = "otherkey".to_owned();
        assert!(verifier.verify("secret").is_err());
    }

    #[test]
    fn a_malformed_token_is_not_a_panic() {
        assert!(ApiKeyTokenVerifier::parse("not-a-token").is_err());
        assert!(ApiKeyTokenVerifier::parse("").is_err());
        assert!(ApiKeyTokenVerifier::parse("a.b").is_err());
    }

    #[test]
    fn verification_needs_a_secret() {
        let token = AccessToken::new("devkey", "secret")
            .with_identity("alice")
            .to_jwt()
            .unwrap();
        assert!(matches!(
            ApiKeyTokenVerifier::parse(&token).unwrap().verify(""),
            Err(Error::KeysMissing)
        ));
    }

    #[test]
    fn a_token_without_exp_is_rejected() {
        // A token carrying no exp would otherwise verify and never expire.
        // Built by hand, because AccessToken always sets exp.
        use base64::Engine as _;
        use jsonwebtoken::{Algorithm, EncodingKey, Header};

        #[derive(serde::Serialize)]
        struct NoExp {
            iss: String,
            sub: String,
        }

        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &NoExp {
                iss: "devkey".to_owned(),
                sub: "alice".to_owned(),
            },
            &EncodingKey::from_secret(b"secret"),
        )
        .unwrap();

        // sanity: the payload really has no exp
        let payload = token.split('.').nth(1).unwrap();
        let decoded = URL_SAFE_NO_PAD.decode(payload).unwrap();
        assert!(!String::from_utf8_lossy(&decoded).contains("exp"));

        assert!(
            ApiKeyTokenVerifier::parse(&token)
                .unwrap()
                .verify("secret")
                .is_err()
        );
    }
}
