//! Multithreaded demo guest for the guest-worker-pool runtime tests.
//!
//! The entrypoint spawns two CPU-bound tasks and joins them. Under the
//! runtime's multithreaded execution (a dedicated worker pool over the
//! guest's shared linear memory) the tasks run concurrently on distinct
//! workers, so a two-worker run completes in less wall-clock time than the
//! one-worker (serial) run — the end-to-end proof of task 6.2.
//!
//! `mode` selects the process lifetime: `0` exits once the tasks complete
//! (6.2); `1` parks a resident task afterwards so the workers stay parked
//! and the host can exercise wake delivery and stop the process (4.2).
//!
//! Built with the atomics target (`+atomics` + `--shared-memory` + the
//! `nightly-wasm-atomics` feature) and, at link time,
//! `--export=__stack_pointer`. That export is what lets the engine give each
//! concurrent invocation its own shadow stack: a rustc-compiled module keeps
//! every frame on the `__stack_pointer` shadow stack, so without a distinct
//! stack per worker two workers would overlap frames and corrupt each other.
//! `scripts/build-all.sh` produces the atomics artifact
//! (`selium_mt_demo_atomics.wasm`) that the `#[ignore]`d end-to-end test reads.

use selium_guest::{entrypoint, spawn};

/// Iterations per CPU-bound task: long enough to be measured in wall time
/// (two tasks must complete faster on two workers than one), short enough to
/// keep the test fast.
const ITERATIONS: u64 = 1 << 26;

/// A CPU-bound task: `iterations` updates of a task-local accumulator. The
/// loop-carried dependency (a multiply-add chain) cannot be folded into a
/// closed form, and the inputs are `black_box`ed so the optimizer cannot treat
/// the loop as constant — so the work is real and measurable.
async fn cpu_bound(iterations: u64) {
    let mut acc: u64 = std::hint::black_box(0);
    let mut remaining: u64 = std::hint::black_box(iterations);
    while remaining > 0 {
        acc = acc
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        remaining -= 1;
    }
    std::hint::black_box(acc);
}

#[entrypoint]
async fn mt_demo(mode: u64) -> anyhow::Result<()> {
    let first = spawn(cpu_bound(ITERATIONS));
    let second = spawn(cpu_bound(ITERATIONS));
    first.await;
    second.await;
    if mode == 1 {
        // Resident mode: park forever so the guest's workers stay parked;
        // the test delivers wakes and then stops the process.
        core::future::pending::<()>().await;
    }
    Ok(())
}
