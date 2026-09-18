//! UDP/TCP mux, shard reactors and the PeerConnection driver over the sans-IO
//! `rtc` core.
//!
//! Replaces the Go packages: `pkg/rtc/transport`, `pkg/rtc/transportmanager`,
//! `pkg/rtc/types/ice`, and the pion internals the Go server relies on.
//!
//! # Why not the `webrtc` async facade
//!
//! The facade binds one socket set per `PeerConnection`, has no UDP or TCP mux,
//! wraps each PC in a `Mutex`, and drops inbound RTP on a 256-slot queue. An SFU
//! serving thousands of PCs behind one published UDP port cannot live with any
//! of those. See issue #982 and plan section 2.1 for the evidence table.
//!
//! # Shape
//!
//! ```text
//! recv batch -> [UdpMux demux] -> Shard::handle_datagram -> rtc::handle_read
//!                                       |
//!            poll_media <- poll_read ---+--- poll_write -> GSO send batch
//!                                       |
//!                                  poll_timeout -> timer wheel
//! ```
//!
//! [`Shard`] is the sans-IO half: it owns `!Send` PeerConnection state on one
//! thread, takes datagrams in and hands datagrams out, and never touches a
//! socket. That is what lets the same driver run over a real socket
//! ([`udp`]) and over the deterministic simulator ([`vnet`]) with no
//! conditional code on the media path.
//!
//! Phase 0 drives exactly one shard. Sharding, cross-shard forwarding and the
//! TCP mux are webrtc-rs gaps #1 and #2, tracked in the Phase 1 epic.

pub mod clock;
pub mod datagram;
pub mod error;
pub mod mux;
pub mod shard;
pub mod stun;
pub mod vnet;

#[cfg(feature = "udp")]
pub mod udp;

pub use clock::{Clock, ManualClock, SystemClock};
pub use datagram::{Datagram, Proto};
pub use error::{Error, Result};
pub use mux::{MuxStats, Route, UdpMux, Unroutable};
pub use shard::{PcIndex, Shard, ShardConfig, ShardEvent, ShardStats};
