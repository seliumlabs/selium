## Context

The signal hostcalls (`SignalWait`, `SignalCreate`, `SignalClose`) were removed from the ABI in the "dumb host, smart guest" migration (commit `cb01b79`). The replacement substrate — shared memory regions with WASM atomics (`memory.atomic.wait32`/`notify`) — handles channel coordination but provides no "sleep until deadline" primitive. Without signals, the `Timer` type in `selium-guest` was gutted to a stub that returns `Poll::Pending` forever, breaking the Quinn transport integration which depends on `quinn::AsyncTimer`.

The runtime already has the pattern for async operations that complete based on `Instant` deadlines (see `HostQueueRecvWait`). Extending this pattern to a simple sleep is the minimal change that restores functionality.

## Goals / Non-Goals

**Goals:**
- Provide a working `Timer` that implements `quinn::AsyncTimer` for Quinn's timeout management
- Add a minimal `Sleep` hostcall to the ABI as the timer wakeup mechanism
- Keep the host "dumb": the sleep implementation is a straightforward deadline check
- Fix `net/quinn.rs` so it compiles with the `quinn` feature enabled
- Consolidate duplicate `RuntimeInstant` and `AsyncTimer` impls into `time.rs`

**Non-Goals:**
- Re-add signals or any general-purpose signal mechanism
- Fix UDP socket send/recv (`quinn.rs` UDP paths remain stubbed — separate follow-up)
- Implement a full async timer/alarm subsystem in the host
- Change the Quinn version or Quinn integration architecture

## Decisions

### Decision 1: `HostcallRequest::Sleep { millis: u64 }` with `HostcallOutput::Empty`

**Rationale**: The simplest possible timer hostcall. The guest computes the remaining duration and fires a sleep. The host returns `Empty` when the time elapses. This is intentionally minimal — no deadline parameter on the hostcall itself because the guest can compute `millis` from its own deadline and the host's monotonic clock.

**Alternatives considered**:
- `SleepUntil { deadline_nanos: u64 }`: The host would need to interpret the guest's monotonic clock, which may have a different epoch. Computing the duration guest-side avoids clock synchronization issues.
- Timer shared region per guest with `memory.atomic.wait32`: More complex on both sides; requires host-side timer polling loop per guest; no advantage over a simple hostcall.
- Re-add `SignalWait` with timeout: Adds unnecessary ABI surface (create/close/wait lifecycle) when a single fire-and-forget sleep suffices.

### Decision 2: `HostOperationState::SleepWait { deadline: Instant }` in the runtime

**Rationale**: Follows the existing `HostQueueRecvWait` pattern exactly. The operation is stored in `Runtime::operations` with a deadline. `poll_hostcall` checks `Instant::now() >= deadline` and transitions to `Ready(HostcallOutput::Empty)` when elapsed. No background threads needed — the guest polls the operation via the existing reactor loop, which naturally handles the timing.

**Alternatives considered**:
- Background thread that sleeps and then writes to guest mailbox: Over-engineered for a simple deadline check. The existing poll-based pattern works because the guest reactor continuously re-polls pending operations.
- Return `Ready` immediately and let the guest spin: Wastes CPU in the guest's cooperative scheduler.

### Decision 3: `Timer` uses `HostcallFuture` directly, no signal machinery

**Rationale**: The old `Timer` had `signal_id`, `operation_id`, `create_signal()`, `close_signal()`, and complex `cancel_wait()` logic — all of which is now dead. The new `Timer` just stores a `HostcallFuture` for the in-flight sleep. When the future completes, the timer is ready. When the `Timer` is dropped or `reset()`, the future's `Drop` impl cleans up the operation.

This reduces `Timer` to ~30 lines of straightforward code.

## Risks / Trade-offs

- **Poll-based wakeup granularity**: The timer resolution depends on how frequently the guest reactor polls. With the current `DEFAULT_READINESS_POLL_MS` of 10ms in the bootstrap, worst-case timer latency is ~10ms. This is acceptable for Quinn's timeout management (typical QUIC timers are on the order of 25ms–500ms). **Mitigation**: If finer granularity is needed later, the readiness poll interval can be reduced or a mailbox-based wakeup can be added.
- **No timeout on `Sleep` itself**: The sleep hostcall always succeeds after the specified duration. If the guest wants a timeout-bounded sleep, it can race the sleep against another operation. **Mitigation**: This matches the old `SignalWait` behavior where the timeout was the primary mechanism, not a fallback.
