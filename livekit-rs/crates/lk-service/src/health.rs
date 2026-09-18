//! The health endpoint.
//!
//! Ports `LivekitServer.healthCheck`. A load balancer takes a node out of
//! rotation on a non-200, and the node's own stats are what tell it whether
//! this process is still doing work: stats older than four seconds mean the
//! background worker is wedged, so the node says it is not ready rather than
//! accepting participants it cannot serve.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// How stale the node stats may be before the node reports itself unready.
pub const STATS_MAX_AGE_SECONDS: i64 = 4;

/// The node's last stats update, shared with whatever produces them.
#[derive(Clone, Debug, Default)]
pub struct NodeStats {
    updated_at: Arc<AtomicI64>,
}

impl NodeStats {
    /// Stats that have never been updated, which reads as unready.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an update at `unix_seconds`.
    pub fn set_updated_at(&self, unix_seconds: i64) {
        self.updated_at.store(unix_seconds, Ordering::Relaxed);
    }

    /// The last update, as seconds since the Unix epoch.
    #[must_use]
    pub fn updated_at(&self) -> i64 {
        self.updated_at.load(Ordering::Relaxed)
    }

    /// Whether the stats are fresh enough for this node to accept traffic.
    #[must_use]
    pub fn is_ready(&self, now_unix_seconds: i64) -> bool {
        now_unix_seconds - self.updated_at() <= STATS_MAX_AGE_SECONDS
    }
}

/// `GET /`: the health check.
pub async fn health_check(State(stats): State<NodeStats>) -> Response {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);

    if stats.is_ready(now) {
        return (StatusCode::OK, "OK").into_response();
    }

    // 406 rather than 503: it is what the Go server returns, and deployments
    // have health checks configured against it
    (
        StatusCode::NOT_ACCEPTABLE,
        format!("Not Ready\nNode Updated At {}", stats.updated_at()),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn stats_go_stale_after_four_seconds() {
        let stats = NodeStats::new();
        stats.set_updated_at(1_000);
        assert!(stats.is_ready(1_000));
        assert!(stats.is_ready(1_004));
        assert!(!stats.is_ready(1_005));
    }

    #[test]
    fn a_node_that_never_reported_is_not_ready() {
        assert!(!NodeStats::new().is_ready(1_000));
    }
}
