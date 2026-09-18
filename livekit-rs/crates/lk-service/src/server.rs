//! The HTTP server.
//!
//! Ports `pkg/service/server.go`'s router and middleware stack. The order is
//! the Go one, and it matters: CORS runs before authentication so a browser's
//! preflight is answered without a token, the double-slash fix runs before
//! routing, and the body limit runs before any handler decodes a body.
//!
//! Prometheus is served from its own listener, as in Go, so scraping does not
//! need a route on the port that carries participant traffic and can carry its
//! own basic auth.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

use crate::auth::{SharedKeyProvider, api_key_auth};
use crate::health::{NodeStats, health_check};
use crate::rtc_ws::{RtcState, rtc_v0, rtc_v0_validate, rtc_v1, rtc_v1_validate};

/// How long a browser may cache a preflight, from the Go CORS options.
pub const CORS_MAX_AGE_SECONDS: u64 = 86_400;

/// Everything the HTTP server needs.
pub struct ServerConfig {
    /// The signal endpoints' state.
    pub rtc: RtcState,
    /// The key provider tokens are verified against.
    pub key_provider: SharedKeyProvider,
    /// The node's stats, for the health endpoint.
    pub node_stats: NodeStats,
    /// The largest request body accepted, from `limit.max_api_request_body_size`.
    pub max_request_body_size: i64,
}

/// Builds the public router: the signal endpoints, the health check and the
/// middleware stack.
pub fn build_router(config: ServerConfig) -> Router {
    let ServerConfig {
        rtc,
        key_provider,
        node_stats,
        max_request_body_size,
    } = config;

    let signal = Router::new()
        .route("/rtc", get(rtc_v0))
        .route("/rtc/validate", get(rtc_v0_validate))
        .route("/rtc/v1", get(rtc_v1))
        .route("/rtc/v1/validate", get(rtc_v1_validate))
        .with_state(rtc);

    let health = Router::new()
        .route("/", get(health_check))
        .with_state(node_stats);

    let mut router = signal
        .merge(health)
        .layer(axum::middleware::from_fn_with_state(
            key_provider,
            api_key_auth,
        ));

    if max_request_body_size > 0 {
        router = router.layer(RequestBodyLimitLayer::new(
            usize::try_from(max_request_body_size).unwrap_or(usize::MAX),
        ));
    }

    // The path rewrite has to happen before routing, and a layer on a router
    // runs after it, so the routed router becomes the fallback of an outer one
    // that carries the rewrite and CORS.
    Router::new()
        .fallback_service(router)
        .layer(axum::middleware::from_fn(remove_double_slashes))
        // Origin is not a security boundary here: the token is. A script served
        // from anywhere may hold a valid token, and refusing its origin would
        // only break legitimate embeds.
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
                .expose_headers(Any)
                .max_age(Duration::from_secs(CORS_MAX_AGE_SECONDS)),
        )
}

/// Collapses a leading `//` in the path.
///
/// Some clients join a base URL and a path without checking for a trailing
/// slash, and the resulting `//rtc` would otherwise 404.
async fn remove_double_slashes(mut request: Request, next: Next) -> Response {
    let uri = request.uri().clone();
    let path = uri.path().to_owned();
    if path.starts_with("//") {
        let mut parts = uri.into_parts();
        let query = parts
            .path_and_query
            .as_ref()
            .and_then(|pq| pq.query())
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        let trimmed = format!("{}{query}", &path[1..]);
        if let Ok(path_and_query) = trimmed.parse() {
            parts.path_and_query = Some(path_and_query);
            if let Ok(uri) = axum::http::Uri::from_parts(parts) {
                *request.uri_mut() = uri;
            }
        }
    }
    next.run(request).await
}

/// Renders the Prometheus exposition text. Implemented by `lk-telemetry`.
pub trait MetricsRenderer: Send + Sync + 'static {
    /// The current metrics, in the text exposition format.
    fn render(&self) -> String;
}

/// Basic-auth credentials for the metrics listener.
#[derive(Clone, Debug)]
pub struct BasicAuth {
    /// The user name a scraper must present.
    pub username: String,
    /// The password a scraper must present.
    pub password: String,
}

/// The metrics listener's state.
#[derive(Clone)]
struct MetricsState {
    renderer: Arc<dyn MetricsRenderer>,
    auth: Option<BasicAuth>,
}

/// Builds the metrics router, on its own listener as in Go.
pub fn build_metrics_router(renderer: Arc<dyn MetricsRenderer>, auth: Option<BasicAuth>) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .route("/", get(metrics))
        .with_state(MetricsState { renderer, auth })
}

async fn metrics(State(state): State<MetricsState>, request: Request) -> Response {
    if let Some(expected) = &state.auth
        && !basic_auth_matches(&request, expected)
    {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"metrics\""),
            )],
            "Unauthorized",
        )
            .into_response();
    }

    (StatusCode::OK, state.renderer.render()).into_response()
}

fn basic_auth_matches(request: &Request, expected: &BasicAuth) -> bool {
    use base64::Engine as _;

    let Some(value) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((username, password)) = decoded.split_once(':') else {
        return false;
    };
    username == expected.username && password == expected.password
}
