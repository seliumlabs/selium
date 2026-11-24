//! Failure injection: cross-process wake races.
//!
//! Two watchers attach to the same channel concurrently and are woken by the
//! same publisher stream; a watcher that attaches after the first wake also
//! converges. Wake delivery must reach *every* registered waiter, not just the
//! first, and it must not depend on the host polling either watcher.
//!
//! `#[ignore]`d by default because it needs the wake-probe guest built for
//! `wasm32-unknown-unknown`.

use failure_common as fx;
use selium_runtime::Runtime;

mod common;
mod failure_common;

#[test]
#[ignore = "requires the wake-probe guest built for wasm32-unknown-unknown"]
fn concurrent_watchers_are_all_woken_by_one_publisher_stream() {
    let runtime = Runtime::default();

    let publisher = fx::spawn_wake_probe(&runtime, "race-publisher", fx::MODE_WAKE_PUBLISHER, "");
    let region = wait_for_channel_region(&runtime, publisher);

    // Both watchers attach before either has been woken: the race is two
    // waiters registered on the same generation word.
    let watcher_a = fx::spawn_wake_probe(
        &runtime,
        "race-watcher-a",
        fx::MODE_WAKE_WATCHER,
        &region.to_string(),
    );
    let watcher_b = fx::spawn_wake_probe(
        &runtime,
        "race-watcher-b",
        fx::MODE_WAKE_WATCHER,
        &region.to_string(),
    );

    fx::wait_for_log(
        &runtime,
        watcher_a,
        "wake-probe: woke",
        fx::CONVERGENCE_BOUND,
    );
    fx::wait_for_log(
        &runtime,
        watcher_b,
        "wake-probe: woke",
        fx::CONVERGENCE_BOUND,
    );
    // A watcher attaching after the first wake still converges: the publisher
    // keeps publishing, and its reader starts at the live tail.
    let late = fx::spawn_wake_probe(
        &runtime,
        "race-watcher-late",
        fx::MODE_WAKE_WATCHER,
        &region.to_string(),
    );
    fx::wait_for_log(&runtime, late, "wake-probe: woke", fx::CONVERGENCE_BOUND);

    // The publisher stayed resident and progressing throughout. Each watcher
    // returns after its wake and is reaped; the publisher is stopped last.
    assert!(runtime.is_guest_resident(publisher));
    // The watchers may still be resident (a woken watcher is not re-polled
    // unless something wakes it again); stop each best-effort.
    for watcher in [watcher_a, watcher_b, late] {
        fx::stop_if_resident(&runtime, watcher);
    }
    runtime.stop_process(publisher).expect("stop publisher");
}

/// Waits for the publisher to announce its channel and returns the region id.
fn wait_for_channel_region(runtime: &Runtime, publisher: u64) -> u64 {
    let logs = fx::wait_for_log(
        runtime,
        publisher,
        "wake-probe: channel",
        fx::CONVERGENCE_BOUND,
    );
    logs.iter()
        .find_map(|message| {
            message
                .strip_prefix("wake-probe: channel ")
                .and_then(|rest| rest.trim().parse::<u64>().ok())
        })
        .expect("publisher announced its channel region")
}
