//! API-key authentication middleware.
//!
//! Ports `pkg/service/auth.go`. A token may arrive in the `Authorization`
//! header as a bearer token or in the `access_token` query parameter, in that
//! order, because a browser cannot set headers on a WebSocket handshake.
//!
//! A request with no token at all is passed through without grants: the Go
//! middleware does the same, and the handlers that need a grant check for one.
//! That is what lets the health and validate endpoints work unauthenticated.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use lk_auth::{ApiKeyTokenVerifier, ClaimGrants, KeyProvider};

use crate::error::Error;

const AUTHORIZATION_HEADER: &str = "authorization";
const BEARER_PREFIX: &str = "Bearer ";
const ACCESS_TOKEN_PARAM: &str = "access_token";

/// The verified grants a request carries, stored in its extensions.
#[derive(Clone, Debug, PartialEq)]
pub struct Grants {
    /// The claims the token carried.
    pub claims: ClaimGrants,
    /// The API key the token was signed with.
    pub api_key: String,
    /// The token's `exp`, as seconds since the Unix epoch.
    pub expires_at: Option<i64>,
}

impl Grants {
    /// The room the token allows joining.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PermissionDenied`] when the token carries no video
    /// grant or does not allow joining.
    pub fn ensure_join_permission(&self) -> Result<String, Error> {
        let video = self.claims.video.as_ref().ok_or(Error::PermissionDenied)?;
        if video.room_join {
            Ok(video.room.clone())
        } else {
            Err(Error::PermissionDenied)
        }
    }

    /// Checks that the token administers `room`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PermissionDenied`] when it does not.
    pub fn ensure_admin_permission(&self, room: &str) -> Result<(), Error> {
        let video = self.claims.video.as_ref().ok_or(Error::PermissionDenied)?;
        if video.room_admin && video.room == room {
            Ok(())
        } else {
            Err(Error::PermissionDenied)
        }
    }

    /// Checks that the token may create rooms.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PermissionDenied`] when it may not.
    pub fn ensure_create_permission(&self) -> Result<(), Error> {
        let video = self.claims.video.as_ref().ok_or(Error::PermissionDenied)?;
        if video.room_create {
            Ok(())
        } else {
            Err(Error::PermissionDenied)
        }
    }
}

/// The key provider the middleware verifies against.
pub type SharedKeyProvider = Arc<dyn KeyProvider>;

/// Verifies the token a request carries and puts the grants in its extensions.
///
/// # Errors
///
/// Returns 401 when a token is present but malformed, signed by an unknown key,
/// or does not verify. A request with no token passes through.
pub async fn api_key_auth(
    axum::extract::State(provider): axum::extract::State<SharedKeyProvider>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = match extract_token(request.headers(), request.uri().query()) {
        Ok(token) => token,
        Err(err) => return err.into_response(),
    };

    if let Some(token) = token {
        let verifier = match ApiKeyTokenVerifier::parse(token) {
            Ok(verifier) => verifier,
            Err(_) => return Error::InvalidAuthorizationToken(String::new()).into_response(),
        };
        let Some(secret) = provider.secret(verifier.api_key()) else {
            return Error::InvalidApiKey.into_response();
        };
        let claims = match verifier.verify(&secret) {
            Ok(claims) => claims,
            Err(err) => {
                return Error::InvalidAuthorizationToken(format!(": {err}")).into_response();
            }
        };
        request.extensions_mut().insert(Grants {
            claims: claims.grants,
            api_key: verifier.api_key().to_owned(),
            expires_at: claims.exp,
        });
    }

    next.run(request).await
}

/// The token a request carries, from the header or the query string.
///
/// # Errors
///
/// Returns [`Error::MissingAuthorization`] when an `Authorization` header is
/// present but is not a bearer token. The Go middleware rejects that rather
/// than falling back to the query parameter, so a client sending a malformed
/// header is told about it instead of silently failing later.
pub fn extract_token<'a>(
    headers: &'a HeaderMap,
    query: Option<&'a str>,
) -> Result<Option<&'a str>, Error> {
    if let Some(value) = headers.get(AUTHORIZATION_HEADER) {
        let value = value.to_str().map_err(|_| Error::MissingAuthorization)?;
        return match value.strip_prefix(BEARER_PREFIX) {
            Some(token) => Ok(Some(token)),
            None => Err(Error::MissingAuthorization),
        };
    }

    Ok(query.and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == ACCESS_TOKEN_PARAM).then_some(value)
        })
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn a_bearer_header_wins_over_the_query_parameter() {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION_HEADER,
            HeaderValue::from_static("Bearer from-header"),
        );
        assert_eq!(
            extract_token(&headers, Some("access_token=from-query")).unwrap(),
            Some("from-header")
        );
    }

    #[test]
    fn a_non_bearer_header_is_refused_rather_than_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION_HEADER, HeaderValue::from_static("Basic abc"));
        assert_eq!(
            extract_token(&headers, Some("access_token=from-query")),
            Err(Error::MissingAuthorization)
        );
    }

    #[test]
    fn the_query_parameter_is_used_when_there_is_no_header() {
        let headers = HeaderMap::new();
        assert_eq!(
            extract_token(&headers, Some("sdk=js&access_token=tok&protocol=17")).unwrap(),
            Some("tok")
        );
        assert_eq!(extract_token(&headers, Some("sdk=js")).unwrap(), None);
        assert_eq!(extract_token(&headers, None).unwrap(), None);
    }
}
