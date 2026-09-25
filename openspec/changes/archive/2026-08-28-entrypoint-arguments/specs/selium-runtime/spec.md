## ADDED Requirements

### Requirement: Entrypoint Argument Injection

`selium-runtime` SHALL inject entrypoint arguments by decoding each
`SystemGuestDescriptor` argument into tagged `WasmValue`s. Integer
arguments SHALL be encoded as `WasmValue::I64`. Pointer arguments SHALL
carry a byte payload that the runtime copies into the guest's linear
memory before invoking the entrypoint; the pair `(address, length)` SHALL
then be encoded as two consecutive `WasmValue::I64` slots (address first,
then length).

#### Scenario: Integer argument encoded as i64

- **WHEN** a descriptor declares an integer argument
- **THEN** `decode_wasm_arguments` decodes it without error for all possible u64 values

#### Scenario: Pointer argument bytes injected into guest memory

- **WHEN** a descriptor declares a pointer argument with payload bytes
- **THEN** the runtime copies the payload into the guest's linear memory before invoking the entrypoint
- **AND** the entrypoint receives two `i64` arguments: the address the bytes were written at, and the byte length

#### Scenario: Pointer argument layout is declaration-ordered

- **WHEN** a descriptor declares an integer argument followed by a pointer argument
- **THEN** the entrypoint receives the integer in the first slot and the pointer pair in the following two slots

#### Scenario: Oversized pointer payload rejected

- **WHEN** a pointer-argument payload cannot be written into the guest's linear memory
- **THEN** the runtime SHALL fail the bootstrap with a descriptive error rather than truncating or silently dropping the payload
