## 1. Foundation Alignment

- [x] 1.1 Confirm implementation assumptions against `selium-runtime::SystemGuestDescriptor`, `ReadinessCondition`, and the zero-argument `#[entrypoint]` macro
- [x] 1.2 Update `ARCHITECTURE.md`, `SUMMARY.md`, or follow-up docs where they still describe stale system guest paths or `selium-guest` as the I/O pattern layer
- [x] 1.3 Define the system guest descriptors, scoped grants, dependencies, readiness conditions, and bootstrap order needed by `selium-runtime`
- [x] 1.4 Decide whether any missing request/reply, fanout, QUIC, or mTLS support is required in this change or must be split into a prerequisite change

## 2. System Guest Base

- [x] 2.1 Create workspace crates at `crates/guests/cluster`, `crates/guests/discovery`, `crates/guests/scheduler`, `crates/guests/supervisor`, and `crates/guests/external-api` with package names `selium-cluster`, `selium-discovery`, `selium-scheduler`, `selium-supervisor`, and `selium-external-api`
- [x] 2.2 Add zero-argument guest entrypoints and metadata using `selium-guest-macros`
- [x] 2.3 Add guest logging and tracing integration using `selium-guest`
- [x] 2.4 Define the `selium-io` topics, live tables, durable logs, network request exchanges, and interface metadata each guest exposes and consumes
- [x] 2.5 Add native tests for guest state machines wherever logic can be tested without Wasm deployment

## 3. Cluster Guest

- [ ] 3.1 Implement host membership tracking and shared host-state projection in the running guest entrypoint, not only native helpers
- [ ] 3.2 Implement host load visibility for placement consumers through an actual `selium-io` table/topic or kernel request-exchange resource
- [ ] 3.3 Implement day 1 single-host bootstrap for shared fabric state with an extension seam for cross-host bootstrap
- [ ] 3.4 Implement configured host-to-host coordination through the current network primitives, or record the missing QUIC/mTLS bridge as a prerequisite
- [ ] 3.5 Stub or defer DNS TXT record publishing behind an explicit day 1 boundary

## 4. Discovery Guest

- [ ] 4.1 Implement URI registration and removal flows in the running guest entrypoint, not only native helpers
- [ ] 4.2 Implement exact and prefix-based resolution flows through an actual guest-facing interface
- [ ] 4.3 Implement persistence for URI and interface metadata through durable host resources
- [ ] 4.4 Implement guest-facing discovery interfaces using explicit `selium-io` state/topics or network request exchanges
- [ ] 4.5 Ingest or reference macro-generated interface metadata where available through a reachable registration path

## 5. Scheduler Guest

- [ ] 5.1 Implement scheduler-owned durable/live state and reconciliation loops in the running guest entrypoint
- [ ] 5.2 Implement placement logic using host load, dependency, and isolation inputs from actual cluster/discovery surfaces
- [ ] 5.3 Implement request-exchange or typed-channel interfaces for placement and scaling intents where synchronous feedback is required
- [ ] 5.4 Implement status publication for workload state transitions through an actual topic/live table
- [ ] 5.5 Integrate scheduler state with cluster-provided host visibility and discovery-provided resolution data

## 6. Supervisor Guest

- [ ] 6.1 Implement runtime activity-log and metering subscriptions in the running guest entrypoint
- [x] 6.2 Implement managed-process health tracking and failure classification
- [x] 6.3 Implement restart-policy evaluation and backoff handling
- [ ] 6.4 Implement recovery or rescheduling intent emission through explicit scheduler-facing state, topic, or request-exchange interfaces
- [ ] 6.5 Integrate supervisor decisions with scheduler and runtime lifecycle events

## 7. External API Guest

> **Superseded by `implement-control-plane`.** The `selium-external-api` text
> protocol and raw-TCP listener are removed; the guest is renamed and re-spec'd
> as `selium-control-plane`, which serves typed FlatBuffers
> `ControlRequest`/`ControlResponse` over the bridge (no text grammar, no
> `TcpListener`). The text-protocol and TCP-listener implementation tasks in
> this section (7.1a–7.1f, 7.4c, 7.4d) are therefore no longer actionable.
> Decomposition and delegation (`DelegatedInteraction` semantics) moved into
> the `control-plane` capability; `SchedulerRequest`/`SchedulerResponse` moved
> into `selium-abi`.

### 7.1 External listener (previously blocked — now unblocked)

The kernel's `tcp_bind()` → `tcp_accept_loop()` → `run_proxy()` infrastructure already exists in `network_runtime.rs`, and the guest SDK exposes `TcpListener::bind()` + `TcpListener::accept()` + `TcpStream` (with `AsyncRead`/`AsyncWrite`). No additional runtime bridge is needed.

- [ ] 7.1a Add `bind_addr: String` field to `ApiContext` and a constructor that accepts it alongside the discovery `Context`
- [ ] 7.1b Replace the placeholder `external_api_main` entrypoint with a real accept loop: bind a `TcpListener` on `ApiContext::bind_addr`, call `mark_ready()`, then loop calling `listener.accept()` and spawning a handler per connection
- [ ] 7.1c Implement `handle_connection`: read bytes from `TcpStream` via `AsyncRead`, accumulate until newline (`\n`), pass the line to the request pipeline, write the `ClientFeedback` response back via `AsyncWrite`
- [ ] 7.1d Handle connection lifecycle: detect EOF (read returns 0), handle I/O errors gracefully, ensure the outbound ring writer count is decremented on connection close
- [ ] 7.1e Add `tokio` to `selium-external-api` dev-dependencies for native-mode tests (use `tokio::io::{AsyncReadExt, AsyncWriteExt}`). Note: full integration testing requires WASM mode because `TcpStream::attach_shared()` returns an error in native mode.
- [ ] 7.1f Grant `Capability::Network`, `Capability::SharedMemory`, and `Capability::HostQueue` to the external API guest in the runtime bootstrap config. `Network` is needed for `TcpListener::bind()`; `SharedMemory` for ring buffer attachment; `HostQueue` for `TcpListener::accept()` → `HostQueueRecv`.

### 7.2 Parsing and decomposition

- [x] 7.2a Define `UserIntent`, `DelegatedInteraction`, `ApiError`, and `ClientFeedback` types
- [x] 7.2b Implement `parse_intent` with full error coverage (EmptyRequest, UnknownCommand, MissingArgument, InvalidReplicaCount)
- [x] 7.2c Implement `decompose_intent` mapping all five `UserIntent` variants to ordered `DelegatedInteraction` lists
- [x] 7.2d Define the text-protocol grammar in module documentation
- [x] 7.2e Write unit tests for `parse_intent` (valid commands, all error variants)
- [x] 7.2f Write unit tests for `decompose_intent` (all five `UserIntent` → `DelegatedInteraction` mappings)
- [x] 7.2g Write end-to-end tests for the sync `accept_request_sync` pipeline

### 7.3 Discovery and scheduler delegation

Discovery dispatch is wired through `Context::lookup()`. Scheduler dispatch is stubbed pending scheduler guest implementation.

- [x] 7.3a Define `SchedulerRequest` and `SchedulerResponse` types (stub with `#[allow(dead_code)]` and TODO to move to `selium_abi` when scheduler crate is ready)
- [x] 7.3b Implement `dispatch_interaction`: route `DiscoveryResolve` to `Context::lookup()`, route scheduler interactions to a TODO stub that logs and succeeds
- [x] 7.3c Implement `dispatch_all` to dispatch a `&[DelegatedInteraction]` in order, returning the first error
- [x] 7.3d Map dispatch errors to `ApiError::DelegationFailed { step, context }`
- [ ] 7.3e Replace scheduler dispatch stub with real `RpcClient<SchedulerRequest, SchedulerResponse>` calls once the scheduler guest implements the RPC service (separate follow-up, tracked in section 5)

### 7.4 Client feedback

- [x] 7.4a Implement async `accept_request`: parse → decompose → dispatch → return `ClientFeedback { accepted: true, message, delegated }`
- [x] 7.4b Implement sync `accept_request_sync` for testing (parse → decompose → ClientFeedback, no dispatch)
- [ ] 7.4c Write `ClientFeedback` as a text response to the outbound `TcpStream` in `handle_connection` (part of 7.1c)
- [ ] 7.4d For parse/dispatch errors: write `ClientFeedback { accepted: false, message: "<error>", delegated: [] }` back to the client instead of dropping the connection

### 7.5 Error propagation

- [x] 7.5a `ApiError` variants cover all failure modes (EmptyRequest, UnknownCommand, InvalidReplicaCount, MissingArgument, DelegationFailed)
- [x] 7.5b `delegation_error()` helper constructs `DelegationFailed` with step and context
- [x] 7.5c `parse_intent` returns `ApiError` (not a string or generic error) for all parse failures

## 8. Integration

- [x] 8.1 Bootstrap all five system guests through `selium-runtime` configuration on a single host
- [ ] 8.2 Add end-to-end tests covering deploy, start, stop, scale, discovery, and restart flows through Wasm entrypoints or hostcall-visible guest resources
- [x] 8.3 Add minimal cross-host tests only for the coordination primitives implemented in this change
- [x] 8.4 Validate that capability scopes for each system guest match their intended authority boundaries
- [ ] 8.5 Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-targets`

## Definition of Done for Running Guests

The native state-machine helpers and tests are not sufficient completion evidence for sections 3-7. A guest task is complete only when its `#[entrypoint]` either:

- opens or consumes the named host resource through `selium-guest`/`selium-io` and performs the described behaviour, or
- records a deliberate day 1 boundary in code and documentation because the required host resource or bridge does not exist yet.

Public Rust functions in guest crates are not host-visible interfaces unless they are called by the entrypoint, exported as Wasm functions, or surfaced through a concrete host resource such as a request exchange, durable log, topic, or live table.

The external-api listener (7.1) was previously marked blocked on the assumption that no guest accept API or runtime network bridge existed. That reasoning is moot: `implement-control-plane` supersedes this section and removes the raw-TCP text listener entirely. The remaining text-protocol/TCP-listener tasks here are not actionable.

Scheduler dispatch (7.3e) remains blocked on the scheduler guest implementing `SchedulerPlace`/`SchedulerStop`/`SchedulerScale` RPC handlers (section 5). Until then, `deploy`, `start`, `stop`, and `scale` commands log and return success without side effects; only `resolve` works end-to-end.

`DurableLog` must not be used for system/boot logs; guest operational logs use tracing through `selium-guest`.

## 9. Documentation

- [x] 9.1 Document system guest responsibilities and dependencies
- [x] 9.2 Document the `selium-io`, durable storage, activity, metering, and network interaction choices used by each guest
- [x] 9.3 Document deferred work that belongs to channel replication, cluster scaling, and migration proposals
- [x] 9.4 Document any deferred runtime/network bridge work for QUIC and mTLS
