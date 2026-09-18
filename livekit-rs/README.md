# livekit-rs

The Rust port of `livekit-server` onto the sans-IO [`rtc`] core.

Phase 0 of the roadmap in [`../docs/RUST_PORT_PLAN.md`](../docs/RUST_PORT_PLAN.md):
workspace, protobuf codegen, the UDP mux and PeerConnection driver, and the
deterministic network simulator. There is no server here yet; Phase 1 builds
one on top of this.

## Layout

| Path | What it is |
|---|---|
| `crates/lk-proto` | All 43 `livekit/protocol` protos, plus twirp and psrpc service definitions |
| `crates/lk-rtcio` | UDP mux, shard driver over `rtc`, and the `vnet` simulator |
| `crates/lk-*` | Stubs for the remaining crates, each naming the Go packages it replaces |
| `xtask/` | `cargo xtask proto-sync` |
| `tests/` | Cross-crate integration tests |
| `fuzz/` | `cargo-fuzz` targets |
| `benches/` | Criterion benchmarks |

## Getting started

```bash
cargo build --workspace
cargo nextest run --workspace     # or `cargo test --workspace`
cargo xtask proto-sync --check    # the vendored protos match the Go server
```

No `protoc` is needed: [`protox`] compiles the protos in-process, so the
toolchain cannot drift between a contributor's machine and CI.

## The two things worth reading first

**The mux and the driver** (`crates/lk-rtcio`). The `webrtc` async facade binds
one socket set per `PeerConnection`, has no UDP mux, wraps each connection in a
mutex, and drops inbound RTP on a 256-slot queue. An SFU serving thousands of
connections behind one published UDP port cannot live with any of those, so
`lk-rtcio` drives the sans-IO core directly: one socket, demultiplexed by
5-tuple with STUN ufrag for first packets, into one shard that owns `!Send`
connection state on one thread.

**The simulator** (`crates/lk-rtcio/src/vnet`). Configurable delay, jitter,
random and burst loss, reorder, a bandwidth cap backed by a real queue, and
full-cone or symmetric NAT, all on a virtual clock. Every run is a pure
function of its seed, so a CI failure replays from
`LK_VNET_SEED=<n> cargo test`.

## Phase 0 exit gate

`crates/lk-rtcio/tests/udp_mux_bringup.rs`: 500 concurrent peers complete ICE,
DTLS and SRTP against one shard on one UDP port, with no dropped datagrams, in
well under the one-second budget.

```
500 peers: setup 254ms, drive 444ms, 12 steps, 3500 shard transmits
```

Still open from the gate: the same handshake against a real Chrome client,
which needs a browser harness and lands with the `interop` CI job in Phase 1.

## Known gaps in `rtc`

`rtc` 0.21.0-rc.2 does not retransmit a lost handshake flight: one dropped
packet at the wrong moment wedges the connection in `Connecting` with no error.
This was found by the simulator and is reproduced, with no `lk-rtcio` code in
the path, by `crates/lk-rtcio/tests/rtc_handshake_loss_recovery.rs` (ignored;
it fails today).

That and the fork workflow are in [`../docs/RTC_FORK.md`](../docs/RTC_FORK.md).

[`rtc`]: https://github.com/webrtc-rs/rtc
[`protox`]: https://github.com/andrewhickman/protox
