//! Failure injection: host authority unavailable at admission.
//!
//! Discovery must deny a registration when the host's authoritative resource
//! query cannot answer, rather than falling back to its projected store. The
//! runtime seam (`Runtime::inject_authority_failure`) arms exactly that
//! failure; the rest of the path is the real guest.
//!
//! `#[ignore]`d by default because it needs the discovery and discovery-probe
//! guests built for `wasm32-unknown-unknown`.

use std::time::Instant;

use failure_common as fx;
use selium_runtime::Runtime;

mod common;
mod failure_common;

// scenario: verification-gate/failure-injection-coverage/authority-failure-fails-closed
#[test]
#[ignore = "requires the discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn authority_unavailable_at_admission_denies_registration() {
    let runtime = Runtime::default();
    let discovery = fx::bootstrap_discovery(&runtime);
    let uri = "sel://probe/app/authority-unavailable";

    runtime.inject_authority_failure(true);
    let started = Instant::now();
    let outcome = fx::bootstrap_probe(
        &runtime,
        "authority-provider",
        fx::ROLE_PROVIDER,
        uri,
        Vec::new(),
    );
    // Whether the bootstrap rolls back or the guest exits on its next poll, the
    // client resolves the failure promptly; it never parks on an unanswered
    // authority query.
    if let Ok(report) = outcome {
        let provider = report
            .guests
            .first()
            .expect("provider in bootstrap report")
            .process_id;
        fx::wait_for_exit(&runtime, provider, fx::EXIT_BOUND);
    }
    assert!(
        started.elapsed() < fx::EXIT_BOUND,
        "the denied admission must settle within the bound"
    );

    // No projection fallback: with the authority answering again, the route was
    // never registered.
    runtime.inject_authority_failure(false);
    let resolver = fx::spawn_probe(
        &runtime,
        "authority-resolver",
        fx::ROLE_RESOLVER,
        uri,
        Vec::new(),
    );
    let logs = fx::wait_for_log(
        &runtime,
        resolver,
        "resolver: notfound",
        fx::CONVERGENCE_BOUND,
    );
    assert!(
        logs.iter()
            .any(|line| line.contains("notfound") && line.contains(uri)),
        "the denied route must not be resolvable, got: {logs:?}"
    );

    fx::wait_for_exit(&runtime, resolver, fx::EXIT_BOUND);
    runtime.stop_process(discovery).expect("stop discovery");
}
