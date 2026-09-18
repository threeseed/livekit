//! Protobuf types for all 43 `livekit/protocol` `.proto` files, plus twirp and
//! psrpc service definitions.
//!
//! Replaces the Go packages: `livekit/protocol`'s generated bindings, and the
//! codegen in `livekit/psrpc`.
//!
//! # What is generated
//!
//! Everything, which is the point. `livekit-protocol` 0.7.13 generates 10 of
//! the 43 files: no `rpc/*` (the psrpc service definitions the nodes use to
//! talk to each other), no `livekit_internal.proto`, no agent or metrics
//! protos. A Rust media node that has to join a live Go cluster needs all of
//! them.
//!
//! The sources are vendored under `protobufs/` at the revision named in
//! `protobufs/PIN`, which must match the `livekit/protocol` pseudo-version in
//! the Go server's `go.mod`. `cargo xtask proto-sync --check` enforces that and
//! the `check` CI job runs it, because `livekit/protocol` moves weekly.
//!
//! # JSON
//!
//! The signal protocol's JSON frames follow protojson semantics, including
//! discarding unknown fields on decode, so JSON goes through `pbjson` rather
//! than `prost`'s own JSON support. A `cargo deny` ban keeps the alternative
//! out of the tree.

// The generated code is not ours to lint. The rustdoc allows cover proto
// comments that were never written for rustdoc: SIP protos document `INVITE
// <uri>`, which rustdoc reads as an unclosed HTML tag.
#![allow(
    rustdoc::invalid_html_tags,
    rustdoc::bare_urls,
    rustdoc::broken_intra_doc_links,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    unreachable_pub
)]

/// `package livekit`: the client-facing protocol, models, rooms, egress,
/// ingress, SIP, agents, analytics, metrics and webhooks.
pub mod livekit {
    include!(concat!(env!("OUT_DIR"), "/livekit.rs"));
    include!(concat!(env!("OUT_DIR"), "/livekit.serde.rs"));

    /// `package livekit.agent`.
    pub mod agent {
        include!(concat!(env!("OUT_DIR"), "/livekit.agent.rs"));
        include!(concat!(env!("OUT_DIR"), "/livekit.agent.serde.rs"));
    }
}

/// `package rpc`: the psrpc service definitions nodes use over Redis.
pub mod rpc {
    include!(concat!(env!("OUT_DIR"), "/rpc.rs"));
    include!(concat!(env!("OUT_DIR"), "/rpc.serde.rs"));
}

/// `package psrpc`: the service options the `rpc` protos are annotated with.
pub mod psrpc_options {
    include!(concat!(env!("OUT_DIR"), "/psrpc.rs"));
    include!(concat!(env!("OUT_DIR"), "/psrpc.serde.rs"));
}

/// `package logger`.
pub mod logger {
    include!(concat!(env!("OUT_DIR"), "/logger.rs"));
    include!(concat!(env!("OUT_DIR"), "/logger.serde.rs"));
}

/// Twirp service traits and routing tables for the five HTTP services.
pub mod twirp {
    include!(concat!(env!("OUT_DIR"), "/twirp.rs"));
}

/// psrpc method tables and topic helpers for the `rpc/*.proto` services.
pub mod psrpc {
    include!(concat!(env!("OUT_DIR"), "/psrpc_services.rs"));

    pub mod channel;
    pub use channel::{Channel, ChannelNames};
}

/// The encoded `FileDescriptorSet` for every vendored proto.
///
/// Served by `lk-service` for reflection, and diffed against the Go server's in
/// the interop suite: a descriptor difference is the earliest possible warning
/// that the two implementations have drifted apart on the wire.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/livekit_descriptor.bin"));

/// The `livekit/protocol` revision the vendored sources came from.
pub const PROTOCOL_REVISION: &str = "a879e945e713e966f17c204ff06873f2aeaa9048";
