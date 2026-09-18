# The `webrtc-rs/rtc` fork: workflow, gaps and rebase runbook

Part of issue #981 (Phase 0, epic #972).

`livekit-rs` is built on the sans-IO `rtc` core rather than the `webrtc` async
facade. `rtc` is pre-1.0, has a single principal author and publishes weekly
pre-releases, so the port carries a fork and a standing rebase budget rather
than pretending upstream is stable. This document is how that fork is run.

---

## 1. Status

**No fork branch exists yet.** The workspace currently depends on the published
`rtc` 0.21.0-rc.2 from crates.io, and the three `SettingEngine` flags this issue
calls for have not been landed.

This is a scope block, not an oversight: creating and pushing a fork of
`webrtc-rs/rtc` needs a repository outside the two this work has access to
(`harana/harana-matrix` and `threeseed/livekit`). Everything that does not need
that repository is done here: the dependency layout the fork will slot into, the
`cargo deny` source allow-list it needs, the rebase procedure, and the gap list
including one gap found while building Phase 0 that was not in the original
table.

**To unblock:** fork `webrtc-rs/rtc` to an organisation account, grant this
workspace push access, and section 3 becomes executable.

---

## 2. How the fork is consumed

The workspace depends on `rtc` by version, not by git, and redirects it through
a `[patch]` entry. That way exactly one line changes between "upstream" and
"forked", every crate keeps its ordinary version requirement, and dropping the
fork later is a deletion rather than a refactor.

Add to `livekit-rs/Cargo.toml` once the fork exists:

```toml
[patch.crates-io]
rtc = { git = "https://github.com/<org>/rtc", rev = "<pinned sha>" }
rtc-shared = { git = "https://github.com/<org>/rtc", rev = "<pinned sha>" }
rtc-ice = { git = "https://github.com/<org>/rtc", rev = "<pinned sha>" }
rtc-dtls = { git = "https://github.com/<org>/rtc", rev = "<pinned sha>" }
# ...one line per `rtc-*` crate the workspace resolves.
```

The revision is pinned, never a branch name: a floating branch means a
dependency that changes under a build that nobody asked to change.

`deny.toml`'s `[sources] allow-git` list is empty today and must gain that
repository at the same time, or `cargo deny` fails the `check` job. That
coupling is intentional: a fork that CI does not know about is a supply-chain
hole.

### Branch layout

```
upstream/main ──────────────────────────────────●  (webrtc-rs/rtc)
                                                 \
livekit/main  ────────────────────────────────────●──●──●──●
                                                     │  │  │
                                    gap #4 permissive sender writes
                                        gap #9 pion behaviour flags
                                            gap #13 two-byte extension ids
```

One commit per gap, never squashed, each with a test and a commit message
naming the gap number. A stack of small, individually revertable commits is
what makes the rebase in section 4 mechanical; a single "LiveKit changes"
commit would make every upstream conflict a merge of unrelated work.

---

## 3. The gaps this issue lands

Verified against the `rtc` 0.21.0-rc.2 sources.

### Gap #4 — relax `RTCRtpSender::write_rtp` validation

`rtp_sender/mod.rs` rejects SSRCs, payload types and extension ids that were not
pre-negotiated on that sender. An SFU forwarder owns those header fields itself:
it rewrites sequence numbers, timestamps and SSRCs as it forwards, and there is
no negotiated sender to derive them from.

Add a `SettingEngine` flag `permissive_sender_writes`, defaulting to off, or a
raw-write entry point that bypasses the checks. Off by default matters: the
validation is correct for every non-SFU caller.

Consumed in Phase 2, by the forwarder and the downtrack.

### Gap #9 — fork-only pion behaviours LiveKit relies on

Go sets these through `livekit/webrtc-pion` in `pkg/rtc/transport.go`:

| Behaviour | Why LiveKit needs it |
|---|---|
| Fire `on_track` before the first RTP packet | The room has to publish the track to subscribers before media flows, not after |
| Ignore RID pause on receive | A paused simulcast layer must not tear the receiver down |
| Disable close-by-DTLS | LiveKit manages connection lifetime itself, via its own supervisor |

`rtc` already has replay-window and DTLS cipher configuration but none of these
three. Each becomes a `SettingEngine` flag with a unit test in the fork.

> **Caveat carried from the port plan.** This list is inferred from the Go call
> sites gated on `EnableWarp`, not from reading the fork: the pion fork could
> not be diffed against upstream when the plan was written. `EnableSped` and
> `EnableSctpSnap` semantics are still unknown. Read `livekit/webrtc-pion`
> before implementing, and correct this table from the source rather than from
> this document.

### Gap #13 — header extension id range

`media_engine.rs` hardcodes `VALID_EXT_IDS = 1..15`, which is the one-byte RTP
header extension form. LiveKit negotiates more than ten extensions per session
and needs the two-byte form from RFC 8285. Widen the range behind a flag.

Consumed in Phase 2.

### Gap #17 — a lost handshake flight is never retransmitted (new)

**Found while building Phase 0; not in the original gap table.**

`rtc` 0.21.0-rc.2 does not recover a handshake flight that is dropped. A single
lost packet at the wrong moment leaves both sides in `Connecting` indefinitely,
with ICE traffic continuing around it and no error reported on either side.

It is not a loss-rate threshold: runs at 1%, 2% and 5% loss all wedge on some
seeds and complete on others, which is the signature of a retransmission that
never happens rather than a backoff that exhausts.

Reproduced by `crates/lk-rtcio/tests/rtc_handshake_loss_recovery.rs`, which uses
two bare `RTCPeerConnection`s with no `Shard` and no mux between them, so the
SFU's own I/O layer is not a suspect. The test is `#[ignore]`d because it fails
today; it is the regression test for the fix.

**Severity: blocking for production.** An SFU on the public internet sees loss
constantly, and a client whose handshake is wedged looks to a user like a call
that never connects. This must be closed before Phase 1's exit gate, ahead of
gaps #4 and #13, which are not consumed until Phase 2.

It also constrains testing in the meantime: the plan's loss-based scenarios
(RTX repair, BWE convergence, allocator behaviour under 20% loss) can only be
run on connections that are already established.

---

## 4. Rebase runbook

Budget: about one engineer-week per month, per the port plan's risk table.
Upstream publishes weekly, so the fork is rebased weekly whether or not
anything looks urgent. A fork rebased on a schedule takes a small cost
repeatedly; one rebased when it becomes painful takes a large cost once, at the
worst possible moment.

```bash
# 1. Fetch upstream and rebase the stack onto the new release tag.
git fetch upstream
git checkout livekit/main
git rebase --onto upstream/v0.21.0-rc.3 upstream/v0.21.0-rc.2

# 2. Resolve conflicts one commit at a time. Each commit is one gap, so a
#    conflict is about one change and not about all of them.

# 3. Run the fork's own tests, including the unit test each gap added.
cargo test --workspace

# 4. Check the public API did not move underneath us.
cargo semver-checks check-release --baseline-rev upstream/v0.21.0-rc.2

# 5. Push and repin.
git push --force-with-lease fork livekit/main
#    Update the `rev` in every [patch.crates-io] entry to the new head.

# 6. Run the port's own suites against the new pin. The vnet suite is the one
#    that matters: it is fast, deterministic, and it is what found gap #17.
cd livekit-rs && cargo nextest run --workspace
```

`--force-with-lease`, never `--force`: the lease is what stops a rebase from
discarding a colleague's commit that landed while you were rebasing.

If step 4 reports a breaking change upstream, that is not a rebase problem to
work around; it is a porting task, and it gets its own commit in the workspace
rather than a compatibility shim in the fork.

---

## 5. Upstreaming policy

Gaps #4, #9, #13 and #17 are generic, not LiveKit-shaped: any SFU or any
application on a lossy network wants them. Open an upstream PR for each as soon
as it lands in the fork, before it has been proved out in the port. A fork whose
commits are also upstream PRs stays rebasable, because upstream eventually
carries the change and the commit drops out of the stack on its next rebase.

Gaps that stay in the workspace and are never upstreamed, because they are
LiveKit-shaped: #1 (UDP mux), #2 (TCP mux), #6 (dependency descriptor),
#10 (embedded TURN server).

Link each upstream PR from issue #981 as it is opened.
