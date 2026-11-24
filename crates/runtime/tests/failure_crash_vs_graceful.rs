//! Failure injection: crash versus graceful exit.
//!
//! Both exits must revoke the process's registration, but they travel different
//! paths: a graceful `stop_process` publishes the revocation directly, while a
//! crashed guest is reaped through the failure path. A registration that
//! survives either is a leak discovery would never learn to drop.
//!
//! `#[ignore]`d by default because it needs the discovery and discovery-probe
//! guests built for `wasm32-unknown-unknown`.

use failure_common as fx;
use selium_runtime::Runtime;

mod common;
mod failure_common;

// scenario: verification-gate/pack-exclusions-are-explicit-and-narrow/crash-versus-graceful-failure-test-runs-in-the-pack
#[test]
#[ignore = "requires the discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn crash_and_graceful_exit_both_revoke_the_registration() {
    let runtime = Runtime::default();
    let discovery = fx::bootstrap_discovery(&runtime);
    let mut feed = fx::attach_feed(&runtime);
    // Sessions are released when their client departs; the baseline is the
    // discovery guest's own regions.
    let baseline_regions = runtime.kernel().memory().live_shared_region_count();

    // Graceful: the provider parks; stopping it revokes its route.
    let graceful_uri = "sel://probe/app/graceful-exit";
    let graceful = fx::spawn_probe(
        &runtime,
        "graceful-provider",
        fx::ROLE_PROVIDER,
        graceful_uri,
        Vec::new(),
    );
    fx::wait_for_log(
        &runtime,
        graceful,
        &format!("provider: registered {graceful_uri}"),
        fx::CONVERGENCE_BOUND,
    );
    runtime
        .stop_process(graceful)
        .expect("graceful stop must succeed");
    let events = fx::wait_for_revocation(
        &runtime,
        &mut feed,
        graceful,
        Some(graceful_uri),
        fx::REVOCATION_BOUND,
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            selium_service::DiscoveryRequest::ProcessGone { process_id } if *process_id == graceful
        )),
        "a graceful stop must publish the owner-keyed revocation, got: {events:?}"
    );

    // Crash: the guest registers and then traps. Its revocation must travel
    // the failure path instead, and the runtime must stay healthy.
    let crash_uri = "sel://probe/app/crash-exit";
    let crashed = fx::spawn_probe(
        &runtime,
        "crashing-provider",
        fx::ROLE_TRAP_AFTER_REGISTER,
        crash_uri,
        Vec::new(),
    );
    fx::wait_for_log(
        &runtime,
        crashed,
        &format!("crash: registered {crash_uri}"),
        fx::CONVERGENCE_BOUND,
    );
    fx::wait_for_exit(&runtime, crashed, fx::EXIT_BOUND);
    let events = fx::wait_for_revocation(
        &runtime,
        &mut feed,
        crashed,
        Some(crash_uri),
        fx::REVOCATION_BOUND,
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            selium_service::DiscoveryRequest::ProcessGone { process_id } if *process_id == crashed
        )),
        "a crash must also publish the owner-keyed revocation, got: {events:?}"
    );

    // Both departed clients' connection regions are released too: a crashed
    // client cannot withdraw its session, so the host marks it departed and
    // the handler frees it.
    let deadline = std::time::Instant::now() + fx::CONVERGENCE_BOUND;
    while runtime.kernel().memory().live_shared_region_count() != baseline_regions
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        runtime.kernel().memory().live_shared_region_count(),
        baseline_regions,
        "a departed client's connection region must be released"
    );

    // A one-shot guest can finish before its waiter observes the log channel:
    // the reap drains the remaining frames into the `ProcessExited` activity
    // record. The milestone must still be observable afterwards (regression:
    // `power_cycle`'s quota leg completed before the test reached it and its
    // line was lost with the channel, reported as "never completed").
    let one_shot = fx::spawn_probe(
        &runtime,
        "one-shot-resolver",
        fx::ROLE_RESOLVER,
        "sel://probe/app/one-shot-never-registered",
        Vec::new(),
    );
    fx::wait_for_exit(&runtime, one_shot, fx::EXIT_BOUND);
    let logs = fx::wait_for_log(
        &runtime,
        one_shot,
        "resolver: notfound",
        fx::CONVERGENCE_BOUND,
    );
    assert!(
        logs.iter().any(|line| line.contains("resolver: notfound")),
        "a one-shot guest's last line must remain observable after it exits"
    );

    // The platform converged: discovery is still serving and neither guest is
    // resident.
    assert!(runtime.is_guest_resident(discovery));
    assert!(!runtime.is_guest_resident(crashed));
    assert!(!runtime.is_guest_resident(graceful));

    runtime.stop_process(discovery).expect("stop discovery");
}
