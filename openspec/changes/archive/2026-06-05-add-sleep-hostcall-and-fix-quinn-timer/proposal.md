## Why

The `Timer` type in `selium-guest` was gutted when signal hostcalls were removed from the ABI (commit `cb01b79`). It can no longer provide async deadline-based wakeups — the `Future::poll` falls through to `Poll::Pending` with no mechanism to ever wake the task. This breaks the Quinn transport integration (`net/quinn.rs`), which depends on `quinn::AsyncTimer`, and blocks any guest code that needs cooperative sleep.

The replacement substrate (shared memory regions + WASM atomics) handles channel coordination but does not address the "sleep until time T" primitive. The simplest fix aligned with the "dumb host, smart guest" architecture is a single-purpose `Sleep` hostcall.

## What Changes

- **Add `HostcallRequest::Sleep { millis: u64 }`** to the ABI — an async hostcall that completes after the specified duration
- **Implement the Sleep handler** in the runtime's `dispatch_hostcall`, using `thread::sleep` on a background thread to complete the pending operation
- **Rewrite `Timer`** in `time.rs` to use the Sleep hostcall instead of the stubbed signal machinery (remove `signal_id` field, store `OperationId` for the in-flight sleep)
- **Fix `net/quinn.rs`** — remove references to removed `Signal` type and `SignalWait`/`SignalGeneration` ABI variants; remove duplicate `RuntimeInstant` and `AsyncTimer` impls that already live in `time.rs`; stub out UDP send/recv signal waits with `Poll::Pending` until the full networking follow-up
- **Export `Timer`** from the guest crate's public API so Quinn integration can use it

## Capabilities

### New Capabilities
- `sleep-hostcall`: A new `HostcallRequest::Sleep { millis: u64 }` variant that provides async timed sleep for guest code, replacing the removed signal-based timer wakeup mechanism.

### Modified Capabilities
- `selium-abi`: New `Sleep` variant on `HostcallRequest`
- `selium-guest`: `Timer` type restored with working async wakeup; `Timer` re-exported publicly; Quinn `RuntimeInstant` and `AsyncTimer` impls consolidated in `time.rs`
- `quinn-transport`: Update spec scenarios that reference `SignalWait` to reference the `Sleep` hostcall or atomic wait instead

## Impact

- **ABI**: One new hostcall variant, backwards-compatible (additive)
- **Runtime**: New `HostOperationState::SleepWait` variant, background thread per sleep operation
- **Guest crate**: `time.rs` rewritten (no more signal stubs), `net/quinn.rs` imports fixed and duplicates removed
- **Kernel**: No changes needed (sleep is purely a runtime concern)
