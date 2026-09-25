# Tasks: Channel Wake/Wait

## 1. Futex primitives

- [x] 1.1 Audit wasmtiny `memory.atomic.wait32`/`notify` support; fix or complete as needed (guests on `PointerBackend` depend on it).
- [x] 1.2 Implement `PointerBackend::atomic_wait32`/`atomic_notify` (wasm32: engine instructions; native: waiters table). Delete the `Ok(0)`/`Ok(())` stubs.
- [x] 1.3 Implement host-side wait/notify for `KernelBackend` and `HeapRegionProvider` (shared waiters registry keyed by region+offset).
- [x] 1.4 Unit tests: concurrent wait/notify between two mappings of one region; spurious-wake tolerance; timeout paths.

## 2. Channel wake registration

- [x] 2.1 Add a reactor wait map in `selium-guest` (TaskId keyed by region_id + observed generation) with wake-on-bump.
- [x] 2.2 `Reader`/`BlockingReader::poll_read` register instead of returning unwakeable Pending; writer-count-zero disconnect bumps generation.
- [x] 2.3 `Writer`/`BlockingWriter::poll_write` register on BufferFull (Park) instead of Pending-spin; readers bump generation on slot advance.
- [x] 2.4 `bump_generation` calls `atomic_notify` on all backends.

## 3. Spin removal

- [x] 3.1 `wire::pubsub::Subscriber::poll_next`, `wire::rpc::{RpcClient::request, RpcConnection::recv}`: convert to generation waits.
- [x] 3.2 Guest UDP `Stream` impl and bridge relay loops: convert to generation waits (or delete with the frozen crates' replacement).
- [x] 3.3 Discovery `feed_loop` and any guest spin loops: convert; document the wait idiom in `selium-guest` docs.

## 4. Sleep wakeups

- [x] 4.1 Runtime timer wheel for `SleepWait` (single min-heap + one driver); mailbox wake at deadline.
- [x] 4.2 `Timer` integration test inside a WASM guest: sleep 50ms completes and measures >= deadline.

## 5. Stress and gates

- [x] 5.1 Stress: N writers × M readers on one channel with slot churn; assert no lost wakeups and no reactor hot-spin (measure reactor stall exits).
- [x] 5.2 Gates: fmt, clippy `-D warnings`, full suite, wasm32 builds, spine + discovery tests green.
