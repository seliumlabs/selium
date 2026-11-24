//! Failure injection: a revocation publish fails mid-teardown.
//!
//! `stop_process` must report the failure, retain the process authority, and
//! leave the revocations staged so a retry completes them. The runtime seam
//! (`Runtime::inject_discovery_publish_failure`) fails the feed publish; the
//! retry then runs against the real discovery feed.
//!
//! `#[ignore]`d by default because it needs the discovery and discovery-probe
//! guests built for `wasm32-unknown-unknown`.

use failure_common as fx;
use selium_runtime::Runtime;

mod common;
mod failure_common;

// scenario: verification-gate/failure-injection-coverage/partial-teardown-is-retryable
#[test]
#[ignore = "requires the discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn partial_teardown_is_retryable_after_a_publish_failure() {
    let runtime = Runtime::default();
    let discovery = fx::bootstrap_discovery(&runtime);
    let uri = "sel://probe/app/partial-teardown";

    let provider = fx::spawn_probe(
        &runtime,
        "teardown-provider",
        fx::ROLE_PROVIDER,
        uri,
        Vec::new(),
    );
    fx::wait_for_log(
        &runtime,
        provider,
        &format!("provider: registered {uri}"),
        fx::CONVERGENCE_BOUND,
    );
    let mut feed = fx::attach_feed(&runtime);

    // Every publish now fails: teardown cannot revoke the process node.
    runtime.inject_discovery_publish_failure(true);
    let stopped = runtime.stop_process(provider);
    assert!(
        stopped.is_err(),
        "teardown must report failure while publishes fail"
    );

    // The retry completes the staged revocations once publishing works again.
    runtime.inject_discovery_publish_failure(false);
    runtime
        .stop_process(provider)
        .expect("retry stop after the publish failure clears");

    let events = fx::wait_for_revocation(
        &runtime,
        &mut feed,
        provider,
        Some(uri),
        fx::REVOCATION_BOUND,
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            selium_service::DiscoveryRequest::ProcessGone { process_id } if *process_id == provider
        )),
        "the retry must publish the owner-keyed revocation, got: {events:?}"
    );

    runtime.stop_process(discovery).expect("stop discovery");
}
