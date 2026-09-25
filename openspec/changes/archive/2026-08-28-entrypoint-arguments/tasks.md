# Tasks: Entrypoint Arguments

## 1. Descriptor and runtime encoding

- [x] 1.1 Introduce `SystemGuestArg::{Integer(u64), Pointer(Vec<u8>)}`; change `SystemGuestDescriptor.arguments` to `Vec<SystemGuestArg>`; port `set_discovery_handle`/`set_discovery_feed_and_handle`.
- [x] 1.2 Extend `execute_entrypoint` to flatten `SystemGuestArg` into `WasmValue::I64` slots, growing guest memory and copying `Pointer` payloads (verify wasmtiny `memory`/`grow_memory`; add a small accessor if missing).

## 2. Entrypoint macro

- [x] 2.1 Replace the fixed-shape generator with a signature-driven classifier: `Context` (leading), integer types (one slot, `as`-narrowed), `(u64, u64)` (two slots), anything else a compile error.
- [x] 2.2 Generate the `extern "C"` export with exactly the total slot count and re-bind slots into the user function call (Context + trailing args included).

## 3. Guest reader helper

- [x] 3.1 Add `selium_guest::args::{bytes, str}` (unsafe, documented) reconstructing a pointer argument.

## 4. Tests and gates

- [x] 4.1 Macro tests: integer narrowing, pointer tuple, Context + pointer, unsupported-type compile error (trybuild).
- [x] 4.2 Runtime tests: integer slot encoding for all u64 values; pointer bytes land in guest memory and the entrypoint receives `(addr, len)`; oversized payload fails bootstrap.
- [x] 4.3 Gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`, wasm32 guest builds green.
