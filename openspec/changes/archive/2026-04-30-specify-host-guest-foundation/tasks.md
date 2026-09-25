## 1. Workspace Foundation

- [x] 1.1 Add `selium-abi`, `selium-kernel`, `selium-runtime`, `selium-guest`, and `selium-guest-macros` to the workspace layout
- [x] 1.2 Add shared workspace dependencies and crate metadata for the foundation crates
- [x] 1.3 Set up cross-crate smoke tests to validate the crate graph builds coherently

## 2. `selium-abi`

- [x] 2.1 Define capability, selector, and compound-scope types in `selium-abi`
- [x] 2.2 Define resource identity and handle types for local and shared resources
- [x] 2.3 Define hostcall payloads, completion states, and explicit error outcomes
- [x] 2.4 Define canonical framing and codec helpers for ABI payload exchange
- [x] 2.5 Add ABI conformance tests for scope matching, framing, and error handling

## 3. `selium-kernel`

- [x] 3.1 Implement shared-memory region primitives over Wasmtiny's shared-memory support
- [x] 3.2 Implement explicit signalling and wait/notify primitives for guest coordination
- [x] 3.3 Implement protocol-neutral network primitives for listeners, sessions, streams, and request/response exchanges
- [x] 3.4 Implement durable storage primitives for logs, replay, checkpoints, blobs, and manifests
- [x] 3.5 Implement primitive process lifecycle resources plus activity and metering hooks

## 4. `selium-runtime`

- [x] 4.1 Embed Wasmtiny into `selium-runtime` and wire guest process execution through the kernel primitives
- [x] 4.2 Implement session-backed capability grants and compound-scope validation at spawn time
- [x] 4.3 Implement declarative system guest descriptors for module, entrypoint, arguments, grants, dependencies, and readiness
- [x] 4.4 Implement generic config-driven bootstrap of system guests without guest-specific startup code
- [x] 4.5 Project lifecycle events and metering observations into the host activity log

## 5. `selium-guest`

- [x] 5.1 Implement safe typed handles over ABI primitives for shared memory, storage, network, and process resources
- [x] 5.2 Implement the prototype messaging-pattern layer for pub/sub, fanout, request/reply, streams, and live tables, with host-backed inter-guest transport deferred
- [x] 5.3 Implement typed codec helpers that map guest types onto canonical ABI framing
- [x] 5.4 Implement tracing and guest log integration over guest-visible log resources
- [x] 5.5 Add native-test fallbacks and coverage for pattern-layer behaviour

## 6. `selium-guest-macros`

- [x] 6.1 Implement the guest entrypoint macro that generates ABI-compatible glue
- [x] 6.2 Implement macro-generated metadata for guest-declared messaging interfaces
- [x] 6.3 Add integration tests covering macro interaction with guest tracing and runtime bootstrap

## 7. Integration and Alignment

- [x] 7.1 Add end-to-end tests that bootstrap system guests from runtime configuration using the new foundation crates
- [x] 7.2 Update dependent proposals and architecture notes to use the new crate names and messaging-pattern framing
- [x] 7.3 Document the foundation boundaries so later system guest work can depend on them cleanly
