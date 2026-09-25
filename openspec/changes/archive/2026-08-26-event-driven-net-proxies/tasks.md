# Tasks: Event-Driven Network Proxies

## 1. WaitRegister bridge

- [x] 1.1 Add `HostcallRequest::WaitRegister { region_id, generation }` to the ABI (rkyv round-trip test)
- [x] 1.2 Runtime wait registry: `(process, task, region, generation)` entries; `note_generation_advance(region, gen)` matches and calls `wake_process_task`
- [x] 1.3 Runtime rejects `WaitRegister` for regions the process has not attached (`PermissionDenied`)
- [x] 1.4 Guest reactor: when parking a task on a host-writable ring, issue `WaitRegister` with the parked `task_id`

## 2. Kernel inbound poller

- [x] 2.1 Add mio dependency to `selium-kernel`; single poller thread owning `mio::Poll`
- [x] 2.2 Register proxy sockets (TCP stream inbound, TCP accept, UDP recv) by `shared_id` token
- [x] 2.3 Readable event → pump available bytes/datagrams to the inbound ring → `note_generation_advance` → guest wake bridge
- [x] 2.4 Accept loop moves into the poller (listener readable → accept → create stream region → enqueue host queue)

## 3. Kernel outbound condvar waits

- [x] 3.1 Outbound TCP pump and UDP send pump block on `atomic_wait32` (host condvar registry) on the ring generation word, with a bounded timeout backstop
- [x] 3.2 Runtime kicks (`waiters::notify`) the guest's network regions on hostcall create, hostcall poll, and reactor stall
- [x] 3.3 Delete all `thread::sleep` retry loops and `PROXY_POLL_INTERVAL_MS` from network proxy paths

## 4. Verification

- [x] 4.1 Test: guest reader parked on inbound ring wakes on socket data without any sleep-based polling (assert no 1 ms poll constants remain; assert wake latency < threshold)
- [x] 4.2 Test: guest write followed by stall is drained to the socket without waiting for the backstop timeout
- [x] 4.3 Test: EOF/half-close propagation still behaves per `guest-networking` scenarios
- [x] 4.4 Test: spin detection — proxy threads consume ~0 CPU while idle (best-effort CI assertion or documented manual check)

> 4.1/4.2 are covered end-to-end by the ignored integration test
> `runtime/tests/net_wake.rs` against a real WASM guest
> (`crates/guests/net-demo`); build with
> `cargo build --target wasm32-unknown-unknown -p selium-net-demo` and run
> `cargo test -p selium-runtime --test net_wake -- --ignored`.
