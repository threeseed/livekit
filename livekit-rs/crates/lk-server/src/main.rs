//! The `livekit-server` binary: CLI, subcommands, wiring and graceful shutdown.
//!
//! Replaces the Go package: `cmd/server`.
//!
//! `cmd/test-server` is deliberately not ported; it stays a Go binary (plan
//! section 9.3).

fn main() {
    // Phase 1 replaces this with the clap-derived CLI from `lk-config`.
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
}
