# Known-unknowns register

A green gate means "green within the coverage below", not "no known unknowns".
This register lists what the gate does **not** exercise. It is deliberately
short and specific: an entry is a gap someone could close, not a disclaimer.

The gate prints the open entries with every result (`scripts/gate.sh`), so a
green run is read as qualified by this list. Record a new gap here when it is
identified rather than leaving it implicit.

## Platform coverage is Linux-only

CI and the default `scripts/gate.sh` run on Linux. Darwin's permanently
Stage-1 wait words, Windows/FreeBSD wait-word builds, and the pthread-backed
std behaviour macOS exposes are not asserted by the gate. `--native` on macOS
covers the host subset only and says so.

## Consumer writes to shared regions are trusted

A consumer granted a writable mapping can write anywhere in that region,
including ring coordination words it does not own. The platform does not
enforce consumer-write isolation; the dedicated
`hardened-consumer-shared-memory` change owns that boundary and its acceptance
coverage. Until it lands, shared-memory integrity assumes well-behaved
consumers rather than defending against a hostile co-tenant with a writable
mapping.

## Platform artifact support is bounded by the advertised runner matrix

The source gate builds and runs the Linux spine set. The release workflow builds
and smokes the runtime component on every advertised target (Linux x86_64/ARM64
GNU, macOS ARM64, Windows x86_64/ARM64 MSVC) by starting it and bootstrapping the
golden-path guest from an isolated package with no source checkout. Matrix rows
only stage archives; one aggregate job uploads them after every target passes.
What is still not covered: targets outside that matrix, the `sel` and
system-guest assets (not bundled yet), and running the full spine set on each
non-Linux target — the per-target smoke proves startup and guest bootstrap, not
the whole platform.

## Multi-host and distributed behaviour is not tested

Every spine binds loopback or a single host: no cross-machine networking,
partition, clock skew, or failover is exercised.

## Network conditions are benign

QUIC/TLS spines run over loopback with no packet loss, reordering, or
bandwidth limit. Congestion-control behaviour and retry storms under a hostile
network are unknown.

## Crash consistency is injected at named boundaries, not at machine level

The crash lane tears the on-disk framed log/manifest at every byte offset (a
short write) and corrupts a written record's checksum, then asserts replay is
the complete prefix; those tests are named `format_*` because they exercise the
record format, not a component. The store has real interruption/restart tests
between blob append and manifest publish and around injected append/sync
failures. Identity and control-plane currently have projection/replay tests,
but their failed-commit cases still use local commit helpers rather than
interrupting the real store append/sync path and restarting the component;
those helper-only cases are not component crash evidence. The required durable
agent hook is a fault at the actual `DurableLog::append`/`StoreRequest::Sync`
boundary, followed by a fresh component projection over the same volumes and
an exact acknowledged-prefix assertion. The gate still does not power-cut the
machine, fail a real `fsync`, kill a guest process mid-write from outside, or
corrupt the storage medium beneath the file, so filesystem-level ordering
guarantees are assumed, not tested.

## Load is bounded

The soak lane exercises repeated spawns, timers, and registrations at a rate a
CI runner can sustain. Sustained multi-tenant load, adversarial scheduling, and
hour-scale runs are not covered, so slow leaks beyond the soak window are
unknown.

## Adversarial wire input beyond the fuzz-smoke seed corpus

Hand-written codec tests cover malformed, truncated, and conflicting frames at
each parser, and `scripts/fuzz-smoke.sh` replays a checked-in seed corpus
through every hardening target. What remains unknown is coverage beyond each
lane's short wall-clock budget and the quality of the seed corpus; see the
continuous-fuzzing limitation below.

## Guest ABI drift is lockstep, not a compatibility window

Hostcall-using modules export `__selium_abi_version`, which the runtime checks
before guest execution, and every request retains a fixed magic/version
preamble ahead of rkyv dispatch. Legacy, older, and newer modules fail bootstrap
with both versions named. The reverse direction is **not** supported: a host
built before negotiation cannot interpret the new module contract, so mixed
host/guest versions are forbidden during the alpha. Artifacts are rebuilt and
released in lockstep; no broader compatibility window is claimed.

## Continuous fuzzing is not yet a merge gate

Short fuzz-smoke targets cover hostcall envelopes, multi-memory headers, frame
codecs, and module probing (`scripts/fuzz-smoke.sh`, seed corpus under
`fuzz/seeds/`), and run in a bounded CI lane. A scheduled nightly workflow
(`.github/workflows/fuzz-nightly.yml`) extends each target's budget and caches
the working corpus between runs. Neither lane is continuous fuzzing: coverage
beyond each budget, dictionary quality, and long-horizon discovery remain
unknown.

## Traceability is claimed, not proven

`scripts/check-traceability.sh` requires each annotation to sit directly above
a test function (trailing declaration blocks fail, and `--self-test` pins that
classifier), and proves that a scenario names such a test or a reviewed waiver.
It cannot prove that the test genuinely asserts the scenario: annotations are
reviewed when added and can rot when a test changes, so `--report` shows drift,
not intent.

Main specs have been reconciled to the current multi-memory, root-level SDK,
mio/std-networking, and connector behaviour. Remaining scenario-specific waivers
in `openspec/traceability-waivers.md` document structural properties or genuine
coverage limitations rather than removed APIs. Delta scenarios of an unparked
active change follow the same rule, so a change cannot be archived with
unannotated new scenarios.

## Soak and crash lanes run nightly, not per merge

A regression that only appears in the long lanes is merged before it is
reported. The nightly run is the first evidence, not a pre-merge guarantee.

## A slow blocking writer anchors the ring window by design

A blocking writer's slot bounds how far other writers may run ahead of its last
committed frame (documented Park backpressure). A writer that goes permanently
idle therefore caps the ring window at one capacity past its position, and the
window arithmetic is not fuzzed: a long-idle neighbour, or many writers with
skewed progress, is only covered by the hand-written slot tests in
`selium-shm` and by the spine/soak lanes. Writers no longer pin themselves
(`reserve_tail_for_writer` excludes the caller's own slot, and the slot advances
to the end of each committed frame), so the large-frame and late-join cases the
bridge's acceptance-frame test used to expose are covered; whether the
cross-writer window itself should be reader-relative remains a ring-protocol
design question rather than a fixed race.

## Watch-driven boot convergence still depends on discovery reachability

`Context::resolve_ready` and protocol-handler resolution converge on
route-watch events, but the initial authoritative `lookup` and the re-resolve
after each appearance still ride the discovery RPC path. A permanently stalled
discovery connection surfaces as a typed lookup error or the readiness bound,
not as a converged route: there is no host-side readiness sequencer behind it.

## A spine can miss its convergence bound under sequential load

The identity spine was observed hitting its onboarding convergence bound while
several long spines ran back to back. One cause was diagnosed: the kernel
poller's UDP rewrite ring dropped handshake datagrams under load, which surfaced
as handshake and route-convergence timeouts. The ring is now 1 MiB, the
connector's idle budget is 300 s, and the spine passes repeatedly on macOS.

The same spines were unreliable inside the Linux Docker container used by the
local gate: the connector logged `inbound ring full ... dropped datagram`, the
bridge's handoff receive timed out, and the client's control-channel open ended
as `connection lost`, while the identical tree passed natively. Root cause: when
the shared rewrite ring filled, the kernel poller kept draining the OS socket
and dropped every datagram it read, discarding the client's handshake
retransmissions while the interpreted connector was inside a long TLS
computation.

The fix is in the poller: on a full ring it stops consuming the OS socket and
sleeps 1 ms before re-arming, so the kernel receive buffer (and QUIC
retransmission) absorbs the burst instead of the ring draining it into drops.
Alongside it, the rewrite ring is 1 MiB, the connector's idle budget is 300 s,
clients send keep-alive probes, and the reviewed handshake bound is 420 s. The
identity spine now passes in the Linux container.

Residual risk: a burst that exceeds both the kernel receive buffer and the 1 MiB
ring can still drop, and the 1 ms pacing is a deliberate trade (a poller thread
briefly sleeps under sustained pressure). Sustained adversarial datagram load
is not covered; the bound is "the handshake completes", not "no packet is ever
dropped".
