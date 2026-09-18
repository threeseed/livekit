# LiveKit server: Rust port plan (webrtc-rs core)

Date: 2026-09-18
Baseline: `livekit-server` at 9feb4c7 (Go 1.26, pion/webrtc v4.2.18 + livekit "warp" forks),
`webrtc` 0.21.0-rc.2 async facade, `rtc` 0.21.0-rc.2 sans-IO core (webrtc-rs/rtc at 4239564).

> **Where the code lives.** The `livekit-rs/` workspace this plan describes is in the
> [`harana/harana-matrix`](https://github.com/harana/harana-matrix) repository, under
> `livekit-rs/`, along with the `rtc` fork runbook at `docs/RTC_FORK.md`. This repository holds
> the Go server being ported and this plan. Phase 0 is done bar the browser-interop half of its
> exit gate; see harana/harana-matrix#972.

## Summary

- Port the server as a workspace of ~12 crates on top of the sans-IO `rtc` core, with a
  LiveKit-owned UDP/TCP mux and driver instead of the `webrtc` async facade. The facade binds
  one socket set per PeerConnection, has no UDP mux, and drops inbound RTP on a 256-slot queue.
- Keep wire compatibility on every external boundary (signal protobuf v17, twirp, psrpc over
  Redis, webhooks, YAML config, Prometheus names). This lets Rust media nodes join a Go cluster
  one node at a time, which is the cutover strategy.
- Twelve webrtc-rs gaps must be closed before media parity, the largest being UDP/TCP mux,
  rrid RTX pairing for simulcast, dependency descriptor, RED, abs-capture-time, an embedded
  TURN server, and a sender-side RTCP read path. Total port size is 85 kLOC of Go (non-test)
  plus 27 kLOC of tests, dominated by `pkg/rtc` (21 kLOC) and `pkg/sfu` (35 kLOC with subpackages).

---

## 1. What is being ported

### 1.1 Source size (non-test Go LOC, generated fakes excluded)

| Area | Packages | LOC | Tests LOC | Notes |
|---|---|---|---|---|
| Media (SFU) | `pkg/sfu/**` | ~35,000 | ~13,900 | buffer 4.4k, rtpstats 4.4k, forwarder 2.5k, downtrack 2.8k, sendsidebwe 2.3k, streamallocator 1.8k |
| RTC (room, participant, transport) | `pkg/rtc/**` | ~25,400 | ~5,600 | participant*.go 6.1k, transport.go 3.4k, room.go 2.4k, subscriptionmanager 1.7k |
| Service (HTTP, twirp, room manager, stores, TURN) | `pkg/service` | 11,700 | 2,900 | wire DI, RoomManager.StartSession, RedisStore 1.1k, SIP 0.8k, WHIP 0.5k |
| Routing (psrpc relay, node selection) | `pkg/routing/**` | 2,100 | 780 | signal relay with seq numbers, Redis router |
| Telemetry, metrics, prometheus | `pkg/telemetry/**`, `pkg/metric` | 3,800 | 1,100 | 30+ metric families |
| Agents | `pkg/agent` | 960 | 260 | worker WS protocol v1 |
| Config, CLI, utils, clientconfiguration | `pkg/config`, `cmd/server`, `pkg/utils`, `pkg/clientconfiguration` | 2,600 | 830 | tengo scripts in clientconfiguration |
| Integration tests + test client | `test/**` | 2,800 | 3,800 | real pion client over localhost |
| test-server (SDK CI mock) | `cmd/test-server` | 1,500 | 450 | NOT ported (see 9.3) |
| Total | | ~85,100 | ~27,300 | 379 Test funcs, 6 benchmarks, 0 fuzz targets |

### 1.2 External dependencies the Go server relies on

| Go dependency | What LiveKit uses | Rust status |
|---|---|---|
| `livekit/protocol` (protobufs, auth, webhook, utils, rpc stubs) | 236 import sites; SignalRequest/Response, Room/Participant/Track protos, twirp servers, psrpc stubs, JWT grants, webhook signing, TimedVersion, ProtoProxy | `livekit-protocol` 0.7.13 crate (prost + pbjson) exists but generates only 10 of 43 protos: no `rpc/*.proto` (psrpc), no `livekit_internal.proto`, no `livekit_agent*`, no `livekit_metrics`. Must regenerate from the full proto set. |
| `livekit/mediatransportutil` | NtpTime, packet Bucket, NACK queue, TWCC responder, VP8 descriptor parser, `rtcconfig` (UDP/TCP mux, ICE servers, NAT rewrite) | No Rust equivalent. Port (~3-4 kLOC). |
| `livekit/psrpc` | Redis/local message bus, typed RPC, streams (RelaySignal), pub/sub, keepalive | No Rust equivalent (crates.io search: 0 hits). Port with wire compatibility (see 4.8). |
| pion/webrtc + livekit forks | PeerConnection, SettingEngine (23 options incl. fork-only `EnableSped`, `EnableSctpSnap`, `SetIgnoreRidPauseForRecv`, `SetFireOnTrackBeforeFirstRTP`, `DisableCloseByDTLS`), `BufferFactory` hijack | `rtc` core: see section 3 gaps |
| pion/interceptor (cc, gcc, twcc) | optional pion GCC path | `rtc-interceptor` has GCC, TWCC, NACK, pacing |
| pion/turn/v5 | embedded TURN server (UDP + TLS + proxy protocol, HMAC creds, quotas) | `rtc-turn` is client-only |
| twirp | 5 services, 50 methods | `twirp` 0.11 crate (async, hyper) + `twirp-build` |
| gorilla/websocket | `/rtc`, `/rtc/v1`, `/agent` | `tokio-tungstenite` |
| go-redis/v9 | stores, router, bus | `fred` or `redis` crate (sentinel + cluster required) |
| prometheus/client_golang | metrics | `prometheus-client` or `metrics` + `metrics-exporter-prometheus` |
| zap via protocol/logger | structured logs, deferred fields | `tracing` + `tracing-subscriber` (JSON) |
| google/wire | DI graph | plain constructor functions; no DI framework |
| tengo | clientconfiguration scripts | replace with Rust match rules (3 rules today) |
| counterfeiter | 38 kLOC generated fakes for 35 interfaces | `mockall` |

---

## 2. Target architecture

### 2.1 Core decision: sans-IO `rtc` core, LiveKit-owned I/O

Use `rtc` (the sans-IO core) directly. Do not build on the `webrtc` async facade.

| Facade property (verified in /home/user/webrtc) | Why it blocks an SFU |
|---|---|
| One driver task and one socket set per PeerConnection (`driver.rs:434 bind_transports`) | LiveKit serves thousands of PCs behind one published UDP port (`rtc.udp_port`) |
| UDP mux and TCP mux are commented-out todos (`setting_engine.rs:656-661`, `:1151`) | ICE-TCP and single-port operation are required for firewall traversal |
| Inbound RTP per track goes through a 256-slot `try_send` queue that drops on overflow (`driver.rs:97`, `:1349`) | An SFU cannot lose publisher packets on a burst; lifecycle events share the queue |
| One `Mutex<RTCPeerConnection>` per PC serialises driver and application (`peer_connection/mod.rs:731`) | Contention on the hot path; LiveKit's forwarder must not block on the PC lock |
| `RtpSender`, `RtpReceiver`, `PeerConnection` traits are sealed | Cannot add `read_rtcp` or bypass `write_rtp` validation downstream |

The sans-IO core is the right shape: `handle_read(TaggedBytesMut)` / `poll_write` / `poll_read` /
`poll_event` / `poll_timeout`, all clock-injected (`docs/sans-io-deterministic-time.md`). It never
binds a socket, so a shared-socket mux is the application's job by design. This also gives
deterministic simulation testing for free (section 8.4).

Alternative considered: `str0m` 0.23 (sans-IO, SFU-oriented, has its own mux story). Rejected
for this plan because the user's forks are of webrtc-rs, and `rtc` has the broader protocol
surface (DTLS, SCTP, TURN client, interceptors) already split into crates that can be forked
individually. Revisit only if the `rtc` gap list in section 3 proves unfundable.

### 2.2 Process and threading model

| Layer | Runtime | Rationale |
|---|---|---|
| Media shards | N OS threads (N = cores reserved for media), each a `tokio` current-thread runtime or a hand-rolled epoll loop | Mirrors the facade's `spawn_reactor` thread-confinement idea; per-shard `!Send` state avoids locks on the packet path |
| UDP I/O | One `quinn-udp` socket per bind address, `SO_REUSEPORT` fanned across shards, GRO/GSO batch recv/send | Kernel spreads flows across shards by 4-tuple hash; each shard demuxes by (peer addr, local addr) then by ICE ufrag for STUN |
| PC placement | A PC lives on exactly one shard for its lifetime; packets for a PC that land on another shard are forwarded via SPSC ring to the owning shard | Cheaper than migrating PC state; forwarding rate is bounded because the kernel hash is stable |
| Cross-PC media fan-out (publisher -> subscribers) | `bytes::Bytes` payload + small header struct, pushed into per-subscriber-shard MPSC queues; batched per tick | Replaces `DownTrackSpreader` goroutine fan-out; zero-copy payload sharing |
| Control plane (rooms, participants, signalling, RPC, HTTP) | Multi-thread `tokio` runtime | Latency-tolerant, IO-bound |
| Participant | One actor task with an `mpsc` command queue (replaces `TypedOpsQueue` + 8 mutexes in `DownTrack` + 25 atomics in `ParticipantImpl`) | Removes the lock-ordering bug class the Go CI guards against with `go-deadlock` |
| Clock | `Instant` injected everywhere below the actor layer; a `Clock` trait with real and virtual implementations | Required by `rtc` and by deterministic tests |

### 2.3 Workspace layout

```
livekit-rs/
  Cargo.toml                (workspace, edition 2024, MSRV pinned)
  crates/
    lk-proto        protobuf codegen from livekit/protocol (all 43 .proto), prost + pbjson-serde
    lk-auth         JWT access tokens, ClaimGrants, KeyProvider, API-key middleware
    lk-config       YAML + CLI flags (clap), defaults, validation, config-sample parity
    lk-bus          psrpc port: local bus, Redis bus, typed RPC, streams, pub/sub, keepalive
    lk-routing      Router (local, redis), node registry, selectors, signal relay sink/source
    lk-rtcio        UDP/TCP mux, shard reactors, PC driver over `rtc`, ICE-TCP framing, TURN client glue
    lk-media        ports of mediatransportutil: NtpTime, Bucket, NACK queue, TWCC responder, VP8 parser
    lk-rtpext       dependency descriptor (reader/writer), abs-capture-time, playout-delay, packet trailer
    lk-sfu          buffer, rtpstats, forwarder, downtrack, sequencer, mungers, pacer, layer selectors,
                    stream trackers, connection quality, RED, audio level, datachannel writers
    lk-bwe          stream allocator, send-side BWE, remote (REMB) BWE, prober, trend detector
    lk-room         Room, Participant actor, transports, subscription manager, uptrack manager,
                    signalling, supervisor, dynacast, data tracks, data blobs
    lk-service      HTTP server (axum/hyper), WS signal endpoints, twirp services, RoomManager,
                    RoomAllocator, stores (local, redis), agents, WHIP, egress/ingress/SIP glue, IOInfo
    lk-turn         embedded TURN server (UDP, TCP, TLS, proxy protocol, HMAC creds, quotas, CIDR policy)
    lk-telemetry    events, webhooks, analytics stats worker, prometheus families, OTel
    lk-testclient   Rust integration client (signal WS + rtc PC), interceptor hooks, synthetic tracks
    lk-server       binary: CLI, subcommands, wiring, graceful shutdown
  xtask/            codegen, fake regeneration, lint bundle, proto sync
  tests/            cross-crate integration tests (single node, multi node, interop)
  fuzz/             cargo-fuzz targets
  benches/          criterion + closed-loop simulations
```

### 2.4 Dependency choices

| Concern | Crate | Reason |
|---|---|---|
| Async runtime | `tokio` 1.x | `rtc` facade already targets it; ecosystem |
| UDP batching | `quinn-udp` 0.6 | GSO/GRO, ECN, already a dep of `webrtc` |
| WebRTC core | `rtc` 0.21 (forked, see section 3) | sans-IO |
| Protobuf | `prost` 0.14 + `pbjson` (JSON signalling frames must match protojson semantics: DiscardUnknown) | Same toolchain `livekit-protocol` uses |
| Twirp | `twirp` 0.11 + `twirp-build` | Actively maintained (2026-07) |
| HTTP | `axum` 0.8 on `hyper` 1 | Router, WS upgrade, middleware (CORS, body limit, request ID) |
| WebSocket | `tokio-tungstenite` | permessage-deflate not required (LiveKit uses gzip inside `join_request`) |
| Redis | `fred` (sentinel, cluster, TLS, pipelining) | Needed for RedisStore + bus + router |
| JWT | `jsonwebtoken` (HS256 only, as LiveKit) | |
| Logging | `tracing` + JSON layer; span fields for room/participant; deferred field pattern via span records | Replaces zap `WithDeferredValues` |
| Metrics | `prometheus-client` | Exact family/label parity with Go (section 9.4) |
| Tracing export | `opentelemetry` + `opentelemetry-otlp` (Jaeger via OTLP) | |
| Config | `serde_yaml` + `clap` derive; flag generation from struct via a derive macro in `xtask` | Go auto-generates a CLI flag per config field |
| Mocks | `mockall` | Replaces counterfeiter |
| Property tests | `proptest` | BWE, allocator, mungers |
| Fuzzing | `cargo-fuzz` (libFuzzer) | DD, VP8 descriptor, RED, SDP munging, signal frames |
| Benchmarks | `criterion` | plus CI perf gate |
| Sim testing | `turmoil` for control plane (TCP/Redis-free actors); custom vnet over `rtc` sans-IO for media | |
| Containers in tests | `testcontainers` (Redis fallback to localhost:6379, as Go does) | |
| Deadlock guard | `parking_lot` with `deadlock_detection` in CI feature | Preserves the `go-deadlock` CI intent |

---

## 3. webrtc-rs work items (must be done in the forks)

Verified against the checkouts. "Fork" means change in threeseed/webrtc or webrtc-rs/rtc fork;
"lk" means implement in the LiveKit workspace without touching the core.

| # | Gap | Evidence | Where | Effort | Blocks |
|---|---|---|---|---|---|
| 1 | UDP mux: one socket, N PCs; demux by 5-tuple, STUN ufrag for first packets | `setting_engine.rs:656-661` todo; core never binds sockets | lk (`lk-rtcio`) | M | Phase 1 |
| 2 | TCP mux for ICE-TCP passive (RFC 4571 framing, shared listener) | `setting_engine.rs:1151` commented out; facade `tcp_transport.rs` is per-PC | lk (`lk-rtcio`), reuse `shared::tcp_framing` | M | Phase 1 |
| 3 | Sender-side RTCP delivery (PLI/FIR/NACK/RR/XR/TWCC/REMB addressed to our SSRCs) | `NoopInterceptor` drops RTCP without `DeliverToApplication` (`rtc-interceptor/src/noop.rs:91`); `endpoint.rs:338-356` already maps sender SSRC to track id | lk: an SFU interceptor at the last slot that tags everything; no fork needed | S | Phase 1 |
| 4 | Relax `RTCRtpSender::write_rtp` validation (SSRC, PT, ext ids must be pre-negotiated) | `rtp_sender/mod.rs:407,422,443,466` | fork `rtc`: add `SettingEngine` flag `permissive_sender_writes` or a raw-write entry point | S | Phase 2 |
| 5 | rrid (repaired-rtp-stream-id) RTX pairing for RID simulcast | `handler/interceptor.rs:510-513` returns false on any rrid packet | fork `rtc` | M | Phase 2 |
| 6 | Dependency descriptor header extension (read + write with active-chain rewrite) | not found in either checkout | lk (`lk-rtpext`), port 1.5 kLOC Go; propose upstream later | M | Phase 3 |
| 7 | abs-capture-time extension (parse, rewrite offset) | not found | lk (`lk-rtpext`) | S | Phase 2 |
| 8 | RED (RFC 2198) audio: codec registration, encode for subscribers, decode + recovery for publishers | no `MIME_TYPE_RED`, no payloader; `is_repair_codec` filters "red" from SDP | fork `rtc` for codec registration; lk-sfu for RED receivers | M | Phase 3 |
| 9 | Fork-only pion behaviours LiveKit sets: fire on-track before first RTP, ignore rid pause on recv, disable close-by-DTLS, DTLS retransmission interval, DTLS curves, SRTP replay disable | `pkg/rtc/transport.go:357-450`; `rtc` has replay window and DTLS cipher config, lacks the first three | fork `rtc` (small flags) | S | Phase 2 |
| 10 | Embedded TURN server with TLS/TCP/UDP listeners, proxy protocol, HMAC time-limited creds, per-user allocation quota, peer CIDR allow/deny | `rtc-turn` is client + proto only (`rtc-turn/src/lib.rs:53-60`) | lk (`lk-turn`) on top of `rtc-turn` proto types; ~2-3 kLOC | L | Phase 4 |
| 11 | TURN client over TCP/TLS (for LiveKit's own outbound relay use in tests and `force_relay`) | facade skips non-UDP TURN (`turn_relayer.rs:272`) | lk-rtcio (own relayer) | M | Phase 4 |
| 12 | Interface/IP candidate filters, NAT 1:1 rewrite rules for both host and srflx, mDNS filtering of remote candidates | `setting_engine.rs:729-746` todo; `with_nat_1to1_ips` exists | fork `rtc` for filters; lk for rewrite rules | S | Phase 1 |
| 13 | Header extension id range: `VALID_EXT_IDS = 1..15` | `media_engine.rs:195` | fork `rtc` to allow two-byte ids (LiveKit negotiates 10+ extensions) | S | Phase 2 |
| 14 | SCTP max message size hard cap 256 KiB; `signal_message_size_limit` and data blobs need larger reliable messages | `setting_engine.rs:283-291` | fork `rtc` | S | Phase 5 |
| 15 | Congestion control quality: shipped GCC bench shows 42% utilisation at 5% loss and "never converged" (rtc-interceptor/benches/README.md) | LiveKit ships its own send-side BWE; do not depend on `rtc` GCC | lk-bwe (port LiveKit's) | covered in 4.5 | Phase 3 |
| 16 | Test infra: no vnet; `MockRuntime` has no loss/latency/NAT and no TCP | `runtime/mock.rs:39` | lk: sans-IO vnet (section 8.4) | M | Phase 0 |

Effort key: S under 2 engineer-weeks, M 2-6 weeks, L over 6 weeks. These are estimates from the
size of the Go code being replaced and the size of the touched Rust surface; they are not measured.

Upstreaming policy: items 4, 5, 8 (codec registration), 9, 12, 13, 14 are small and generic;
open PRs against webrtc-rs/rtc from the fork so the fork stays rebasable. Items 1, 2, 6, 10
are LiveKit-shaped and stay in the workspace.

---

## 4. Component breakdown

Strategy column: Translate (1:1 port), Redesign (same behaviour, new structure), Reuse (existing
Rust crate), Drop (not ported). Difficulty from the survey's "hardest to port" lists.

### 4.1 Protocol, auth, shared utilities

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| Protobufs (43 files) | livekit/protocol `protobufs/**` | proto: rtc 673, models 977, room 312, internal 228, rpc/* 1287 | `lk-proto` | Reuse toolchain, regenerate | S | Extend `livekit-protocol`'s `generate_proto.sh` to all files incl. `rpc/*`, `livekit_internal`, `livekit_agent*`, `livekit_metrics`. Generate twirp server traits and psrpc service traits via a custom protoc plugin in `xtask`. |
| JWT / grants | protocol/auth | ~1k | `lk-auth` | Translate | S | HS256, `video.*` grants, `GetParticipantKind`, `UpdateFromPermission`, `RoomConfig` in token, 0-perms key file check |
| Webhook notifier | protocol/webhook | ~0.8k | `lk-telemetry` | Translate | S | Queued per-URL delivery, JWT-signed `Authorization`, SHA256 body hash, processed-hook feedback |
| TimedVersion, ProtoProxy, TimeSizeCache, IncrementalDispatcher, MultitonService, guid prefixes (`PA_`, `RM_`, `ND_`, `CO_`) | protocol/utils, pkg/utils | ~1.5k | `lk-proto` utils module | Translate | S | TimedVersion must be bit-identical (it is compared across nodes) |
| Monotonic clock (`mono`) | protocol/utils/mono | small | `Clock` trait | Redesign | S | |
| Logger | protocol/logger | - | `tracing` | Reuse | S | Component names `room`, `pub`, `sub`; deferred fields via span `record` |
| Mime type enum + helpers | protocol/codecs/mime | ~0.5k | `lk-proto::mime` | Translate | S | 51 import sites in the server |
| SDP helpers (`lksdp`) | protocol/sdp | ~0.5k | `lk-room::sdp` | Translate | S | ExtractDTLSRole, fingerprints, ICE creds, stream id, bundle mid, codecs from media desc, SDP fragment |
| clientconfiguration | pkg/clientconfiguration | 335 | `lk-room::client_config` | Redesign | S | Replace tengo scripts with Rust predicates (Safari no AV1; Safari > 18.3 no VP9 publish; some Android/Firefox no H264 publish) |

### 4.2 Transport layer (`lk-rtcio`, replaces `PCTransport` + pion internals)

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| UDP/TCP mux + shard reactors | mediatransportutil `rtcconfig`, pion `UDPMux`/`TCPMux` | n/a | `lk-rtcio::mux` | New | M | Section 2.2. STUN Binding routed by ufrag; established flows by peer addr. ICE-TCP passive per RFC 6544 with 2-byte length framing. |
| PC driver | `webrtc/src/peer_connection/driver.rs` (2.1k, reference only) | - | `lk-rtcio::driver` | Redesign | M | Per-shard loop: recv batch -> `handle_read` -> drain `poll_read` into media pipeline -> drain `poll_write` into GSO batch -> timers via `poll_timeout`. No per-PC mutex; PC state is `!Send` shard-local. |
| `PCTransport` | pkg/rtc/transport.go | 3,408 | `lk-room::transport` | Redesign | L | Negotiation state machine (pending offer, restart, migration transceiver reuse, codec preferences, `MediaSectionsRequirement`), data channels (`_reliable`, `_lossy`, `_data_track`), ICE candidate filtering (mDNS drop, TCP fallback, `allow_udp_unstable_fallback`), owns pacer + BWE + allocator for subscriber PC. |
| `TransportManager` | pkg/rtc/transportmanager.go | 1,086 | `lk-room::transport_manager` | Translate | M | Publisher/subscriber PC pair, `subscriber_as_primary`, ICE config cache, fallback policy |
| Media engine config | pkg/rtc/mediaengine.go, config.go | ~440 | `lk-rtcio::media_engine` | Translate | S | Codec list from `room.enabled_codecs`, RTX PT = base + 1 with `apt`, RED, per-direction extension lists, RTCP feedback lists |
| Interceptor chain | pkg/rtc/transport.go:456-540, pkg/sfu/interceptor | 385 + wiring | `lk-rtcio::interceptors` | Redesign | M | Order: (subscriber, optional) GCC+TWCC sender; RTT-from-XR; unhandle-simulcast (migration); RTX info extractor. On `rtc` use fixed slots; the "deliver RTCP to app" tagger sits last. |
| BufferFactory replacement | pkg/sfu/buffer/factory.go, pkg/rtc/config.go:104 | 149 | `lk-rtcio::ingress_tap` | Redesign | M | LiveKit consumes raw SRTP-decrypted RTP per SSRC, bypassing pion tracks. On `rtc`, consume `RTCMessage::RtpPacket` from `poll_read` and route by SSRC to `Buffer`; pre-bind queueing for packets arriving before `on_track` maps SSRC to track. |
| ICE candidate types, mDNS detection | pkg/rtc/types/ice.go | small | `lk-room::ice` | Translate | S | |

### 4.3 SFU receive path (`lk-sfu`, `lk-media`, `lk-rtpext`)

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| `Buffer` + `BufferBase` | pkg/sfu/buffer/buffer.go, buffer_base.go | 2,137 | `lk_sfu::buffer` | Redesign | L | Replace `sync.Cond` reader with an SPSC ring drained by the forwarder task on the same shard. TWCC push, RTX demux to primary, NACK generation, RR generation, DD parse, fps calc, audio level, abs-capture-time, PLI throttle, keyframe seeder, restart detection, codec change. |
| Packet bucket | mediatransportutil/pkg/bucket | ~0.4k | `lk_media::bucket` | Translate | S | Fixed-size slot store indexed by seq; serves NACK retransmits |
| NACK queue | mediatransportutil/pkg/nack | ~0.3k | `lk_media::nack` | Translate | S | |
| TWCC responder (feedback to publishers) | mediatransportutil/pkg/twcc | ~0.5k | `lk_media::twcc` | Translate | S | Could use `rtc-interceptor::twcc::receiver` but LiveKit feeds it from the buffer path; keep the port for control over timing |
| `RTPStatsReceiver`, `RTPStatsSender`, base, lite variants | pkg/sfu/rtpstats | 4,396 | `lk_sfu::rtpstats` | Translate | L | Extended seq/ts unwrap, loss, jitter, drift, RR/SR reconciliation, snapshot windows, per-seq `snInfo` history. Thin Go tests (286 LOC): add golden tests (section 8.3). |
| Dependency descriptor parser | pkg/sfu/buffer/dependencydescriptorparser.go + rtpextension/dependencydescriptor | 364 + 1,416 | `lk_rtpext::dd`, `lk_sfu::dd_parser` | Translate | L | Bit-exact reader and writer; `MarshalWithActiveChains`. Fuzz target mandatory. |
| Frame rate calculators, frame integrity, video layer utils | pkg/sfu/buffer/{fps,frameintegrity,videolayerutils}.go | 1,508 | `lk_sfu::buffer::*` | Translate | M | videolayerutils has 892 LOC of tests to port |
| Stream trackers (packet, frame, DD) + manager | pkg/sfu/streamtracker, streamtrackermanager.go | 1,734 | `lk_sfu::stream_tracker` | Translate | M | Timer-driven; use injected clock |
| RED primary receiver (decode + recover) | pkg/sfu/redprimaryreceiver.go | 385 | `lk_sfu::red` | Translate | M | Needs RED codec negotiation from gap #8 |
| Audio level | pkg/sfu/audio | 197 | `lk_sfu::audio_level` | Translate | S | |
| `WebRTCReceiver` / `ReceiverBase` | pkg/sfu/receiver*.go | 1,612 | `lk_sfu::receiver` | Redesign | M | Per-layer buffers, forwarder loops (one task per layer on the shard), downtrack spreader, PLI throttle, RED transformer hookup, restart |
| Wrapped/Dummy receivers | pkg/rtc/wrappedreceiver.go | 660 | `lk_room::wrapped_receiver` | Translate | S | Multi-codec fan-in; placeholder before publish |
| Media loss proxy | pkg/rtc/medialossproxy.go | ~90 | `lk_room` | Translate | S | |

### 4.4 SFU send path (`lk-sfu`)

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| `DownTrack` | pkg/sfu/downtrack.go | 2,760 | `lk_sfu::downtrack` | Redesign | L | Collapse 8 mutexes + 3 worker goroutines into one per-subscriber-PC task with a command enum (nack, keyframe, max-layer notify). Owns forwarder, sequencer, rtpstats, playout delay, connection stats. Writes via shard-local sender handle, not through `write_rtp` validation (gap #4). |
| `Forwarder` | pkg/sfu/forwarder.go | 2,476 | `lk_sfu::forwarder` | Translate | L | Hardest single unit. Port with its 2,263-LOC test file first; the test is the spec. Allocation modes: optimal, provisional, cooperative, best-weighted, next-higher, pause. |
| `RTPMunger` | pkg/sfu/rtpmunger.go | 353 | `lk_sfu::rtp_munger` | Translate | M | Seq/ts rewriting with `RangeMap`; snapshot to `RTPMungerState` proto for migration |
| `RangeMap` | pkg/sfu/utils/rangemap.go | 200 | `lk_sfu::util::range_map` | Translate | S | Generic over unsigned width; Go uses `unsafe.Sizeof` for half-range wrap |
| Codec mungers (VP8, Null) | pkg/sfu/codecmunger | 590 | `lk_sfu::codec_munger` | Translate | M | Picture id 7/15-bit wrap handling; VP9/AV1/H264 are Null (no rewrite) |
| VP8 payload descriptor parser | mediatransportutil/pkg/codec | ~0.6k | `lk_media::vp8` | Translate | S | Do not use `rtc-rtp` VP8 depacketizer (different struct shape); fuzz it |
| `sequencer` (retransmit metadata ring) | pkg/sfu/sequencer.go | 490 | `lk_sfu::sequencer` | Translate | M | Byte-exact RTX rebuild: munged codec header, DD bytes, abs-capture-time bytes, trailer size |
| Video layer selectors (base, simulcast, VP9, DD, temporal VP8) + decision cache, frame chain, frame number wrapper | pkg/sfu/videolayerselector/** | 1,581 | `lk_sfu::layer_selector` | Translate | L | DD selector is the hard one; VP9 uses `rtc-rtp` VP9 depacketizer (verify field parity) |
| Playout delay controller + ext | pkg/sfu/playoutdelay.go, rtpextension/playoutdelay | 277 | `lk_sfu::playout_delay` | Translate | S | `rtc-rtp` has the extension already |
| abs-capture-time rewrite | rtpextension/abscapturetime, downtrack.go:1099-1120 | 114 | `lk_rtpext::abs_capture_time` | Translate | S | |
| Packet trailer (LKTS) | pkg/sfu/packettrailer | 48 | `lk_rtpext::trailer` | Translate | S | Protocol v17 feature |
| Pacer (pass-through, no-queue, leaky bucket) + base + probe observer | pkg/sfu/pacer | 636 | `lk_sfu::pacer` | Redesign | M | Send-time patching of abs-send-time and TWCC seq; pooled buffers replaced by `Bytes` ownership hand-off to the shard writer |
| RED encoder receiver | pkg/sfu/redreceiver.go | 252 | `lk_sfu::red` | Translate | M | |
| Blank frames / silence on mute | downtrack.go:1795-2700 | ~600 | `lk_sfu::downtrack::blank` | Translate | S | VP8, H264 2x2 keyframe blobs, Opus, PCMU silence |
| Connection quality scorer + stats | pkg/sfu/connectionquality | 1,164 | `lk_sfu::connection_quality` | Translate | M | 903-LOC subtest table to port verbatim |
| Forward stats (latency) | pkg/sfu/forwardstats.go | 306 | `lk_sfu::forward_stats` | Translate | S | Sharded atomic slots map to `AtomicU64` arrays |
| `DownTrackSpreader` | pkg/sfu/utils/downtrackspreader.go | 200 | `lk_sfu::spreader` | Redesign | M | Replace per-packet goroutine fan-out with per-shard batched queues (section 2.2) |
| Data channel writers + bitrate calc | pkg/sfu/datachannel | 327 | `lk_sfu::datachannel` | Translate | S | Reliable slow-threshold, unreliable target-latency, uses `rtc` buffered-amount thresholds |

### 4.5 Bandwidth management (`lk-bwe`)

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| `StreamAllocator` + track sorters | pkg/sfu/streamallocator | 1,840 | `lk_bwe::allocator` | Translate | L | 0 Go tests. Actor with event queue; REMB/TWCC/NACK inputs; probing; deficiency boost; stream state updates to signalling. Write proptest and trace-replay tests as it is ported. |
| `SendSideBWE` (congestion detector, packet groups, JQR/DQR) | pkg/sfu/bwe/sendsidebwe | 2,288 | `lk_bwe::send_side` | Translate | L | 0 Go tests. Bespoke, non-GCC. Capture TWCC feedback traces from the Go server for replay parity (section 8.3). |
| `RemoteBWE` (REMB) | pkg/sfu/bwe/remotebwe | 875 | `lk_bwe::remote` | Translate | M | 0 Go tests |
| Prober, probe regulator, trend detector | pkg/sfu/ccutils | 1,016 | `lk_bwe::probe`, `lk_bwe::trend` | Translate | M | 0 Go tests |
| BWE trait + null | pkg/sfu/bwe | 200 | `lk_bwe::Bwe` trait | Translate | S | |
| Optional pion GCC path (`use_send_side_bwe_interceptor`) | transport.go:458-482 | - | `rtc-interceptor::gcc` | Reuse | S | Keep config flag, but default off given the bench numbers in gap #15 |
| Dynacast (video + audio managers, quality trackers) | pkg/rtc/dynacast | 1,236 | `lk_room::dynacast` | Translate | M | Debounced; 523 LOC tests to port |

### 4.6 Room and participant (`lk-room`)

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| `Room` | pkg/rtc/room.go | 2,428 | `lk_room::room` (actor) | Redesign | L | Participant map, join/resume, hold refcount + empty/departure timers, participant update batching with `TimedVersion` + LRU dedupe, speaker/audio-level worker, connection quality worker, data packet fan-out + reliable cache (TTL 2 s, 100k entries), agent dispatch bookkeeping, simulate scenarios |
| `ParticipantImpl` | pkg/rtc/participant*.go | ~6,100 | `lk_room::participant` (actor + sub-structs) | Redesign | XL | Split into: identity/grants, state machine (JOINING/JOINED/ACTIVE/DISCONNECTED, monotonic), publisher side (pending tracks, AddTrack reconciliation, migration state), subscriber side, data (reliable seq, dedupe, data tracks, data blobs), signalling glue, metrics. 28 close reasons -> DisconnectReason mapping is a table. |
| `SubscriptionManager` | pkg/rtc/subscriptionmanager.go | 1,681 | `lk_room::subscriptions` | Translate | L | Desired-vs-actual reconciler, 3 s tick, 5 error classes with distinct timeouts, deferred subscribe behind pending unsubscribe, subscription limits |
| `UpTrackManager` + track permissions | pkg/rtc/uptrackmanager.go | 459 | `lk_room::uptracks` | Translate | M | Versioned allow-lists, revocation |
| `MediaTrack`, `MediaTrackReceiver`, `MediaTrackSubscriptions`, `SubscribedTrack` | pkg/rtc/mediatrack*.go, subscribedtrack.go | 2,820 | `lk_room::media_track` | Translate | M | Multi-codec receiver, adaptive-stream defaults, subscriber settings -> forwarder |
| Signalling (responses, handler dispatch, signaller with handshake gate) | pkg/rtc/signalling | 1,074 | `lk_room::signalling` | Translate | M | 26 response builders, 5 s handshake window with generation counter, `signallingUnimplemented` for WHIP |
| Participant signal glue | pkg/rtc/participant_signal.go | 436 | `lk_room::participant::signal` | Translate | M | Update cache (LRU 128), queued updates flush after handshake |
| SDP handling | pkg/rtc/participant_sdp.go | 408 | `lk_room::participant::sdp` | Translate | M | Codec filtering, `usedtx`, stereo, RED, publish codec regression |
| Supervisor | pkg/rtc/supervisor | 373 | `lk_room::supervisor` | Translate | S | 1 s publication watchdog |
| Data tracks, data down tracks, up data track manager | pkg/rtc/*datatrack*.go, updatatrackmanager.go | ~700 | `lk_room::data_track` | Translate | M | Dedicated unreliable DC `_data_track`, `datatrack.Packet` |
| Data blobs | pkg/rtc/participant_data_blob*.go | 213 | `lk_room::data_blob` | Translate | S | |
| Client info / capability gates | pkg/rtc/clientinfo.go, types/protocol_version.go | ~250 | `lk_room::client_info` | Translate | S | 17 protocol gates; semver compare via `semver` crate |
| Egress launcher glue | pkg/rtc/egress.go | 176 | `lk_room::egress` | Translate | S | |
| Interfaces (`types/interfaces.go`) | pkg/rtc/types | 1,729 | traits in `lk_room::types` | Redesign | M | Narrow the 150-method `LocalParticipant` interface into role traits; `mockall` on each |

### 4.7 Service layer (`lk-service`, `lk-turn`)

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| HTTP server + middleware | pkg/service/server.go | 450 | `lk_service::server` | Translate | S | CORS (permissive), double-slash removal, body limit, API key auth, request id, health `/` with 406 when node stats stale > 4 s, debug endpoints, separate prometheus listener with basic auth |
| `RTCService` (WS signal) | pkg/service/rtcservice.go, wsprotocol.go | 1,046 | `lk_service::rtc_ws` | Translate | M | `/rtc` (query params), `/rtc/v1` (`join_request` base64 + gzip), validate endpoints, protobuf vs protojson auto-detect, decompressed size limit, 10 s WS ping, inline Ping/PingReq answers, first-response-before-upgrade rule with `3+attempt` s timeout |
| `RoomManager` | pkg/service/roommanager.go, roommanager_service.go | 1,621 | `lk_service::room_manager` | Redesign | L | `StartSession` (350 lines: create-or-join, resume, duplicate identity eviction, ~60 participant params, TURN creds, psrpc topic registration + rollback, v1 AddTrack/PublisherOffer replay), token refresh loop (5 min), psrpc Room/Participant/AgentDispatch/WHIP servers |
| `RoomAllocator` | pkg/service/roomallocator.go | 248 | `lk_service::allocator` | Translate | S | Room lock, presets, node selection |
| Twirp services: RoomService (14), AgentDispatch (3), Egress (9), Ingress (6), SIP (18) | pkg/service/{roomservice,agent_dispatch_service,egress,ingress,sip}.go | 2,173 | `lk_service::api::*` | Translate | M | Permission gates (`Ensure*Permission`), limits, delegate over bus; deterministic egress ids; twirp hooks for logging/status metrics |
| Auth middleware | pkg/service/auth.go, basic_auth.go | 291 | `lk_service::auth_mw` | Translate | S | Bearer header or `access_token` query |
| Stores (local, redis, redis SIP) | pkg/service/{localstore,redisstore,redisstore_sip}.go | 1,647 | `lk_service::store` | Translate | M | Key names in section 9.2 must match exactly for mixed clusters |
| `SignalServer` (psrpc RelaySignal stream server) | pkg/service/signal.go | 222 | `lk_service::signal_relay` | Translate | M | Hijacks stream, drains on teardown without losing final Leave |
| `IOInfoService` + clients | pkg/service/ioservice*.go, clients.go | 498 | `lk_service::io_info` | Translate | S | |
| Agent service + handler | pkg/service/agentservice.go, pkg/agent | 1,542 | `lk_service::agents`, `lk_room::agent_client` | Translate | M | `/agent` WS, worker protocol v1 (Register, Availability, UpdateJob, SimulateJob, Ping, UpdateWorker, MigrateJob), per-key job topics, load-aware assignment (`target_load` 0.7), 1 min enabled cache |
| WHIP service (RFC 9725) | pkg/service/whipservice.go | 543 | `lk_service::whip` | Translate | M | One-shot signalling mode, per-participant WHIP RPC topics |
| TURN server + quota | pkg/service/turn.go, turnquota.go | 548 + server impl | `lk-turn` | New | L | Gap #10 |
| Twirp hooks, request id, egress id, errors | pkg/service/{twirp,requestid,egressid,errors}.go | 591 | `lk_service::twirp_hooks` | Translate | S | |
| DI (wire) | pkg/service/wire*.go | 666 | constructor functions in `lk-server` | Drop | S | |

### 4.8 Routing and message bus (`lk-bus`, `lk-routing`)

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| psrpc: local bus, Redis bus, request/response, multi-response, streams, pub/sub, keepalive, metadata, OTel middleware | livekit/psrpc (external, ~8 kLOC Go) | - | `lk-bus` | Redesign for wire compat | L | Must match Redis channel names, message envelopes (`internal.Request`, `internal.Response`, `internal.Stream`), request ids, claim/affinity flow, timeouts and retry semantics so Go and Rust nodes interoperate. Verify by running Go client against Rust server and vice versa (section 8.6). |
| Generated psrpc stubs (`protocol/rpc`) | protocol/rpc/*.psrpc.go | - | `lk-proto` via custom protoc plugin | New | M | Topic formatting (`FormatRoomTopic`, `FormatParticipantTopic`), typed clients/servers |
| Local router, Redis router | pkg/routing/{localrouter,redisrouter}.go | 425 | `lk_routing::router` | Translate | M | `nodes` hash, `room_node_map`, dead node removal (5 s), stats worker, keepalive pub/sub, drain -> `SHUTTING_DOWN` |
| Signal relay sink/source, message channel | pkg/routing/{signal,messagechannel,interfaces}.go | 813 | `lk_routing::relay` | Translate | L | Seq numbers, batching, exponential backoff between `min_retry_interval` and `max_retry_interval` until `retry_timeout`, gap detection (`ErrSignalMessageDropped`), prefix skip on replay, `BlockOnClose` |
| Node, node stats ring | pkg/routing/{node,nodestats}.go | 249 | `lk_routing::node` | Translate | S | |
| Selectors (any, cpuload, sysload, regionaware; sort_by; lowest/twochoice) | pkg/routing/selector | 546 | `lk_routing::selector` | Translate | S | 728 LOC tests to port |

### 4.9 Config, CLI, telemetry

| Component | Go source | LOC | Rust target | Strategy | Difficulty | Notes |
|---|---|---|---|---|---|---|
| Config struct + defaults + validation + auto CLI flags | pkg/config/config.go | 1,113 | `lk-config` | Translate | M | Every YAML field must keep its name (config-sample.yaml is the contract). Generate one clap flag per field with a derive macro. Strict mode (`deny_unknown_fields`) with `--disable-strict-config` escape. |
| CLI + subcommands | cmd/server | 605 | `lk-server` | Translate | S | `generate-keys`, `ports`, `create-join-token`, `list-nodes`, `help-verbose`; env vars `LIVEKIT_CONFIG`, `LIVEKIT_KEYS`, `NODE_IP`, `UDP_PORT`, `REDIS_HOST`, ...; two-stage SIGTERM |
| Telemetry service, events, stats worker | pkg/telemetry | 1,989 | `lk_telemetry` | Translate | M | 12 webhook events, `AnalyticsStat` emission, per-participant stats aggregation |
| Prometheus families | pkg/telemetry/prometheus | 1,299 | `lk_telemetry::prom` | Translate | M | 0 Go tests; parity checked by scraping both servers (section 8.7) |
| Client metrics (`MetricsBatch`, timestamper, RTT series) | pkg/metric | 513 | `lk_telemetry::client_metrics` | Translate | S | |
| Node stats (CPU, memory, load, packet counters via go-tc on Linux) | prometheus/node.go + hwstats | ~400 | `lk_telemetry::node_stats` | Translate | S | `sysinfo` crate; Linux qdisc drop counters via netlink (`neli`) or `/proc` fallback |
| OTel (psrpc middleware, Jaeger) | ioservice.go, main.go | small | `lk_bus::otel` | Reuse | S | |

---

## 5. Feature matrix for the port

"v1" = required before any production traffic. "v2" = after v1 parity. Status column is what
the Go server does today (from the survey), so scope is not accidentally widened.

| Feature | Go today | Port target | Depends on |
|---|---|---|---|
| Opus, VP8, VP9, H264, H265, AV1 negotiation and forwarding | Yes | v1 | media engine |
| Simulcast (RID) with layer switching | Yes | v1 | gap #5 rrid, forwarder |
| VP9 k-SVC layer selection | Yes | v1 | VP9 depacketizer parity |
| AV1 / VP9 SVC via dependency descriptor incl. DD rewrite | Yes | v1 | gap #6 |
| VP8 temporal layer selection + picture id munging | Yes | v1 | codec munger |
| RTX send (subscriber NACK) and receive (publisher) | Yes | v1 | sequencer, buffer |
| RED audio encode/decode | Yes | v1 | gap #8 |
| TWCC feedback generation to publishers | Yes | v1 | lk-media twcc |
| Send-side BWE (LiveKit algorithm) + probing + pacer | Yes | v1 | lk-bwe |
| REMB receive-side BWE | Yes | v1 | lk-bwe |
| Stream allocator, adaptive stream, dynacast (video + audio codec) | Yes | v1 | lk-bwe, lk-room |
| PLI/FIR, keyframe seeding, PLI throttle | Yes | v1 | |
| Audio level, active speakers | Yes | v1 | |
| abs-capture-time rebasing, playout delay, packet trailer | Yes | v1 | gap #7 |
| Connection quality scoring | Yes | v1 | |
| Data channels reliable/lossy, DataPacket kinds, reliable replay on resume, dedupe | Yes | v1 | rtc SCTP |
| Data tracks (`_data_track`), data blobs | Yes (config-gated) | v1 | |
| E2EE passthrough (opaque payload) | Passthrough only | v1 | none |
| Muted track blank frames / silence | Yes | v1 | |
| Signal protocol v17 with all 17 gates, `/rtc` and `/rtc/v1`, protobuf + JSON frames | Yes | v1 | lk-proto |
| Resume (signal reconnect), full reconnect, migration (`MigrateState`), room move | Yes | v1 | signalling gate, forwarder state snapshot |
| Permissions at runtime (`SetPermission`), track subscription permissions | Yes | v1 | |
| Room lifecycle (empty/departure timeouts, hold, presets, max participants) | Yes | v1 | |
| Twirp RoomService, AgentDispatch, Egress, Ingress, SIP APIs | Yes | v1 (Room, AgentDispatch), v1 (Egress, Ingress, SIP as bus pass-through) | lk-bus |
| Webhooks (12 events, signed) | Yes | v1 | |
| Agents (`/agent` worker protocol v1, job dispatch, load balancing) | Yes | v1 | |
| WHIP (`/whip/v1`, one-shot signalling) | Yes | v2 | |
| Embedded TURN (UDP, TLS, proxy protocol, quotas, CIDR policy) | Yes | v1 for UDP+TLS; proxy protocol v2 | gap #10 |
| ICE-TCP passive, TCP fallback, UDP-unstable fallback | Yes | v1 | gap #2 |
| ICE lite (off by default because of Firefox), NAT 1:1, IP filters | Yes | v1 | gap #12 |
| Multi-node over Redis (router, stores, psrpc relay), node selectors | Yes | v1 | lk-bus |
| Prometheus metrics parity, health endpoint, debug endpoints | Yes | v1 | |
| OTel tracing via Jaeger URL | Yes | v2 | |
| Client metrics (`MetricsBatch`) | Yes | v2 | |
| Pion GCC interceptor alternative BWE | Yes (config) | v2 | rtc-interceptor gcc |
| FlexFEC / ULPFEC | No | not in scope | |
| Opus DTX detection in media path | No (SDP only) | not in scope | |
| VP9/AV1/H264 codec header munging | No (Null) | not in scope | |
| `ForwardParticipant`, `MoveParticipant` | Stubs ("not implemented") | stubs | |
| `cmd/test-server` | Go mock for SDK CI | keep Go binary, do not port | |

---

## 6. Ownership and concurrency design notes

| Go pattern | Count / place | Rust design |
|---|---|---|
| `sync.Pool` of 1460 B payloads and `rtp.Header` with preserved extension capacity, handed from `WriteRTP` through pacer to `SendPacket` | sfu.go, pacer/base.go | Publisher payload as `Bytes` (refcounted, shared by all subscribers); per-subscriber header built into a small stack struct; the shard writer serialises header + `Bytes` payload directly into the GSO batch buffer. No pool needed; measure and add a slab only if allocation shows in profiles. |
| `BufferBase` `sync.Cond` handoff from SRTP goroutine to forwarder goroutine | buffer_base.go:151 | Same-shard SPSC ring; forwarder task is woken by the driver after each recv batch. No condvar. |
| `DownTrackSpreader.BroadcastRTP` spawns up to NumCPU goroutines per packet above a threshold | downtrackspreader.go | Group subscribers by owning shard; enqueue one `(header_template, Bytes)` per shard; each shard fans out locally. Cross-shard hop cost is one MPSC push per shard, not per subscriber. |
| `TypedOpsQueue` actors (StreamAllocator, PCTransport) | streamallocator.go, transport.go | `mpsc` command channel + single task; identical semantics |
| `DownTrack` 8 mutexes + 3 worker goroutines + channels capped at 5000 pending NACKs | downtrack.go | One task per subscriber PC handling all downtracks of that PC; NACK, keyframe, notifier are enum commands with bounded queues |
| `ParticipantImpl` ~25 atomics, callbacks, listener locks | participant.go | Actor with typed messages; listeners become `mpsc::Sender<Event>`; no re-entrancy because callbacks never run inside the actor's lock |
| `Room` 1 s server-wide sweep for `CloseIfEmpty` | server.go, room.go | Per-room timers armed on last-leave/creation instead of polling |
| go-deadlock swap in CI | buildtest.yaml | `parking_lot` `deadlock_detection` feature in CI; most locks disappear under the actor model |
| `time.AfterFunc`, tickers everywhere | many | All through the `Clock` trait so tests can advance a virtual clock |
| `atomic.Value` for codec/bind state | downtrack.go | `arc_swap::ArcSwap` |
| `zapcore.ObjectMarshaler` on ~30 hot structs | sfu | `tracing::Value` impls or `Debug` behind a `debug_enabled` guard; never format on the packet path |

---

## 7. Phased roadmap

Each phase has an exit gate. Effort figures are engineer-months (EM) and are rough size-based
estimates, not measurements; treat them as relative weights.

| Phase | Scope | Exit gate | Effort (EM) |
|---|---|---|---|
| 0. Spike and infra (4-6 weeks) | Workspace skeleton; `lk-proto` full codegen incl. `rpc/*`; `lk-rtcio` UDP mux + one shard driving `rtc`; sans-IO vnet harness (8.4); CI (fmt, clippy -D warnings, deny, nextest, coverage); fork branches of `rtc` with gaps #4, #9, #13 | A Rust process on one UDP port completes ICE+DTLS+SRTP with 500 concurrent `rtc` peers in vnet and with Chrome; deterministic vnet test runs under 1 s | 3 |
| 1. Transport and signalling (single node, audio only) | `lk-config`, `lk-auth`, `lk-service` HTTP/WS, `RoomManager.StartSession` (new/resume), `Room` + `Participant` actors (join, leave, metadata, permissions), `PCTransport` negotiation, data channels, local bus + local store, TCP mux, RTCP tagging interceptor | Existing JS and Go client SDKs join, publish and subscribe Opus through the Rust server; `test/singlenode_test.go` scenarios pass against it via the Go test client (8.6); protocol gates for v17 verified | 6 |
| 2. Video forwarding core | `Buffer`, `RTPStats`, `Forwarder`, `RTPMunger`, `DownTrack`, `sequencer` + RTX, VP8 munger, simulcast selector, stream trackers, PLI/keyframe, gap #5 rrid, gap #7 abs-capture-time, blank frames, muted handling, playout delay | Simulcast VP8/H264 publish with layer switching by subscriber settings; RTX repairs verified on-wire in vnet; byte-level differential test vs Go forwarder passes (8.3) | 8 |
| 3. Bandwidth management and SVC | `lk-bwe` (allocator, send-side BWE, REMB, prober), pacer variants, TWCC responder, dynacast, DD reader/writer + DD selector, VP9 k-SVC, RED (gap #8), connection quality, audio level | Trace-replay parity with Go BWE within tolerance (8.3); AV1 SVC and VP9 SVC layer selection interop with Chrome; adaptive stream and dynacast integration tests pass | 8 |
| 4. Distributed mode and TURN | `lk-bus` Redis bus wire-compatible with psrpc; Redis router, stores, signal relay with seq/backoff; node selectors; `lk-turn` (UDP, TLS); agents; webhooks; telemetry + Prometheus parity; egress/ingress/SIP pass-through services | Mixed cluster: Go signal node fronting a Rust media node and the reverse; `test/multinode*_test.go` scenarios pass; TURN relay integration test (allow/deny CIDR matrix) passes | 7 |
| 5. Parity and hardening | Migration/resume edge cases, room move, data tracks, data blobs, WHIP, client metrics, SCTP cap (gap #14), fuzzing corpus, soak tests, load tests with livekit-cli, perf gates | Full Go integration suite green against Rust on all three signalling paths; 72 h soak with no leak; p99 forwarding latency and CPU per subscriber at or below Go on the same load profile | 6 |
| 6. Cutover | Canary Rust media nodes in a Go cluster; shadow metrics comparison; region by region | Rust nodes carry 100 % of media in one region for 30 days with parity on quality and reconnect metrics | 3 |

Total: roughly 41 EM of core work, before review, upstreaming and unplanned rework. With 4-5
engineers this is 12-15 calendar months. The uncertainty is dominated by phases 2 and 3 (the
untested BWE and allocator code) and by how much of `rtc` needs forking beyond section 3.

Ordering rationale: audio-only signalling first proves the actor model and wire compatibility
with zero media risk; video core before BWE because the forwarder tests are the best spec in
the repo; distributed mode after media because the mixed-cluster cutover requires a media node
that already works.

---

## 8. Testing strategy

### 8.1 Principles

- Every Go test file with real assertions is ported, not rewritten, and lands in the same
  commit as the component it covers. The 2,263-LOC forwarder test and the 903-LOC connection
  quality table are the spec for those units.
- Anything the Go server tests only indirectly (BWE, allocator, ccutils, rtpstats, prometheus:
  about 10 kLOC with zero or near-zero tests) gets new tests in the port; the port is the one
  chance to close those holes.
- Time is injected everywhere below the actor layer, so unit and simulation tests never sleep.
- Unit tests run with no Redis, no network, no Docker (`cargo nextest run`). Integration tests
  are a separate target gated behind `--features integration` and skip (not fail) when Redis is
  absent, unlike the Go suite which hard-codes `localhost:6379`.

### 8.2 Test layers and tooling

| Layer | Go today | Rust port | Tooling |
|---|---|---|---|
| Unit | 306 Test funcs in `pkg/`, testify `require` in 86 files | `#[test]` + `pretty_assertions`; table tests with `rstest` | `cargo nextest`, `cargo llvm-cov` (coverage did not exist in Go CI) |
| Mocks | counterfeiter, 38 kLOC generated, 35 interfaces | `mockall` `#[automock]` on role traits; expectations replace `CallCount`/`ArgsForCall` | regenerated in-tree, no checked-in generated code |
| Property | none | `proptest` on: RTPMunger seq/ts continuity, RangeMap, VP8 picture id wrap, DD writer round trip, RTPStats unwrap, RED encode/decode, allocator invariants (never exceeds estimate, monotone under more bandwidth), send-side BWE (estimate bounded, decreasing under sustained delay growth) | `proptest` |
| Fuzz | none | `cargo-fuzz` targets: DD extension reader, DD writer + reader round trip, VP8 descriptor, RED decode, abs-capture-time, playout delay, signal frame decode (protobuf and JSON), SDP munging, `join_request` decompress, TWCC feedback parse, WS frame handling; corpus checked in; 10 min per target in nightly CI | libFuzzer, OSS-Fuzz later |
| Differential vs Go (section 8.3) | none | Byte-exact and trace-based comparisons against the Go implementation for forwarder, mungers, rtpstats, BWE | Go harness binary emits JSONL fixtures |
| Deterministic simulation (section 8.4) | pion vnet in 2 suites; `MockRuntime` in webrtc-rs has no loss/latency | Sans-IO vnet with loss, delay, jitter, reorder, bandwidth cap, NAT; virtual clock; seeds recorded on failure | custom, in `lk-rtcio::vnet` |
| Integration single node | `test/singlenode_test.go` (25 funcs x 3 signalling paths), in-process server + real pion client | Same scenarios with `lk-testclient`; plus the Go test client against the Rust server (8.6) | `tokio::test`, in-process server |
| Integration multi node | `test/multinode*_test.go` (18 funcs), Redis required | Same, Redis via `testcontainers` with localhost fallback; plus mixed Go/Rust cluster jobs | |
| Browser interop | none in repo | Playwright + Chromium (pre-installed in CI images): publish simulcast VP8/VP9/AV1/H264 + Opus RED, verify `getStats` on subscriber; Firefox and Safari in a nightly matrix | Playwright |
| SDK conformance | SDK repos' CI hit `cmd/test-server`; none hit the real server | Nightly job running the JS, Go, Rust, Python, Swift, Android SDK example test suites against a Rust server | |
| Load and soak | none in repo (livekit-cli load-test is external) | `lk load-test` (livekit-cli) profiles: 1 pub x 500 sub, 50 pub x 50 sub simulcast, 200 audio-only rooms; 72 h soak with reconnect churn; assert no RSS growth, no fd growth | livekit-cli, Prometheus scrape |
| Benchmarks | 6 Benchmark funcs, never run in CI | criterion: forward path per packet, buffer write, rtpstats update, DD parse, RED encode, VP8 munge, RangeMap, TWCC feedback build; closed-loop BWE simulation printing convergence and utilisation (as `rtc-interceptor` does); CI gate at +10 % regression | criterion, `cargo bench` on a pinned runner |
| Sanitizers and model checking | `go test -race` whole tree; go-deadlock swap | ThreadSanitizer nightly on integration tests; `loom` on the SPSC ring, shard queues and `ForwardStats` slots; Miri on pure parsers (DD, VP8, RED) | nightly toolchain job |
| Static | golangci-lint (staticcheck + one depguard rule) | `clippy -D warnings`, `cargo deny` (licenses, bans: e.g. forbid `prost`'s JSON in favour of `pbjson`), `cargo audit`, `cargo semver-checks` on forked `rtc` | |

### 8.3 Differential testing against the Go implementation

The Go server stays the oracle until cutover. Add a small Go harness (in this repo, under
`test/oracle/`) that drives specific Go units and writes JSONL fixtures the Rust tests replay:

| Unit | Input fixture | Compared output | Tolerance |
|---|---|---|---|
| `Forwarder` + `RTPMunger` + VP8 munger | Sequence of `ExtPacket` (seq, ts, marker, layer, VP8 descriptor) with subscriber setting changes | Output seq, ts, marker, picture id, TL0PICIDX, drop decisions, layer switch points | exact |
| `sequencer` + RTX rebuild | Forwarded stream + NACK list | RTX packet bytes | exact |
| DD reader/writer | Real DD extension bytes captured from Chrome AV1/VP9 SVC publishes | Parsed structure; rewritten bytes for given active chains | exact |
| `RTPStatsReceiver`/`Sender` | Packet arrival trace + RR/SR | Loss, jitter, drift, RR fields, snapshot deltas | exact for integers, 1e-6 for floats |
| Send-side BWE + allocator | TWCC feedback trace + probe events from a real Go session | Estimate over time, allocation decisions | estimate within 5 % at each feedback; identical allocation decisions after 2 s warm-up |
| Connection quality scorer | Delta stats table | MOS-like score, quality enum | exact |
| Signalling | Recorded `SignalRequest` stream | `SignalResponse` stream (order and content, ignoring timestamps and ids) | exact |

Fixtures are versioned with the Go commit that produced them. When Go behaviour changes upstream,
regenerate and diff.

### 8.4 Deterministic simulation (vnet replacement)

- `lk-rtcio::vnet`: an in-memory network of `rtc` peers driven by a virtual clock. Links have
  configurable one-way delay, jitter, loss (random and burst), reorder, bandwidth cap with a
  queue (so BWE sees queueing delay), and NAT types (full cone, symmetric).
- Every test is a seeded `proptest`-style run; failing seeds are printed and re-runnable.
- Used for: RTX repair correctness, TWCC feedback timing, BWE convergence and reaction to a
  step change, allocator behaviour under 20 % loss, migration mid-stream, ICE-TCP fallback,
  TURN relay selection, reconnect with dropped signal messages (relay seq gap path).
- Control-plane actors (rooms, participants, bus) are tested under `turmoil` for partition and
  restart scenarios: node death during join, Redis unavailable for 10 s, signal node loss.

### 8.5 Rust integration test client (`lk-testclient`)

Mirror the two design choices that make the Go client useful:
- Signal request/response interceptor hooks for fault injection (drop, reorder, rewrite).
- Synthetic media: 5-byte samples every 20 ms and a minimal H264 SPS/PPS/IDR, no media files.
Add: simulcast publish with three RIDs, DD-carrying AV1 stream from a captured corpus, RED audio,
`getStats` reading, NACK injection, forced relay, ICE-TCP-only mode.

### 8.6 Cross-implementation interop matrix (CI jobs)

| Client | Server | Purpose |
|---|---|---|
| Go `test/client` (existing) | Rust server | Proves protocol v17 wire compatibility; run the whole `test/` suite with a `LIVEKIT_SERVER_BIN=rust` switch |
| `lk-testclient` | Go server | Proves the Rust client is a faithful test tool before it is trusted |
| Go signal node | Rust media node (Redis) | psrpc relay compatibility |
| Rust signal node | Go media node (Redis) | same, reverse |
| Go RoomService client (`livekit-server-sdk-go`) and Rust `livekit-api` | Rust server | twirp and JWT compatibility |
| Chrome, Firefox, Safari | Rust server | codec, simulcast, SVC, RED, ICE-TCP, TURN |

### 8.7 Observability parity checks

- Scrape `/metrics` from Go and Rust under the same synthetic load and diff the family and label
  sets; fail on any missing family (the Grafana dashboard in `deploy/grafana` is the consumer).
- Replay a recorded session and diff webhook payloads (ignoring ids and timestamps).
- Log schema test: every `tracing` event on the join path carries `room`, `roomID`,
  `participant`, `pID` once known.

### 8.8 CI layout

| Job | Trigger | Content | Budget |
|---|---|---|---|
| check | PR | fmt, clippy, deny, audit, doc build | < 5 min |
| unit | PR | `nextest` with coverage upload; MSRV and stable | < 10 min |
| sim | PR | vnet and turmoil suites, 3 seeds each | < 10 min |
| integration | PR | single node (3 signalling paths), multi node with Redis service | < 20 min |
| interop | PR (labelled) and nightly | Go client vs Rust server, mixed cluster, Playwright Chromium | < 40 min |
| fuzz | nightly | 10 min per target, corpus minimisation | |
| perf | nightly, pinned runner | criterion + closed-loop BWE sim; regression gate | |
| sanitizers | nightly | TSan integration, Miri parsers, loom | |
| soak | weekly | 72 h load with churn; RSS/fd/latency assertions | |
| release | tag | static musl `x86_64` and `aarch64` binaries, multi-arch Docker (matches `.goreleaser.yaml` and `docker.yaml`) | |

---

## 9. Compatibility contracts

### 9.1 Client-facing

- Signal protocol version 17 and all gates in `pkg/rtc/types/protocol_version.go`; `ServerInfo`
  must report the same edition/version fields the SDKs read.
- `/rtc` query parameters (17 of them) and `/rtc/v1` `join_request` encoding (base64url, optional
  gzip, size bound `http.DefaultMaxHeaderBytes` = 1 MiB).
- JSON signalling frames must use protojson semantics with unknown fields discarded.
- Ping/Pong constants: WS ping 10 s, participant `PingIntervalSeconds` 5, `PingTimeoutSeconds` 15.
- Data channel labels `_reliable`, `_lossy`, `_data_track`; RTX PT = base PT + 1.
- SDP shape differences are the largest hidden risk: the SDKs tolerate pion's SDP. Capture
  Go-produced offers/answers for each codec set as fixtures and diff the Rust SDP semantically
  (attributes per m-section, ordering of codecs, extmap ids, `a=ssrc-group`, `a=simulcast`).

### 9.2 Cluster-facing (mixed Go/Rust clusters)

- Redis keys: `nodes`, `room_node_map`, `rooms`, `room_internal`, `room_participants:<room>`,
  `room_lock:<room>`, `egress`, `ended_egress`, `egress:room:<room>`, `ingress*`,
  `agent_dispatch:<room>`, `agent_job:<room>`, `livekit_version`.
- psrpc channel naming, envelope protobufs, request id format, claim/affinity protocol,
  `RelaySignal` stream framing with `Seq` and `Close`, keepalive ping topic.
- `livekit.Node` and `NodeStats` protos published by Rust nodes must be complete enough for Go
  selectors (`cpuload`, `sysload`, `regionaware`) to pick them.
- `StartSession` grants are carried as JSON in `GrantsJson`, not protobuf; keep the JSON shape.

### 9.3 Operator-facing

- YAML config field names and defaults are frozen; `config-sample.yaml` is copied into the Rust
  repo and parsed in a test with `deny_unknown_fields`.
- CLI flag names, env vars, subcommands, exit codes, two-stage shutdown.
- Prometheus metric families and labels (section 4.9), health endpoint semantics (406 when stale).
- Docker image entrypoint `/livekit-server`, ports 7880/7881/7882 and TURN 3478/5349.
- `cmd/test-server` stays a Go binary and keeps its Docker image; SDK CIs depend on it and it
  does not exercise the server.

---

## 10. Risks and mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `rtc` is pre-1.0 (0.21.0-rc.2, no MSRV, single principal author, weekly pre-release bumps, history rewritten at 0.20) and re-exports 15 subcrates as public API | High | High | Pin a fork commit per phase; `cargo semver-checks` on rebase; keep LiveKit-specific changes behind feature flags so upstream PRs are small; budget one engineer-week per month for rebases |
| Untested Go code (send-side BWE 2.3 kLOC, allocator 1.8 kLOC, ccutils 1 kLOC, remote BWE 0.9 kLOC) has behaviour only its authors know | High | High | Trace-replay differential tests (8.3) captured from production-like Go sessions before porting; involve the original authors in reviewing the traces |
| Byte-exact RTX/DD/VP8 rewriting bugs are silent (decoders drop frames, no error) | Medium | High | Differential fixtures + fuzz round-trips + browser `getStats` assertions on `framesDecoded` and `pliCount` in interop CI |
| SDP compatibility with mobile SDKs (transceiver reuse, mid handling on migration) | Medium | High | SDP fixture diffing (9.1); nightly SDK conformance matrix (8.2); keep protocol gate behaviour identical |
| Shard/mux design has different failure modes than pion's per-PC goroutines (head-of-line blocking on a shard) | Medium | Medium | Shard count configurable; per-shard queue depth metrics; soak with skewed rooms (one 1000-subscriber room) |
| psrpc wire compatibility drift (psrpc is versioned with the Go server) | Medium | High | Vendor the psrpc envelope protos; mixed-cluster CI job on every PR that touches `lk-bus` |
| TURN server from scratch (security-sensitive) | Medium | High | Reuse `rtc-turn` message parsing; fuzz the allocation path; restrict listeners in v1 to UDP + TLS; external review before exposing |
| Performance not better than Go on the forwarding path (Go already avoids allocations via pools) | Medium | Medium | Criterion gates from phase 2; profile with `perf` on the shard loop; GSO batching is the expected win |
| Scope creep into features Go does not have (FEC, codec munging for VP9/AV1) | Medium | Low | Section 5 fixes v1 scope to Go parity |
| `livekit/protocol` changes during the port (it moves weekly) | High | Medium | Proto sync in `xtask` pinned to the Go server's `go.mod` version; CI fails if the pin and the Go repo diverge |

---

## 11. Open decisions

| Decision | Options | Recommendation |
|---|---|---|
| Fork strategy for `rtc` | (a) long-lived fork with rebases, (b) vendor and diverge, (c) upstream everything first | (a), with the upstreaming policy in section 3; revisit after phase 2 based on upstream responsiveness |
| Reactor model | (a) tokio current-thread per shard, (b) hand-rolled epoll/io_uring loop | (a) for phases 0-3; benchmark (b) with `io_uring` via `tokio-uring` or `glommio` only if the perf gate fails |
| Redis client | `fred` vs `redis` | `fred` (sentinel + cluster + pipelining without extra crates); confirm licence compatibility with `cargo deny` |
| HTTP stack | `axum` vs raw `hyper` | `axum`; twirp crate integrates with hyper services |
| Signal node and media node in one binary (as Go) or split | same binary | Same binary; roles by config, as Go |
| Keep Go BWE algorithm or adopt `rtc` GCC | port Go | Port Go; bench data in gap #15 rules out GCC as default |
| Where the DD implementation lives | LiveKit crate vs upstream `rtc-rtp` | LiveKit crate first (needs the writer with active-chain rewrite); offer reader upstream later |

---

## 12. Caveats and limitations of this plan

- The livekit "warp" pion forks (`livekit/webrtc-pion`, `livekit/dtls`, `livekit/ice`) could not
  be diffed against upstream here (module cache empty, repos out of session scope), so the list of
  fork-only behaviours in gap #9 comes from call sites gated on `EnableWarp` and SettingEngine
  calls, not from the fork source. `EnableSped` and `EnableSctpSnap` semantics are unknown and
  must be read from the fork before phase 1.
- `livekit/psrpc` source was not available; the wire-compat requirements in 4.8 and 9.2 are
  derived from how the server uses it. The psrpc repo must be read before designing `lk-bus`.
- Effort numbers are size-based estimates from the surveyed LOC and the touched `rtc` surface.
  They are not calibrated against a prior port.
- The `rtc` checkout is shallow (1 commit at 4239564, 2026-09-14). Claims about what is missing
  were verified by grep in that snapshot; upstream may add items (rrid, UDP mux) before the port
  reaches them, so re-run the gap check at the start of each phase.
- `livekit-protocol` 0.7.13 was inspected from the crates.io tarball; its `generate_proto.sh`
  covers 10 of 43 protos as of that version.
- Browser and SDK interop numbers (which SDK versions, which browsers) are not fixed here; set
  them from the SDK support matrix the team maintains.
