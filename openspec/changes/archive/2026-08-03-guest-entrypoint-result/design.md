## Context

Guest entrypoints currently return nothing across the Wasm boundary. The macro-generated `extern "C" fn` has no return type, and `run_entrypoint_safely` is hardcoded to `Future<Output = ()>`. The host's `call_function` gets back an empty `Vec<WasmValue>` every time.

We want to let entrypoints return `Result<()>` so guests can use `?` for error propagation. The error message is logged on the guest side; the host gets a boolean signal (0 = success, 1 = error).

## Goals / Non-Goals

**Goals:**
- Accept `async fn NAME(args) -> Result<()>` and `fn NAME(args) -> Result<()>` in `#[entrypoint]`
- Return `i32` across the Wasm boundary: `0` for `Ok(())`, `1` for `Err(e)`
- Log the error via `selium_guest::error!` before returning
- Host fails fast on non-zero exit code, before readiness check
- Existing `()`-returning entrypoints continue to work without changes

**Non-Goals:**
- `Result<T, E>` for `T != ()`
- Structured error propagation (rkyv-encoded hostcalls)
- Changing panic behavior

## Decisions

### 1. Wasm `i32` return value for the success/failure signal

The macro-generated `extern "C" fn` changes from returning nothing to returning `i32`. The wrapper spawned around the user's function matches the `Result` and returns `0` or `1`.

**Rationale:** Wasm natively supports `i32` returns. The host already captures `Vec<WasmValue>` from `call_function`. No new hostcall imports or shared memory conventions needed.

**Alternative:** Hostcall-based result reporting. Rejected as over-engineered for a boolean signal.

### 2. New `run_entrypoint_with_result` function

`run_entrypoint_safely` is hardcoded to `Future<Output = ()>`. Rather than changing its signature (and all existing call sites in the macro), we add a new function:

```rust
pub fn run_entrypoint_with_result<F, T>(future: F) -> T
where
    F: Future<Output = T> + 'static,
{
    let join = spawn(future);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        poll_safely();
    }));
    if result.is_err() {
        std::process::abort();
    }
    join.take_result().expect("entrypoint task must have completed")
}
```

`JoinHandle` gets a `pub(crate) fn take_result(&self) -> Option<T>` that drains the stored output from the `JoinState`.

**Rationale:** Keeps `run_entrypoint_safely` unchanged for backward compat. The new function is a thin wrapper — spawn, poll reactor, extract result.

**Alternative:** Change `run_entrypoint_safely` to be generic over `T`. Rejected to avoid touching existing call sites and changing the semantics of the existing name.

### 3. Error type bound: `std::error::Error`

The macro generates code that logs `{e}` (Display). Rather than relying on the compiler producing a confusing "Display not implemented" error from the format string, the generated code includes a static assertion or uses `E: std::error::Error` on a helper function so users get a clear error message.

**Rationale:** `std::error::Error` implies `Display + Debug`. Every practical error type implements it (`anyhow::Error`, `GuestError` via `thiserror`, `std::io::Error`).

### 4. Host-side fail fast

After `execute_entrypoint` captures `Vec<WasmValue>`, a new check:

```rust
if results == [WasmValue::I32(1)] {
    return Err(Error::EntrypointFailed(descriptor.name.clone()));
}
```

This triggers the existing cleanup path in `spawn_system_guest` (record `ProcessExited`, `cleanup_failed_process`). The readiness check never runs for a failed entrypoint.

**Rationale:** If the guest entrypoint returned an error, waiting for a readiness signal is pointless — the guest never reached `mark_ready()`.

### 5. Macro: detect `Result<()>` by matching the return type AST

The macro currently rejects anything that isn't `ReturnType::Default`. Instead, when it sees a return type, it checks whether it's `Result<()>`. If so, it generates the `-> i32` variant with result matching. If it's anything else, it produces a clear compile error.

Generated code for an async one-arg entrypoint returning `Result<()>`:

```rust
#[unsafe(export_name = "discovery_probe")]
pub extern "C" fn __selium_guest_entrypoint_discovery_probe(handle: i64) -> i32 {
    ::selium_guest::init().expect("failed to install Selium guest runtime");
    let result = ::selium_guest::run_entrypoint_with_result(async move {
        discovery_probe(handle as u64).await
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            ::selium_guest::error!("{e}");
            1
        }
    }
}
```

For the existing `()` return type, the generated code is unchanged.

## Generated Code Matrix

| Args | Async | Return | Export signature | Wrapper |
|------|-------|--------|-----------------|---------|
| 0 | yes | `()` | `fn()` | spawn + poll_safely (unchanged) |
| 0 | yes | `Result<()>` | `fn() -> i32` | run_entrypoint_with_result + match |
| 0 | no | `()` | `fn()` | call directly (unchanged) |
| 0 | no | `Result<()>` | `fn() -> i32` | call + match (no reactor needed) |
| 1 | yes | `()` | `fn(i64)` | spawn + poll_safely (unchanged) |
| 1 | yes | `Result<()>` | `fn(i64) -> i32` | run_entrypoint_with_result + match |
| 1 (Context) | yes | `Result<()>` | `fn(i64) -> i32` | Context::from_raw + run_entrypoint_with_result + match |
| 2 | yes | `Result<()>` | `fn(i64, i64) -> i32` | run_entrypoint_with_result + match |

And equivalent for sync variants of each.

## Risks / Trade-offs

- **`JoinHandle::take_result` panics if the task hasn't completed** — the reactor polls until all tasks are done, so the result must be present. If this invariant breaks, the panic message should be clear enough to debug.
- **`std::error::Error` trait bound** — if a guest wants to use a custom error type that doesn't implement `Error`, they'll get a compile error. This is intentional; `Error` is standard practice.
- **Host-side `EntrypointFailed`** — this changes bootstrap behavior for any guest that starts returning `Result<()>`. If a guest previously logged an error and `return;`-ed (silent from host perspective), it will now cause a bootstrap failure. This is the intended behavior.
