//! Failure injection: discovery unavailable to a connecting guest.
//!
//! A guest whose discovery rendezvous cannot be made must fail or park within
//! a bounded time; it must not spin or hang the runtime. The probe is
//! bootstrapped with a zero `Context` handle and no discovery service running,
//! which is exactly what a client sees when the registry is down.
//!
//! `#[ignore]`d by default because it needs the discovery-probe guest built for
//! `wasm32-unknown-unknown`.

use std::time::Instant;

use failure_common as fx;
use selium_runtime::Runtime;

mod common;
mod failure_common;

// scenario: verification-gate/failure-injection-coverage/discovery-outage-does-not-hang-a-client
#[test]
#[ignore = "requires the discovery-probe guest built for wasm32-unknown-unknown"]
fn discovery_unavailable_does_not_hang_a_connecting_guest() {
    let runtime = Runtime::default();
    let uri = "sel://probe/app/no-discovery";

    let started = Instant::now();
    let outcome = fx::bootstrap_probe_without_discovery(
        &runtime,
        "no-discovery-probe",
        fx::ROLE_RESOLVER,
        uri,
    );
    match outcome {
        Ok(report) => {
            let probe = report
                .guests
                .first()
                .expect("probe in bootstrap report")
                .process_id;
            fx::wait_for_exit(&runtime, probe, fx::EXIT_BOUND);
        }
        Err(error) => {
            // A failed bootstrap is the bounded error: it returned instead of
            // parking forever.
            assert!(
                !error.to_string().is_empty(),
                "a failed discovery rendezvous must surface an error"
            );
        }
    }
    assert!(
        started.elapsed() < fx::EXIT_BOUND,
        "discovery being unavailable must settle within {:?}, took {:?}",
        fx::EXIT_BOUND,
        started.elapsed()
    );

    // The runtime is still responsive: an unrelated guest can still be spawned
    // and driven to completion.
    assert_eq!(
        runtime.loaded_guest_count(),
        0,
        "the failed probe must not stay resident"
    );
    let publisher = fx::spawn_wake_probe(
        &runtime,
        "post-outage-publisher",
        fx::MODE_WAKE_PUBLISHER,
        "",
    );
    fx::wait_for_log(
        &runtime,
        publisher,
        "wake-probe: channel",
        fx::CONVERGENCE_BOUND,
    );
    runtime.stop_process(publisher).expect("stop publisher");
}
