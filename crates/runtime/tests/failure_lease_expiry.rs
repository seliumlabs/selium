//! Failure injection: lease expiry under load.
//!
//! A registration whose owner stops renewing must lapse when its lease TTL
//! passes, even while the registry is serving other registrations. The guests
//! register through the raw discovery client (no SDK renewal loop) and the
//! runtime's test clock is advanced past the TTL, so the real sweep expires
//! them without an hour-long wait.
//!
//! `#[ignore]`d by default because it needs the discovery and discovery-probe
//! guests built for `wasm32-unknown-unknown`.

use std::time::Duration;

use failure_common as fx;
use selium_runtime::Runtime;
use selium_service::REGISTRATION_LEASE_TTL_MS;

mod common;
mod failure_common;

/// Registrations held open (without renewal) while the sweep runs: the
/// registry is under load, not idle, when the leases lapse.
const LAPSED_LEASES: usize = 3;
/// Safety margin past the TTL so the next sweep tick certainly sees the lapse.
const TTL_MARGIN_MS: u64 = 5_000;

// scenario: guest-context/discovery-client-in-context/guest-resolves-a-uri
#[test]
#[ignore = "requires the discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn a_lapsed_lease_is_expired_under_load() {
    let runtime = Runtime::default();
    let discovery = fx::bootstrap_discovery(&runtime);

    let mut holders = Vec::with_capacity(LAPSED_LEASES);
    for index in 0..LAPSED_LEASES {
        let uri = format!("sel://probe/app/lapsed-{index}");
        let holder = fx::spawn_probe(
            &runtime,
            &format!("lease-holder-{index}"),
            fx::ROLE_REGISTER_NO_RENEW,
            &uri,
            Vec::new(),
        );
        fx::wait_for_log(
            &runtime,
            holder,
            &format!("lease: registered {uri}"),
            fx::CONVERGENCE_BOUND,
        );
        holders.push((holder, uri));
    }

    // The holders are alive but never renew: expiry is the sweep's decision,
    // not owner-exit revocation.
    for (holder, uri) in &holders {
        assert!(
            runtime.is_guest_resident(*holder),
            "the no-renew holder for {uri} must stay resident until its lease lapses"
        );
    }

    // Age the leases past their TTL. The sweep ticks on host time, so its next
    // tick (<= 2 s) observes the lapsed clock.
    runtime.advance_guest_clock(Duration::from_millis(
        REGISTRATION_LEASE_TTL_MS + TTL_MARGIN_MS,
    ));

    for (index, (_, uri)) in holders.iter().enumerate() {
        // The resolver retries: a single resolve races the sweep, and the
        // sweep may tick either side of it.
        let resolver = fx::spawn_probe(
            &runtime,
            &format!("lease-resolver-{index}"),
            fx::ROLE_RESOLVER_UNTIL_GONE,
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
            "the lapsed lease {uri} must be expired, got: {logs:?}"
        );
        fx::wait_for_exit(&runtime, resolver, fx::EXIT_BOUND);
    }

    runtime.stop_process(discovery).expect("stop discovery");
}
