# Tasks: Shared-Page Wait/Wake Fast Path

## 1. Guest-side notify emission (the driver)

- [x] 1.1 `selium-shm`: emit `atomic_notify` on the ring generation word after every generation bump, feature-gated on `nightly-wasm-atomics` (stable builds keep the honest no-op). Wake count is 1: exactly one host drainer parks on the generation word, and a genuine notify with a huge count would spin the engine's registry loop under its read lock.
- [x] 1.2 Guest module shape: declare memory 0 shared with a maximum when the atomics feature is enabled (wasmtiny validator requirement); verify the entrypoint/macro path produces conformant modules
- [x] 1.3 Toolchain wiring: nightly + `stdarch_wasm_atomic_wait` + `+atomics` target feature for guest builds opting into the fast path

## 2. wasmtiny contract + integration

- [x] 2.1 Pin the engine contract: public host-facing wait/notify on shared region offsets (`register_region_waiter`/`notify_region` + `RegionWaiter` handle) — engine implementation tracked in the wasmtiny repo
- [x] 2.2 Support advertisement: engine/build capability flag (`HostWaitSupport`) consumable at region attach
- [x] 2.3 (Stage 2 only) Engine notify additionally emits the platform wake on the region's host mapping address. Engine-side guard added: `shared_notify` wakes the registered waiter once regardless of the guest's count argument (a `u32::MAX` count previously looped under the read lock).

## 3. Stage 1 — unified wait registry

- [x] 3.1 `selium-memory` host backend: delegate `atomic_wait32`/`atomic_notify` to the wasmtiny region waiter registry for engine-backed regions (keep the private registry for heap/test providers)
- [x] 3.2 Network proxies wait via the unified registry
- [x] 3.3 Runtime suppresses transition kicks for regions with the fast path active; keep kicking all others. Eligibility is per attachment and requires ALL attachers capable; votes are removed on release/destroy.
- [x] 3.4 Keep the bounded-timeout backstop from `event-driven-net-proxies` in place

## 4. Stage 2 — optional per-OS wait-words

- [x] 4.1 Host backend `wait_word`/`wake_word` implementations: Linux futex, macOS `__ulock_*`, Windows `WaitOnAddress`, FreeBSD `_umtx_op`, behind `cfg(target_os)` (macOS `available()` is permanently false — Darwin rejects the wait-word syscalls outright for ordinary binaries; constants audited against XNU's `ulock.h`)
- [x] 4.2 Notify/wait race conformance test (many iterations, guest notify wakes host waiter) gating per-platform opt-in: `stage2_notify_wait_race_conformance` in `selium-kernel` (feature-gated) + engine-side `guest_notify_wakes_host_os_waiter_conformance`; CI runs both on Linux
- [x] 4.3 Enablement path: `stage2-wait-words` feature chain (`selium-runtime` → `selium-kernel` → wasmtiny `platform-wake-emission`), detected via `HostWaitSupport::RegistryAndOsWake`, off by default. The Linux CI job is the standing enablement evidence; Windows/FreeBSD enablement awaits the same conformance evidence there, macOS never (permanent Stage 1 fallback).
- [x] 4.4 (follow-up found during integration) `selium-shm`'s notify wake count corrected from `u32::MAX` to 1, and wasmtiny's `shared_notify` no longer iterates the count — see design.md

## 5. Verification

- [x] 5.1 Guest write → host waiter wakes with no runtime kick (Stage 1), asserted in CI: `fastpath_wake` deploys the real atomics net-demo guest, asserts fast-path detection, prompt drain via the guest's `memory.atomic.notify`, and zero transition kicks delivered to fast-path regions after detection (startup kicks before attach complete are excluded — the region is not yet kick-suppressible)
- [x] 5.2 Stable-build guests (no atomics feature) still work via the kick path, unchanged (`net_wake` integration test; the module probe classifies their modules as non-capable)
- [x] 5.3 No-spin regression: idle proxies consume no polling CPU on both stages
- [x] 5.4 Latency comparison benchmark: kick path vs Stage 1 vs Stage 2 (documented, non-gating) — `latency_reports` in `selium-kernel`'s `wait_notify` tests share one notify→wake harness for the three-way comparison
