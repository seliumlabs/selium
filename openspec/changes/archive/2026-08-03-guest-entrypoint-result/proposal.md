# Proposal: Guest Entrypoint `Result<()>` Support

## Why

Guest entrypoints currently must return `()` — the `#[entrypoint]` macro rejects any other return type. This forces guests into manual error handling with `match` + `return;` patterns and prevents use of the `?` operator for clean error propagation.

The `discovery-probe` guest illustrates the pain:

```rust
#[entrypoint]
async fn discovery_probe(discovery_handle: u64) -> Result<()> {
    let _ctx = match Context::from_raw(discovery_handle).await {
        Ok(ctx) => ctx,
        Err(error) => {
            selium_guest::error!("failed to create discovery context: {error}");
            return;  // silently swallows the error from the host's perspective
        }
    };
    match Channel::create(PROBE_CHANNEL_CAPACITY, ChannelBackpressure::Park) {
        Ok(channel) => { /* ... */ }
        Err(error) => {
            selium_guest::error!("probe: channel create failed: {error}");
            return;
        }
    }
    selium_guest::mark_ready();
    Ok(())  // unreachable on error paths above
}
```

With `Result<()>` and `?`, this collapses to ~10 lines with proper error visibility.

## What Changes

1. **`selium-guest-macros`**: Accept `Result<()>` as an entrypoint return type. Generate `extern "C" fn … -> i32` exports that return `0` on `Ok(())` and `1` on `Err(e)`, logging the error via `selium_guest::error!` before returning.

2. **`selium-guest`**: Add `run_entrypoint_with_result` that mirrors `run_entrypoint_safely` but returns the future's output instead of `()`. Expose a `take_result` accessor on `JoinHandle` so the generated macro code can extract the value after the reactor parks.

3. **`selium-runtime`**: After `execute_entrypoint`, check the captured `Vec<WasmValue>`. If it's `[I32(1)]`, fail fast with a new `Error::EntrypointFailed` variant before reaching the readiness check.

Existing `()`-returning entrypoints are unchanged.

## Impact

- **Backward compatible**: All existing entrypoints continue to work as before.
- **Error type constraint**: `E` in `Result<(), E>` must implement `std::error::Error` (providing `Display` for log messages).
- **Host-side behavior**: Non-zero exit codes cause immediate bootstrap failure with the guest's error already in the activity log.

## Non-goals

- `Result<T, E>` for `T != ()` (deferred until there's a concrete use case)
- Structured error propagation via rkyv-encoded hostcalls (deferred)
- Changing panic behavior — panics still abort the guest process
