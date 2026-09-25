# Design: Entrypoint Arguments

## Context

`wasm32-unknown-unknown`, no WASI: the only data a system guest sees at
boot is what the runtime hands it through its exported entrypoint. Today
that is zero to two `i64` scalars (or a `Context` wrapping the discovery
handle). Config strings — a DNS resolver address, HTTP TLS material —
cannot cross that boundary, so connectors would have to hard-code deploy
configuration. This change makes the entrypoint ABI signature-driven so
the host ships bytes and the guest owns their interpretation ("dumb host,
smart guest").

## Goals / Non-Goals

**Goals:**

- Guest entrypoints declare arbitrary integer + pointer parameters.
- The host writes pointer-argument bytes into guest memory and passes
  `(address, length)`.
- Existing entrypoints are byte-for-byte ABI-compatible.

**Non-Goals:**

- Passing structured/serialised types by value (guests decode raw bytes).
- Cross-host argument forwarding.
- Changing how `Context` (the discovery handle) is delivered.

## Decisions

### 1. Uniform `i64` slots on the wire

Every argument slot is a `WasmValue::I64`, matching the existing
u64-handle convention (`encode_u64_argument`). Integer parameters narrow
canonically in the generated wrapper (`arg as u32`, `arg as u16`, …). A
pointer argument expands on the host side into two consecutive `I64`
slots: `address` then `length`. On wasm32 an address fits `u32` and is
carried in the low bits of its `i64` slot; the wrapper downcasts.

### 2. Signature-driven wrapper generation

The macro classifies each parameter by its syntactic type:

- `Context` — leading only; the discovery-handle slot becomes
  `Context::from_raw(handle).await` (existing behaviour).
- `u8`/`u16`/`u32`/`u64`/`i8`/`i16`/`i32`/`i64` — one slot, `as`-narrowed.
- `(u64, u64)` — two slots, passed through as the tuple.
- anything else — compile error.

The generated `extern "C"` export takes exactly the total slot count of
`i64` parameters and re-binds them into the user function call. Arity is
known at expansion time, so no variadic exports are needed.

### 3. Descriptor carries structured arguments

`SystemGuestDescriptor.arguments` changes from `Vec<Vec<u8>>` (opaque
`WasmValue` bytes) to a structured list of
`Integer(u64)` / `Pointer(Vec<u8>)` entries. `Integer` descends to the
existing `WasmValue::I64` encoding unchanged; `Pointer(payload)` is the
self-describing case. `set_discovery_handle` /
`set_discovery_feed_and_handle` move to `Integer(...)` entries, so the
existing bootstrap tests continue to compile with `Vec::new()`.

### 4. Byte injection happens at invoke time

`execute_entrypoint` walks the structured arguments:

- `Integer(v)` → `WasmValue::I64(v)`
- `Pointer(bytes)` → grow the guest memory to append `bytes`, remember the
  base address, write the bytes, emit `WasmValue::I64(addr)` then
  `WasmValue::I64(len)`

This needs a guest-memory handle on the invoke path (wasmtiny exposes
`Instance::grow_memory` and `memory`); a thin accessor on the loaded
application is added if one is missing. If a payload cannot be written the
bootstrap fails loudly — bytes are never truncated or dropped (point 7,
"capabilities are honest").

### 5. Guest-side read helper

`selium_guest::args::{bytes, str}` wrap the single `unsafe` slice
construction. They are `unsafe` (the pointer is valid only because the
runtime wrote it) but documented, so the common config case is one call:
`let resolver = args::str(ptr, len)`.

## Risks / Trade-offs

- **wasmtiny memory access on the invoke path** is the one unknown; the
  change verifies (`Instance::memory`/`grow_memory`) and adds an accessor
  if needed. This is a fenced, small extension in step with the engine's
  purpose.
- **Descriptor type change** touches every system-guest descriptor
  literal, but all of them use `Vec::new()` or the two setters, so the
  surface stays small.
- **Uniform i64** is slightly larger than i32 for small integers; accepted
  in exchange for one code path and no new ABI.
