## Why

The current arch3 proposals assume a host/guest foundation that is still underspecified. System guest work already depends on ABI contracts, runtime bootstrap, kernel primitives, and guest-side messaging ergonomics, but those boundaries are not yet captured clearly enough to guide implementation.

## What Changes

- Define `selium-abi` as the explicit host/guest contract for capabilities, compound scopes, resource identity, codecs, and async hostcall semantics.
- Define `selium-kernel` as the primitive surface for shared memory, signalling, network, storage, process lifecycle, activity logging, and metering.
- Define `selium-runtime` as the Wasmtiny-based execution and bootstrap layer, including config-driven system guest startup and generic capability grants.
- Define `selium-guest` as the guest SDK that wraps the ABI with safe handles, typed codecs, tracing integration, and a messaging-pattern layer.
- Define `selium-guest-macros` as the macro layer for guest entrypoints and generated pattern/service metadata.
- Reframe guest communication around messaging patterns such as pub/sub, fanout, request/reply, streams, and live tables, instead of treating RPC as a privileged architectural primitive.
- Replace the old queue-centric mental model with a shared-memory-first substrate, reflecting Wasmtiny's hot-pluggable shared memory support.

## Capabilities

### New Capabilities
- `selium-abi`: Low-level host/guest contract covering capabilities, scopes, handles, hostcalls, codecs, and completion semantics.
- `selium-kernel`: Host primitive resource model covering shared memory, signalling, network, storage, process lifecycle, activity logs, and metering hooks.
- `selium-runtime`: Wasmtiny-backed execution, capability enforcement, session handling, and generic config-driven bootstrap of system guests.
- `selium-guest`: Guest SDK with safe handles, typed wrappers, tracing/logging integration, and messaging-pattern composition.
- `selium-guest-macros`: Procedural macros for guest entrypoints and generated metadata for guest-facing messaging patterns.

### Modified Capabilities

## Impact

- Creates the missing specification foundation for `crates/abi/`, `crates/kernel/`, `crates/runtime/`, `crates/guest/`, and `crates/guest/macros/`.
- Provides the dependency contracts that `openspec/changes/implement-system-guests/` currently assumes but does not define.
- Aligns arch3 with Wasmtiny's shared-memory capabilities, reducing dependence on queue-shaped workarounds inherited from older runtimes.
- Changes the expected abstraction boundary for guest communication from RPC-first APIs to a general messaging-pattern layer.
