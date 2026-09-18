//! Room, Participant actor, transports, subscription manager, uptrack manager,
//! signalling, supervisor, dynacast, data tracks and data blobs.
//!
//! Replaces the Go packages: `pkg/rtc`, `pkg/rtc/dynacast`,
//! `pkg/rtc/supervisor`, `pkg/rtc/types`, `pkg/clientconfiguration`,
//! `protocol/sdp`
//!
//! See `docs/RUST_PORT_PLAN.md` for the component tables behind this mapping.
//!
//! # What is here
//!
//! The parts of the room layer that hold no state and answer a question about
//! a client or a description: the protocol-version gates, the per-client
//! capability gates, the client-configuration rules, and the SDP helpers. The
//! `Room` and `Participant` actors build on them.

pub mod client_config;
pub mod client_info;
pub mod error;
pub mod participant;
pub mod protocol_version;
pub mod room;
pub mod sdp;

pub use crate::client_config::StaticClientConfigurationManager;
pub use crate::client_info::ClientInfoExt;
pub use crate::error::{Error, Result};
pub use crate::participant::{CloseReason, ParticipantHandle, ParticipantParams};
pub use crate::protocol_version::{CURRENT_PROTOCOL, ProtocolVersion};
pub use crate::room::{JoinParams, RoomCloseReason, RoomHandle, RoomParams};
