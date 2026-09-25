## 1. Pub/Sub Generation-Change Detection

- [x] 1.1 Add `Error::Overwritten` variant to `guest/src/error.rs` with a descriptive message
- [x] 1.2 Implement generation-change check in `Subscriber<T>::recv`: load current generation counter, compute `delta = current - last_generation`, return `Error::Overwritten` if `delta > capacity`
- [x] 1.3 Update `last_generation` after each successful read in `Subscriber<T>::recv`
- [x] 1.4 Write a unit test: simulate fast publisher by manually advancing generation counter past capacity, verify subscriber returns `Error::Overwritten`
- [x] 1.5 Write a unit test: verify normal publishing (generation advances within capacity) does not trigger overwrite detection

## 2. Non-Blocking Reader Poll

- [x] 2.1 Rename `has_ready_frame` to `poll_ready` on `StrongReader` and make it `pub` (currently `pub(crate)`)
- [x] 2.2 Add doc comment explaining that `poll_ready` is non-blocking, returns `Ok(true)` if a frame is immediately readable
- [x] 2.3 Update any internal callers of `has_ready_frame` to use `poll_ready` (audit: currently no callers, so this is a no-op)
- [x] 2.4 Write a unit test: write a frame, call `poll_ready`, verify `Ok(true)`; call on empty ring, verify `Ok(false)`

## 3. Kernel State Cleanup

- [x] 3.1 Remove `page_offset`, `prot`, and `reader_slot` fields from `SharedMappingState` in `kernel/src/state.rs`
- [x] 3.2 Remove `#[derive(Clone, Copy)]` if no longer needed (or keep if remaining fields require it)
- [x] 3.3 Update construction sites for `SharedMappingState` to omit removed fields
- [x] 3.4 Verify kernel crate compiles with no warnings: `cargo check -p selium-kernel`

## 4. External API Spec and Stub Graduation

- [x] 4.1 Define the text-protocol grammar in `external-api/src/lib.rs` module documentation
- [x] 4.2 Define `ApiContext` struct containing pre-connected `RpcClient<DiscoveryRequest, DiscoveryResponse>` and `RpcClient<SchedulerRequest, SchedulerResponse>`
- [x] 4.3 Update `external_api_main` entrypoint to accept `ApiContext` parameter via `#[entrypoint]` macro
- [x] 4.4 Implement the TCP connection read loop: read bytes from inbound ring buffer, accumulate until newline, parse request
- [x] 4.5 Dispatch `DelegatedInteraction::DiscoveryResolve` to discovery RPC client and collect results
- [x] 4.6 Dispatch `DelegatedInteraction::SchedulerPlace`/`Stop`/`Scale` to scheduler RPC client
- [x] 4.7 Write `ClientFeedback` response back to outbound ring buffer
- [x] 4.8 Define `SchedulerRequest` and `SchedulerResponse` types (or stub them with TODO if scheduler crate isn't ready)
- [x] 4.9 Write unit tests for `parse_intent` with valid and invalid inputs
- [x] 4.10 Write unit tests for `decompose_intent` mapping each `UserIntent` variant to correct `DelegatedInteraction` list
- [x] 4.11 Write unit tests for `accept_request` end-to-end parsing pipeline

## 5. Specs

- [x] 5.1 Write `external-api` spec: requirements for text protocol, parsing pipeline, delegation dispatch, inbound bridge interface
- [x] 5.2 Update `selium-guest` spec: add pub/sub generation-change detection requirement, add non-blocking poll requirement
- [x] 5.3 Update `selium-kernel` spec: remove or update requirement referencing vestigial `SharedMappingState` fields

## 6. Verification

- [x] 6.1 Run full workspace build: `cargo build --workspace`
- [x] 6.2 Verify zero warnings remain (excluding known `selium-discovery` warnings): `cargo build --workspace 2>&1 | grep -c warning`
- [x] 6.3 Run full test suite: `cargo test --workspace`
- [x] 6.4 Verify `selium-external-api` crate compiles and tests pass
- [x] 6.5 Run `cargo clippy --workspace` and fix any new lints
