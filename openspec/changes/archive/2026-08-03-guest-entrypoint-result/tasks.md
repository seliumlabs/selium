## 1. Guest async_runtime: JoinHandle result access

- [x] 1.1 Add `pub(crate) fn take_result(&self) -> Option<T>` to `JoinHandle<T>` that drains `self.state.borrow_mut().result.take()`
- [x] 1.2 Add `pub fn run_entrypoint_with_result<F, T>(future: F) -> T` — identical to `run_entrypoint_safely` but spawns the future and returns the output via `take_result()` after the reactor parks; aborts on panic
- [x] 1.3 Re-export `run_entrypoint_with_result` from `selium_guest::lib.rs`

## 2. Guest macros: Result<()> detection and i32 codegen

- [x] 2.1 Replace the `ReturnType::Default` check with logic that accepts both `()` (unchanged) and `Result<()>` (new path)
- [x] 2.2 Detect the `Result<()>` return type by inspecting `function.sig.output` for `Type::Path` with `Result` and a single `()` generic argument
- [x] 2.3 For `Result<()>` entrypoints, generate `extern "C" fn … -> i32` with the result match: `Ok(()) => 0`, `Err(e) => { error!("{e}"); 1 }`
- [x] 2.4 Add a static assertion or trait-bound helper so that `E: std::error::Error` is enforced with a clear compile error
- [x] 2.5 Apply the `Result<()>` codegen pattern across all parameter count variants (0, 1, 1-with-Context, 2) and both sync/async
- [x] 2.6 Reject any return type that is neither `()` nor `Result<()>` with a clear compile error message

## 3. Runtime: fail fast on non-zero exit code

- [x] 3.1 Add `EntrypointFailed(String)` variant to `crate::Error` in `selium-runtime`
- [x] 3.2 After `execute_entrypoint` captures results, check for `[I32(1)]` and return `Err(Error::EntrypointFailed(descriptor.name.clone()))`
- [x] 3.3 Ensure the `spawn_system_guest` cleanup path handles the new error variant correctly (it already records `ProcessExited` and calls `cleanup_failed_process` on any error)

## 4. Update discovery-probe guest

- [x] 4.1 Remove manual `match` + `return;` error handling from `discovery_probe`
- [x] 4.2 Replace with `?` propagation on `Context::from_raw` and `Channel::create`
- [x] 4.3 Keep `Ok(())` at end of function

## 5. Tests

- [x] 5.1 Add macro test: entrypoint with `Result<()>` return generates `-> i32` extern fn
- [x] 5.2 Add compile-fail test: entrypoint with `Result<(), NotAnError>` produces clear error
- [x] 5.3 Add runtime integration test: guest that returns `Err` causes `EntrypointFailed`
- [x] 5.4 Add runtime integration test: guest that returns `Ok(())` bootstraps normally
- [x] 5.5 Existing entrypoint tests continue to pass (void return type unchanged)
