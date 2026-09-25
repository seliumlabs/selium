## 1. ABI: Add Sleep Hostcall Variant

- [x] 1.1 Add `Sleep { millis: u64 }` variant to `HostcallRequest` enum in `crates/core/abi/src/lib.rs`
- [x] 1.2 Verify ABI round-trip tests pass (rkyv encode/decode for the new variant)

## 2. Runtime: Implement Sleep Dispatch and Poll

- [x] 2.1 Add `SleepWait { deadline: Instant }` variant to `HostOperationState` in `crates/core/runtime/src/state.rs`
- [x] 2.2 Add match arm for `HostcallRequest::Sleep` in `dispatch_hostcall` (compute deadline, return `HostOperationState::SleepWait`)
- [x] 2.3 Add match arm for `HostOperationState::SleepWait` in `poll_hostcall` (check `Instant::now() >= deadline`, return `Ready(Empty)` or `Pending`)
- [x] 2.4 Add `SleepWait` to the status determination in `begin_hostcall` (return `HOSTCALL_STATUS_PENDING`)
- [x] 2.5 Run runtime tests to verify no regressions

## 3. Guest: Rewrite Timer with Sleep Hostcall

- [x] 3.1 Remove `signal_id` field and `create_signal`/`close_signal` methods from `Timer` in `crates/core/guest/src/time.rs`
- [x] 3.2 Add `sleep_future: Option<HostcallFuture>` field to `Timer` to track in-flight sleep operations
- [x] 3.3 Rewrite `Timer::poll` to use `hostcall_async(HostcallRequest::Sleep { millis })` instead of stubbed signal logic
- [x] 3.4 Simplify `Timer::cancel_wait` to drop the `HostcallFuture` (which cleans up via its `Drop` impl)
- [x] 3.5 Simplify `Timer::Drop` to just call `cancel_wait`
- [x] 3.6 Export `Timer` from `crates/core/guest/src/lib.rs` public API
- [x] 3.7 Run guest crate tests (`cargo test -p selium-guest`) to verify no regressions

## 4. Guest: Fix net/quinn.rs Compilation

- [x] 4.1 Remove `use crate::Signal` import (type no longer exists)
- [x] 4.2 Remove references to `HostcallRequest::SignalWait` and `HostcallOutput::SignalGeneration` in send/recv paths; replace with `Poll::Pending` stubs and TODO comments for the networking follow-up
- [x] 4.3 Remove duplicate `impl quinn::RuntimeInstant for Instant` block (already in `time.rs`)
- [x] 4.4 Remove duplicate `impl quinn::AsyncTimer for Timer` block (already in `time.rs`)
- [x] 4.5 Fix `UdpSocketInner` references — either import from udp module or stub out the sender struct
- [x] 4.6 Verify `cargo check -p selium-guest --features quinn` succeeds

## 5. Verification

- [x] 5.1 Run full workspace build with default features (`cargo build`)
- [x] 5.2 Run full workspace build with quinn feature (`cargo build --features quinn`)
- [x] 5.3 Run all tests (`cargo test`)
- [x] 5.4 Manually verify `Timer` exports appear in rustdoc (`cargo doc -p selium-guest --no-deps`)
