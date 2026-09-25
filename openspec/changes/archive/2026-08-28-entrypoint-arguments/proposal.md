# Proposal: Entrypoint Arguments

## Why

The `#[entrypoint]` macro hard-codes four fixed signatures (zero args, a
single `Context`, one `u64`, or two `u64`s), so system guests cannot
receive configuration data from the runtime. The DNS connector is the
first consumer that needs it — its upstream resolver address is
per-deployment config — and HTTP-connector TLS material is next. There is
no way today to express "the host ships these bytes to the guest" in a
guest signature.

## What Changes

- **`selium-guest-macros`**: replace the fixed entrypoint-argument shapes
  with a signature-driven parser. An entrypoint may take an optional
  leading `Context`, followed by any number of integer parameters
  (`u8`/`u16`/`u32`/`u64`, and signed equivalents) and pointer parameters
  declared as `(u64, u64)` = `(address, length)`. Integer parameters
  consume one argument each; pointer parameters consume two.
- **`selium-runtime`**: carry pointer-argument payloads in
  `SystemGuestDescriptor` and, at entrypoint invocation, copy each payload
  into the guest's linear memory and pass the `(address, length)` pair.
  Integer arguments keep the existing tagged `WasmValue` encoding.
- **`selium-guest`**: a small documented helper so a guest can read a
  `(u64, u64)` argument back as bytes (and as UTF-8) without repeating unsafe
  slice construction at every entrypoint.
- **BREAKING**: none. Existing zero/single/two-argument and `Context`
  entrypoints keep their exact behaviour and ABI.

## Capabilities

### New Capabilities

(None — this extends existing capabilities; see Modified.)

### Modified Capabilities

- `selium-guest-macros`: `Context-Aware Entrypoint Parameter` is generalised
  to arbitrary integer and pointer parameter lists.
- `selium-runtime`: entrypoint argument injection gains pointer-argument
  byte injection.
- `selium-guest`: gains an entrypoint-argument reader helper.

## Impact

- `crates/core/guest/macros`: signature parsing and ABI-wrapper codegen.
- `crates/core/runtime`: `SystemGuestDescriptor.arguments`, argument
  encoding, and byte injection at `execute_entrypoint`; requires a guest
  memory handle at invoke time (possibly a small wasmtiny addition).
- `crates/core/guest`: `args` reader helper.
- First consumer: `selium-connector-dns` passes `udp://<resolver>:53` as a
  pointer argument instead of a hard-coded resolver.
