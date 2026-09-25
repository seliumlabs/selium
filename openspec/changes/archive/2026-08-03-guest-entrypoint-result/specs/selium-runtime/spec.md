## ADDED Requirements

### Requirement: Entrypoint Failure Detection
`selium-runtime` SHALL check the Wasm return value after executing a guest entrypoint and fail fast if it indicates an error.

#### Scenario: Non-zero exit code fails bootstrap
- **WHEN** `execute_entrypoint` returns `[WasmValue::I32(1)]`
- **THEN** the runtime SHALL return `Error::EntrypointFailed` before reaching the readiness check
- **AND** the existing cleanup path SHALL be invoked (record `ProcessExited`, `cleanup_failed_process`)

#### Scenario: Zero exit code proceeds normally
- **WHEN** `execute_entrypoint` returns `[WasmValue::I32(0)]`
- **THEN** the runtime SHALL proceed to the readiness check as normal

#### Scenario: Void entrypoint (no return value) proceeds normally
- **WHEN** `execute_entrypoint` returns an empty `Vec<WasmValue>` (existing `()`-returning entrypoints)
- **THEN** the runtime SHALL proceed to the readiness check as normal
