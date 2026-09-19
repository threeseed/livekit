//! HTTP server, WS signal endpoints, twirp services, `RoomManager`, stores,
//! agents, WHIP and the egress, ingress and SIP glue.
//!
//! Replaces the Go packages: `pkg/service`
//!
//! See `docs/RUST_PORT_PLAN.md` for the component tables behind this mapping.
//!
//! # What is here
//!
//! The front door: the HTTP server and its middleware, API-key authentication,
//! and the `/rtc` and `/rtc/v1` signal endpoints with their validation,
//! framing and ping handling. The room manager plugs into it through
//! [`rtc_ws::SessionStarter`] and [`connect::RoomAllocator`].

pub mod auth;
pub mod client_info;
pub mod connect;
pub mod error;
pub mod health;
pub mod room_allocator;
pub mod room_manager;
pub mod rtc_ws;
pub mod server;
pub mod store;
pub mod ws;

pub use crate::auth::{Grants, SharedKeyProvider};
pub use crate::connect::{ParticipantInit, RoomAllocator};
pub use crate::error::{Error, Result};
pub use crate::health::NodeStats;
pub use crate::room_manager::RoomManager;
pub use crate::rtc_ws::{RtcState, SessionStarter, StartedSession};
pub use crate::server::{ServerConfig, build_metrics_router, build_router};
pub use crate::store::{LocalStore, ObjectStore};
