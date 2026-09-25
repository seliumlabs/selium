## 1. Crate Rename and Scaffolding

- [x] 1.1 Rename `guests/external-api` to `guests/control-plane` (package `selium-control-plane`) and update crate references; verify the crate declares its new name and no `selium-external-api` references remain outside the superseded `implement-system-guests` artifacts
- [x] 1.2 Remove the text-protocol parser and `TcpListener` wiring from the crate (entrypoint skeleton stays); verify no `TcpListener`, `parse_intent`, or text-grammar code remains via `grep` and the retained unit tests still compile
- [x] 1.3 Add `selium-control-plane` to the workspace members with deps on `selium-abi`, `selium-guest`, `selium-wire`, `selium-shm`, and `selium-encoding`; verify `cargo check -p selium-control-plane` passes

## 2. Control Message Schemas

- [x] 2.1 Define `ControlRequest`/`ControlResponse` FlatBuffers message bindings (deploy, scale, stop, resolve, upload, status variants) in the shared encoding surface; verify a codec round-trip unit test passes
- [x] 2.2 Move `SchedulerRequest`/`SchedulerResponse` into `selium-abi` as typed messages (resolving the old external-api stub TODO); verify a round-trip test passes

## 3. Served Channel and RPC Loop

- [x] 3.1 Implement the entrypoint to create its own host-queue listener and register the serving route via `Context::serve(["control"])`, marking ready only after registration (mirroring `bridge-server`); verify a native test observes the registration and readiness ordering
- [x] 3.2 Run a `selium_wire::rpc` `RpcConnection<ControlRequest, ControlResponse>` accept loop over the served listener; verify a native test where a client half sends a typed request and receives a correlated reply

## 4. Desired-State Ownership and Projection

- [x] 4.1 Open the control plane's durable log and append each accepted intent; verify a native test records an accepted deployment intent
- [x] 4.2 Materialise the deployment/pipeline live-table projection from durable-log replay; verify a test reconstructs the table from appended records
- [x] 4.3 Serve deployment/status reads over the control surface from the projection; verify a request/response test returns the last accepted desired state

## 5. Narrow Delegation

- [x] 5.1 Route resolve requests through `Context::lookup` to discovery; verify an end-to-end resolve against a native discovery seam
- [x] 5.2 Route placement/scale/stop to a typed `SchedulerRequest` RPC client, stubbed until the scheduler service lands, returning a typed delegated status; verify dispatch unit tests and the seam the real client replaces

## 6. Module Upload via Storage Hostcalls

- [x] 6.1 Store uploaded module bytes with `StorageBlobPut` and record a manifest with `StorageBlobSetManifest`; verify a test that stores bytes and reads back the manifest
- [x] 6.2 Record a deployment's module reference by blob identity or manifest name in desired state; verify the recorded reference appears in the projection

## 7. Workspace, Tests, and Integration

- [x] 7.1 Verify the workspace builds with the crate enabled: `cargo check --workspace` and `cargo build -p selium-control-plane --target wasm32-unknown-unknown`
- [x] 7.2 Add native state-machine and codec tests mirroring the discovery guest's test style; verify `cargo test -p selium-control-plane` passes
- [x] 7.3 Document the control-plane `CapabilityGrant` set (storage, shared memory, host queue) and add an authority-boundary check that a client without a control-plane grant is refused; verify the refusal test passes
- [x] 7.4 Add a single-host bootstrap test that starts `selium-control-plane` alongside discovery via `SystemGuestDescriptor` (gated on those guests being bootstrappable); verify the control route registers and readiness fires

## 8. Protocol-Aware Bridge Rendezvous

- [x] 8.1 Dispatch in `bridge_pipe` on the resolved target's class: channel/`SharedRegion` targets keep the existing transparent splice unchanged, host-queue targets take the rendezvous path; verify the existing `bridge-channel` splice and pub/sub tests pass unmodified (regression)
- [x] 8.2 Implement the host-queue rendezvous in `bridge-channel`: allocate the two-ring session region (mirroring `rpc::connect`), splice the client stream into it (request frames stream → request ring; reply ring → stream), and enqueue the session's shared id into the served queue via `ResourceSender` (handoff metadata is empty until guest `Process::start` supports pointer arguments; identity pass-through is a follow-up, authorization rests on the runtime's attach enforcement); verify a native seam test where a stream stand-in exchanges a correlated typed request/reply with a server `rpc::accept` through the rendezvous end-to-end
- [x] 8.3 Teardown parity: free the session region on stream close or fabric close so the serving guest observes session end (mirroring `OwnedRpcClient` drop); verify the server-side session end finishes the client's stream and no session region leaks (attach after teardown fails)
- [x] 8.4 Verify the enforcement path: the bridge-channel's discovery lookup records the queue in `resolved_queue_ids` (discovery `Resolve` already records via `record_resolved_queue_for`) and `ResourceSender::attach` passes the runtime's `HostQueueAttach` enforcement; attach is refused without a recorded resolution (covered by the existing `non_discovery_process_cannot_self_authorize_attach` runtime test)
- [x] 8.5 Add an external-reachability integration test (gated like 7.4): a native `selium-client` RPC session reaches the control plane's served route through the bridge and exchanges a typed `deploy`/`status` round trip end-to-end

## 9. Supersession and Validation

- [x] 9.1 Record a superseded note on `implement-system-guests` section 7 (external-api) pointing at this change; verify no external-api text-protocol tasks remain actionable there
- [x] 9.2 Run `openspec validate implement-control-plane --strict`, `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-targets`; verify all pass
