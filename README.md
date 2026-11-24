# Selium

Selium is a software-defined cloud infrastructure platform: application
stacks are composed entirely in code — typed I/O channels, strict
capabilities, and zero external configuration — and executed as WebAssembly
guests on a minimal host.

The design intention, in one sentence: **developers should build and own the
whole stack without touching traditional infrastructure or networking.**
See [`DESIGN-INTENT.md`](DESIGN-INTENT.md) for the non-negotiables, the
rejected alternatives, and the invariants that guide contributions.

## What runs today

The platform's **spine** and its **system guests** work and are continuously
tested:

- Real WASM guests (`wasm32-unknown-unknown`, no WASI) executing on wasmtiny
- The hostcall ABI (`selium-abi`, rkyv-encoded): capability-gated shared
  memory alloc/attach, host queues, durable volumes, process lifecycle,
  activity and guest logs
- Shared-memory channels (`selium-shm`): lock-free ring buffers with typed
  pub/sub, RPC, and live-table overlays (`selium-wire`)
- Structured guest logging over a shared-memory channel, drained by the host
- Config-driven bootstrap of WASM system guests with per-process capability
  grants and a fixed magic/version hostcall preamble; host and guest packages
  share `SELIUM_ABI_VERSION` in lockstep, and a mismatched guest is rejected
  before its request is decoded
- Capability enforcement: real scope contexts (tenant, resource class,
  identity), grant-time selector admission, and an enforcement matrix — see
  `selium-abi` docs for the capability × selector table
- **Durable storage**: the host exposes opaque named volumes (open, append,
  read-at, length, list, remove, sync) rooted at a state directory, and the
  `store` system guest owns all storage semantics over them — record framing,
  sequences, checkpoints, content-addressed blob ids, manifests, per-tenant
  layout and crash-consistent recovery — served as typed RPC on
  `sel:///store`. The host never interprets a byte it stores
- **A stateful boot**: the runtime takes a state directory (`--state-dir` /
  `SELIUM_STATE_DIR`), boots the store before its dependants, persists the
  online intermediate, per-tenant CAs, and the root public certificate (the
  offline root private key is never written to online state), and
  reconstructs per-tenant storage usage from the resident volumes before any
  guest runs. A power cycle preserves tenants, principals, deployments,
  uploaded modules and certificate chains
- System guests built on that foundation: discovery (Tier-1 runtime feed
  registration, Tier-2 guest-driven URI resolution over shm RPC, and
  revocation on process exit), the store, identity, accounting, control plane,
  the per-tenant bridge and per-stream bridge-channel, and the DNS/HTTP/QUIC
  edge connectors

The proof is the integration test suite:

**Spine** — a real guest (`integration/spine-demo`) is compiled to WASM,
bootstrapped by the runtime, creates shared-memory channels, completes a
typed pub/sub round trip, and streams structured logs back to the host:

```sh
cargo build --target wasm32-unknown-unknown -p selium-spine-demo
cargo test -p selium-runtime --test spine -- --ignored
```

**Discovery** — the discovery system guest and a probe fixture guest
(`integration/discovery-probe`) are compiled to WASM and bootstrapped
together: the runtime injects discovery wiring (feed ring + RPC listener),
both guests reach readiness, Tier-1 region registration events flow through
the feed, Tier-2 RPC resolution works between two real WASM guests, and URI
revocation fires on process exit:

```sh
cargo build --target wasm32-unknown-unknown -p selium-discovery -p selium-discovery-probe
cargo test -p selium-runtime --test discovery -- --ignored
```

**Network wake** — a real guest (`integration/net-demo`) binds a listener
and parks a read on an inbound ring; a host TCP client connects and writes.
Verifies the event-driven proxy paths end-to-end: kernel-poller accept,
`WaitRegister` → mailbox wake of the parked task, exact echo evidence for
stall-kick outbound drain, and EOF propagation. Generous bounds classify a
missing wake; they are not sub-second latency assertions:

```sh
cargo build --target wasm32-unknown-unknown -p selium-net-demo
cargo test -p selium-runtime --test net_wake -- --ignored
```

**External HTTPS** — a plain `rustls` client (no Selium SDK) completes TLS
against the real HTTP connector WASM guest, is routed through discovery to the
real `guests/http-demo` application guest, and receives the guest's
response byte-for-byte, including chunked streaming under a pipelined load that
exceeds the bounded forwarding window, a typed 400 for malformed transfer
codings, an exact 404 for an unregistered route, and a typed 413 for an
oversized request. The connector refuses to start without TLS material rather
than serving plaintext:

```sh
cargo build --release --target wasm32-unknown-unknown \
  -p selium-discovery -p selium-store -p selium-connector-http -p selium-http-demo
cargo test -p selium-runtime --test https_spine -- --ignored
```

**CLI** — the real `sel` binary runs as a separate process against the real
connector, bridge, and control plane (identity mTLS onboarding included). A
successful `sel status api` exits zero and writes exactly `workload api: 3
replicas, module api/v1` plus a newline to stdout; an unknown workload exits
non-zero and writes `not found` to stdout. Typed outcomes do not write transport
diagnostics to stderr. Each invocation opens one connection and sends one
request:

```sh
cargo test -p selium-runtime --test identity_spine -- --ignored
```

## Deferred (explicitly not working yet)

- Multi-host clustering (the system guests are single-host)
- Scheduler placement: the control plane records desired state, but a
  `deploy`/`scale`/`stop` that needs scheduler application returns the typed
  deferred-delegation status (`scheduler service not yet online`) and a
  non-zero CLI exit. The scheduler guest itself is frozen (see below)
- Storage retention: log truncation, blob deletion and compaction. Durable
  bytes accumulate; quotas cap a tenant's resident bytes but nothing reclaims
  them yet
- A bulk storage path: today a write travels ring → store guest → volume
  hostcall → disk, and reads are chunked. Zero-copy payloads (slice-source
  appends) and file-backed mapped reads are the two documented follow-ups
- Consumer write isolation: a guest with a writable mapping can corrupt ring
  coordination words it does not own. The `hardened-consumer-shared-memory`
  change owns closing that boundary; until it lands, shared-memory integrity
  assumes well-behaved consumers (see `docs/known-unknowns.md`)

The `guests/cluster`, `guests/scheduler`, and `guests/supervisor` guests are
retained in-tree, frozen for later increments: they are not members of the
workspace and do not build.

## Repository layout

| Crate | Role |
| --- | --- |
| `crates/abi` | Canonical host↔guest contract: capabilities, scopes, hostcall payloads, framing |
| `crates/service` | Service message types and FlatBuffers codecs (schema bindings, log record types) |
| `crates/memory` | `RegionMapping`/`RegionProvider` shared-memory abstraction |
| `crates/shm` | Shared-memory ring channels (`Channel`, `RingBuf`, blocking/non-blocking readers/writers) |
| `crates/wire` | Transport-agnostic framing + pub/sub, RPC, live-table patterns |
| `crates/kernel` | Primitive host resources: shared memory, network, durable volumes, processes, activity, metering |
| `crates/runtime` | Wasmtiny-backed execution, capability enforcement, system-guest bootstrap, hostcall dispatch |
| `crates/guest` | The guest SDK: hostcalls, async reactor, typed handles, tracing integration |
| `crates/guest/macros` | `#[entrypoint]`, `#[pattern_interface]`, `#[schema]` proc macros |
| `crates/client` | Native client SDK: QUIC + TLS + mTLS + wire framing behind typed channel handles |
| `crates/cli` | `sel` command-line control-plane client over the typed control surface |
| `crates/proto-dns` | DNS protocol wire types and DNS wire-format codec |
| `crates/proto-http` | HTTP protocol wire types (FlatBuffers) |
| `guests/discovery` | Discovery system guest (URI registration/resolution store + wiring) |
| `guests/store` | Durable store system guest: the platform's only owner of durable-log and blob-store semantics (framing, sequences, checkpoints, content ids, manifests, crash-consistent recovery) over host volumes |
| `guests/identity` | Identity system guest (tenant CA and user-certificate lifecycle, registries, anchor and grant publication) |
| `guests/accountant` | Accounting system guest (metering reduction, billing, quota, narrowing, and rate-limit authoring) |
| `guests/control-plane` | Control-plane system guest (typed RPC control surface and desired-state ownership) |
| `guests/connector-dns` | DNS egress connector system guest |
| `guests/connector-http` | HTTP/1.1 edge connector system guest |
| `guests/connector-quic` | QUIC edge connector system guest |
| `guests/bridge` | Per-tenant bridge server system guest (authenticates clients, spawns bridge-channels) |
| `guests/bridge-channel` | Per-stream bridge channel system guest (attaches a relayed region and splices frames) |
| `integration/discovery-probe` | Discovery probe test fixture guest (exercises Tier-2 RPC against discovery) |
| `integration/spine-demo` | Golden-path demo guest used by the spine test |
| `integration/net-demo` | Network demo guest used by the event-driven proxy wake test |
| `integration/dns-demo` | DNS resolution demo guest used by the DNS spine test |
| `integration/quic-demo` | QUIC echo demo guest used by the QUIC spine test |
| `integration/identity-onboard` | Onboarding operator test guest for the identity golden-path test |
| `integration/mt-demo` | Multithreaded demo guest used by the guest-worker-pool tests |

## Building

Requires stable Rust and the `wasm32-unknown-unknown` target:

```sh
rustup target add wasm32-unknown-unknown
cargo build --workspace
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

**Note:** the workspace pins [`wasmtiny`](https://github.com/seliumlabs/wasmtiny)
by git revision in the root `Cargo.toml`, so a plain checkout builds without a
sibling directory. For engine development, uncomment the path override in that
file and keep this repository and `wasmtiny` side-by-side.

## Releases

Publication is gated on the same Linux gate as merge and is currently
component-only; the crates.io lane stays disabled until the dependency patches
are published under owned packages. See `docs/release-targets.md` for the
advertised targets, the smoke each runs, and the support rule, and
`docs/release-process.md` for the lockstep versioning and rehearsal flow.

## Contributing

See `AGENTS.md` for rules: stable Rust, edition 2024, no WASI, `tracing`
for logging, International English, and the pre-commit gate (fmt, clippy,
tests, wasm32 guest builds, spine test).

## Licence

MPL v2 (see `LICENCE`)
