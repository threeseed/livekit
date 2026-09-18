//! Service errors and their HTTP status codes.
//!
//! The Go handlers pair every failure with an explicit status
//! (`HandleError(w, r, http.StatusUnauthorized, err)`), and clients branch on
//! those: a 401 makes the JS SDK stop retrying, a 503 makes it try another
//! node. The pairing is therefore part of the wire contract and lives on the
//! error type rather than at each call site.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// The result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything a signal or API request can fail with.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The token does not carry the grant the request needs.
    #[error("permissions denied")]
    PermissionDenied,

    /// The `Authorization` header is present but not a bearer token.
    #[error("invalid authorization header. Must start with Bearer ")]
    MissingAuthorization,

    /// The token did not parse or did not verify.
    #[error("invalid authorization token{0}")]
    InvalidAuthorizationToken(String),

    /// The token names an API key this server does not know.
    #[error("invalid API key")]
    InvalidApiKey,

    /// The token carries no identity.
    #[error("participant identity cannot be empty")]
    IdentityEmpty,

    /// The identity is longer than `limit.max_participant_identity_length`.
    #[error("participant identity exceeds limits: max length {0}")]
    ParticipantIdentityExceedsLimits(i32),

    /// No room name in the token or the request.
    #[error("room name is required")]
    NoRoomName,

    /// The room name is longer than `limit.max_room_name_length`.
    #[error("room name exceeds limits: max length {0}")]
    RoomNameExceedsLimits(i32),

    /// The room does not exist and `room.auto_create` is off.
    #[error("requested room does not exist")]
    RoomNotFound,

    /// The node that would serve this room is at its limit.
    #[error("node has reached its configured limit")]
    LimitExceeded,

    /// A request parameter did not decode.
    #[error("{0}")]
    BadRequest(&'static str),

    /// The join request exceeded the decompressed size limit.
    #[error("join request too large")]
    JoinRequestTooLarge,

    /// The session could not be started.
    #[error("could not start session: {0}")]
    SessionStart(String),

    /// Something failed that the client cannot act on.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// The HTTP status the Go handler pairs this failure with.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            Self::PermissionDenied
            | Self::MissingAuthorization
            | Self::InvalidAuthorizationToken(_)
            | Self::InvalidApiKey => StatusCode::UNAUTHORIZED,
            Self::IdentityEmpty
            | Self::ParticipantIdentityExceedsLimits(_)
            | Self::NoRoomName
            | Self::RoomNameExceedsLimits(_)
            | Self::BadRequest(_)
            | Self::JoinRequestTooLarge => StatusCode::BAD_REQUEST,
            Self::RoomNotFound => StatusCode::NOT_FOUND,
            Self::LimitExceeded => StatusCode::SERVICE_UNAVAILABLE,
            Self::SessionStart(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        // The Go handler writes the error text as the body, and the SDKs show
        // it to the developer, so it is reproduced rather than swallowed.
        (self.status(), self.to_string()).into_response()
    }
}
