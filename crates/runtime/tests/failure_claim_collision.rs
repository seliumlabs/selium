//! Failure injection: claim collision and forgery.
//!
//! A reserved handler route is claim-gated. Two guests holding the host's
//! handler claim may not both own the name — the second registration is a
//! conflict — and a guest without the claim is refused outright. Neither may
//! take the name from its first holder.
//!
//! `#[ignore]`d by default because it needs the discovery and discovery-probe
//! guests built for `wasm32-unknown-unknown`.

use failure_common as fx;
use selium_runtime::Runtime;

mod common;
mod failure_common;

/// The handler scheme the claimants declare; the host provisions a `RouteClaim`
/// for `sel:///handler/<scheme>`.
const SCHEME: &str = "sel-fixture";

// scenario: selium-runtime/protocol-handler-route-claims/guests-cannot-forge-a-handler
// scenario: route-claims/host-issued-route-claims/process-without-a-claim-is-refused
// scenario: guest-context/context-register-convenience-method/registration-rejected-forbidden
// scenario: verification-gate/failure-injection-coverage/a-forged-claim-is-refused
#[test]
#[ignore = "requires the discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn a_forged_or_second_claim_is_refused() {
    let runtime = Runtime::default();
    let discovery = fx::bootstrap_discovery(&runtime);
    let uri = format!("sel:///handler/{SCHEME}");

    // The first claimant holds the host-provisioned claim and registers.
    let first = fx::spawn_root_probe(
        &runtime,
        "claim-first",
        fx::ROLE_CLAIM_ROUTE,
        &uri,
        vec![SCHEME.to_string()],
    );
    fx::wait_for_log(
        &runtime,
        first,
        &format!("claim: registered {uri}"),
        fx::CONVERGENCE_BOUND,
    );

    // A second guest with the *same* claim collides: the name is already owned
    // by a different process.
    let second = fx::spawn_root_probe(
        &runtime,
        "claim-second",
        fx::ROLE_CLAIM_ROUTE,
        &uri,
        vec![SCHEME.to_string()],
    );
    fx::wait_for_log(
        &runtime,
        second,
        &format!("claim: refused {uri} Conflict"),
        fx::CONVERGENCE_BOUND,
    );

    // A guest with no claim at all is refused rather than falling back to a
    // projected record or its own ownership of a resource.
    let forger = fx::spawn_root_probe(&runtime, "claim-forger", fx::ROLE_FORGE, &uri, Vec::new());
    fx::wait_for_log(
        &runtime,
        forger,
        &format!("forge: refused {uri} Forbidden"),
        fx::CONVERGENCE_BOUND,
    );

    // The first claimant still owns the name; the forger, refused, exits.
    assert!(
        runtime.is_guest_resident(first),
        "the first claimant must keep its registration"
    );
    fx::wait_for_exit(&runtime, forger, fx::EXIT_BOUND);

    runtime.stop_process(first).expect("stop first claimant");
    runtime.stop_process(second).expect("stop second claimant");
    runtime.stop_process(discovery).expect("stop discovery");
}
