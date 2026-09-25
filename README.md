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
  memory alloc/attach, host queues, storage, process lifecycle, activity and
  guest logs
- Shared-memory channels (`selium-shm`): lock-free ring buffers with typed
  pub/sub, RPC, and live-table overlays (`selium-wire`)
- Structured guest logging over a shared-memory channel, drained by the host
- Config-driven bootstrap of WASM system guests with per-process capability
  grants
- Capability enforcement: real scope contexts (tenant, resource class,
  identity), grant-time selector admission, and an enforcement matrix — see
  `selium-abi` docs for the capability × selector table
- System guests built on that foundation: discovery (Tier-1 runtime feed
  registration, Tier-2 guest-driven URI resolution over shm RPC, and
  revocation on process exit), identity, accounting, control plane, the
  per-tenant bridge and per-stream bridge-channel, and the DNS/HTTP/QUIC edge
  connectors

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
`WaitRegister` → mailbox wake of the parked task, stall-kick outbound drain
(well under the bounded backstop), and EOF propagation:

```sh
cargo build --target wasm32-unknown-unknown -p selium-net-demo
cargo test -p selium-runtime --test net_wake -- --ignored
```

## Deferred (explicitly not working yet)

- Multi-host clustering (the system guests are single-host)
- Durable storage (current log and blob stores are in-memory)

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
| `crates/kernel` | Primitive host resources: shared memory, network, storage, processes, activity, metering |
| `crates/runtime` | Wasmtiny-backed execution, capability enforcement, system-guest bootstrap, hostcall dispatch |
| `crates/guest` | The guest SDK: hostcalls, async reactor, typed handles, tracing integration |
| `crates/guest/macros` | `#[entrypoint]`, `#[pattern_interface]`, `#[schema]` proc macros |
| `crates/client` | Native client SDK: QUIC + TLS + mTLS + wire framing behind typed channel handles |
| `crates/cli` | `sel` command-line control-plane client over the typed control surface |
| `crates/proto-dns` | DNS protocol wire types and DNS wire-format codec |
| `crates/proto-http` | HTTP protocol wire types (FlatBuffers) |
| `guests/discovery` | Discovery system guest (URI registration/resolution store + wiring) |
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

**Note:** the workspace depends on a sibling checkout of
[`wasmtiny`](https://github.com/itpetey/wasmtiny) via a path patch
(`../../wasmtiny`). Both repos must sit side-by-side for the build to
resolve.

## Contributing

See `AGENTS.md` for rules: stable Rust, edition 2024, no WASI, `tracing`
for logging, International English, and the pre-commit gate (fmt, clippy,
tests, wasm32 guest builds, spine test).

## Licence

MPL v2 (see `LICENCE`)
