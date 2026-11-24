//! Control-plane durability interruption tests over real DurableLog/store RPCs.
//!
//! The interrupted component is restarted before its served status is read.
//! Append rejection preserves the acknowledged deployment; append-success/
//! sync-failure returns an error but replays the complete frame actually left
//! in the store, without claiming that the failed sync was acknowledged.

#![allow(clippy::panic, reason = "integration-test assertions")]

use selium_runtime::{Runtime, RuntimeConfig};
use selium_service::{DesiredStateRecord, FlatMsg};
use spine_common::{
    CONVERGENCE_BOUND, StateRoot, control_plane_descriptor, discovery_descriptor,
    operator_descriptor_named, read_log_records, read_wasm, runtime_in_state_root,
    store_descriptor, wait_for_logs,
};

mod common;
mod spine_common;

// scenario: verification-gate/crash-injection-stops-durable-components-mid-write/identity-or-control-plane-stops-during-policy-write
#[test]
#[ignore = "requires discovery, store, control-plane and identity-onboard WASM guests"]
fn store_interruptions_replay_control_plane_state_consistent_with_durable_bytes() {
    let state = StateRoot::new("control-plane-durability");
    let runtime = runtime_in_state_root(&state);
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                store_descriptor(vec!["discovery".to_string()]),
                control_plane_descriptor(read_wasm(
                    "selium-control-plane",
                    "selium_control_plane.wasm",
                )),
            ],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap control-plane durability spine");
    let control_plane = find(&report, "control-plane");

    let initial = deploy(
        &runtime,
        "deploy-initial",
        selium_identity_onboard::MODE_DEPLOY,
    );
    wait_for_logs(
        &runtime,
        initial,
        &[("power-cycle: deployed power-cycle-worker replicas=2", 1)],
        CONVERGENCE_BOUND,
    );
    assert_eq!(deployment_replicas(&runtime), vec![2]);

    runtime.inject_volume_append_failure(true);
    deploy_interrupted(
        &runtime,
        "deploy-append-failure",
        selium_identity_onboard::MODE_DEPLOY_NINE,
        "injected volume append failure",
    );
    runtime.inject_volume_append_failure(false);
    assert_eq!(deployment_replicas(&runtime), vec![2]);

    let control_plane = restart_control_plane(&runtime, &state, control_plane);
    assert_served_replicas(&runtime, "verify-after-append-failure", 2);
    assert_eq!(deployment_replicas(&runtime), vec![2]);

    runtime.inject_volume_sync_failure(true);
    deploy_interrupted(
        &runtime,
        "deploy-sync-failure",
        selium_identity_onboard::MODE_DEPLOY_NINE,
        "injected volume sync failure",
    );
    runtime.inject_volume_sync_failure(false);
    assert_eq!(deployment_replicas(&runtime), vec![2, 9]);

    let control_plane = restart_control_plane(&runtime, &state, control_plane);
    assert_served_replicas(&runtime, "verify-after-sync-failure", 9);
    assert_eq!(deployment_replicas(&runtime), vec![2, 9]);

    drop(runtime.stop_process(control_plane));
    for guest in report.guests {
        drop(runtime.stop_process(guest.process_id));
    }
}

fn deploy(runtime: &Runtime, name: &str, mode: u64) -> u64 {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![operator_descriptor_named(
                name,
                read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
                Vec::new(),
                mode,
                Vec::new(),
            )],
            state_dir: None,
        })
        .expect("bootstrap deployment driver");
    find(&report, name)
}

fn deploy_interrupted(runtime: &Runtime, name: &str, mode: u64, boundary: &str) {
    let report = match runtime.bootstrap_system_guests(RuntimeConfig {
        start_discovery: false,
        system_guests: vec![operator_descriptor_named(
            name,
            read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
            Vec::new(),
            mode,
            Vec::new(),
        )],
        state_dir: None,
    }) {
        Ok(report) => report,
        Err(selium_runtime::Error::EntrypointFailed(failed)) => {
            assert_eq!(failed, name);
            return;
        }
        Err(error) => panic!("unexpected interrupted deploy error: {error}"),
    };
    // Depending on how far the driver ran during bootstrap, the typed RPC
    // error either failed the entrypoint synchronously (above) or lands just
    // after the first park. Both are negative acknowledgements of the real
    // store boundary; neither is a synthetic projection failure.
    wait_for_logs(
        runtime,
        find(&report, name),
        &[(boundary, 1)],
        CONVERGENCE_BOUND,
    );
}

fn restart_control_plane(runtime: &Runtime, state: &StateRoot, control_plane: u64) -> u64 {
    runtime
        .stop_process(control_plane)
        .expect("stop control-plane");
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![control_plane_descriptor(read_wasm(
                "selium-control-plane",
                "selium_control_plane.wasm",
            ))],
            state_dir: state.config_dir(),
        })
        .expect("restart control-plane");
    find(&report, "control-plane")
}

fn assert_served_replicas(runtime: &Runtime, name: &str, replicas: u32) {
    let verifier = deploy(
        runtime,
        name,
        selium_identity_onboard::MODE_VERIFY_DEPLOYMENT,
    );
    wait_for_logs(
        runtime,
        verifier,
        &[(
            &format!("power-cycle: deployment power-cycle-worker replicas={replicas}"),
            1,
        )],
        CONVERGENCE_BOUND,
    );
}

fn deployment_replicas(runtime: &Runtime) -> Vec<u32> {
    read_log_records(runtime, selium_control_plane::CONTROL_LOG)
        .into_iter()
        .filter_map(|record| DesiredStateRecord::decode(&record.payload).ok())
        .filter_map(|record| match record {
            DesiredStateRecord::Deployment { deployment, .. } => Some(deployment.replicas),
            _ => None,
        })
        .collect()
}

fn find(report: &selium_runtime::BootstrapReport, name: &str) -> u64 {
    report
        .guests
        .iter()
        .find(|guest| guest.name == name)
        .unwrap_or_else(|| panic!("bootstrap report contains {name}"))
        .process_id
}
