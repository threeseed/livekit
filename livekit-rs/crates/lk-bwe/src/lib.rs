//! Bandwidth management: stream allocator, send-side BWE, remote (REMB) BWE, prober, trend detector.
//!
//! Replaces the Go packages: `pkg/sfu/bwe`, `pkg/sfu/bwe/{sendsidebwe,remotebwe}`, `pkg/sfu/ccutils`
//!
//! See `docs/RUST_PORT_PLAN.md` for the component tables behind this mapping.
