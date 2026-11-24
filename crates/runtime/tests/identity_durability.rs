//! Identity durability interruption tests over the real store RPC path.
//!
//! A failed principal-grant append leaves the last acknowledged grant set in
//! place. An append that succeeds before a failed sync has an uncertain
//! outcome: the caller receives an error, and restart replays the complete
//! frame actually present rather than treating the failed sync as an ack.

#![allow(clippy::panic, reason = "integration-test assertions")]

use rcgen::PublicKeyData as _;
use selium_identity::PrincipalRecord;
use selium_runtime::{Runtime, RuntimeConfig};
use spine_common::{
    CONVERGENCE_BOUND, ONBOARD_STORE, StateRoot, discovery_descriptor, identity_descriptor,
    operator_descriptor_named, read_blob, read_log_records, read_manifest, read_wasm,
    runtime_in_state_root, store_descriptor, wait_for_logs,
};

mod common;
mod spine_common;

const FINGERPRINT: [u8; 32] = [0xAA; 32];
const LEAF_AFTER_ROTATION: &str = "acme-leaf-after-restart";

// scenario: verification-gate/crash-injection-stops-durable-components-mid-write/identity-or-control-plane-stops-during-policy-write
// scenario: selium-identity/identity-mutations-are-durable-before-visible/principal-append-fails
#[test]
#[ignore = "requires discovery, store, identity and identity-onboard WASM guests"]
fn store_interruptions_replay_identity_state_consistent_with_durable_bytes() {
    let state = StateRoot::new("identity-durability");
    let runtime = runtime_in_state_root(&state);
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                store_descriptor(vec!["discovery".to_string()]),
                identity_descriptor(read_wasm("selium-identity", "selium_identity.wasm")),
            ],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap identity durability spine");
    let identity = find(&report, "identity");

    let initial = set_grants(
        &runtime,
        "grants-initial",
        selium_identity_onboard::MODE_SET_GRANTS_ONE,
    );
    wait_for_logs(
        &runtime,
        initial,
        &[("principal grants [1] recorded", 1)],
        CONVERGENCE_BOUND,
    );
    assert_eq!(grant_values(&runtime), vec![1]);

    // Intercept the real VolumeAppend hostcall made by the store for the
    // identity DurableLog append. No projection helper or synthetic Result is
    // involved, and no replacement record reaches the durable log.
    runtime.inject_volume_append_failure(true);
    set_grants_interrupted(
        &runtime,
        "grants-append-failure",
        selium_identity_onboard::MODE_SET_GRANTS_TWO,
        "append failed",
    );
    runtime.inject_volume_append_failure(false);
    assert_eq!(grant_values(&runtime), vec![1]);

    let identity = restart_identity(&runtime, &state, identity);
    assert_served_grants(&runtime, "grants-after-append-failure", 1);
    assert_eq!(grant_values(&runtime), vec![1]);

    // This interruption is after the store accepted the append. The sync RPC
    // fails, so the caller is not acknowledged; the complete replacement frame
    // is nevertheless present and is exactly what replay must recover.
    runtime.inject_volume_sync_failure(true);
    set_grants_interrupted(
        &runtime,
        "grants-sync-failure",
        selium_identity_onboard::MODE_SET_GRANTS_TWO,
        "sync failed",
    );
    runtime.inject_volume_sync_failure(false);
    assert_eq!(grant_values(&runtime), vec![1, 2]);

    let identity = restart_identity(&runtime, &state, identity);
    assert_served_grants(&runtime, "grants-after-sync-failure", 2);
    assert_eq!(grant_values(&runtime), vec![1, 2]);

    drop(runtime.stop_process(identity));
    for guest in report.guests {
        drop(runtime.stop_process(guest.process_id));
    }
}

/// A definite tenant-log append rejection restores the CA displaced by a
/// rotation. A later sync failure is deliberately different: the appended CA
/// remains installed and is what identity replay reconciles.
#[test]
#[ignore = "requires discovery, store, identity and identity-onboard WASM guests"]
fn rotation_append_failure_restores_old_ca_but_sync_failure_replays_new_ca() {
    let state = StateRoot::new("identity-rotation-durability");
    let runtime = runtime_in_state_root(&state);
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                store_descriptor(vec!["discovery".to_string()]),
                identity_descriptor(read_wasm("selium-identity", "selium_identity.wasm")),
            ],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap identity rotation spine");
    let identity = find(&report, "identity");
    let client_key =
        rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("generate client key");
    let spki = client_key.subject_public_key_info();

    let onboarded = operator(
        &runtime,
        "rotation-onboard",
        spki.clone(),
        selium_identity_onboard::MODE_ONBOARD,
    );
    wait_for_logs(
        &runtime,
        onboarded,
        &[("acme onboarded and client certificate issued", 1)],
        CONVERGENCE_BOUND,
    );
    let ca_path = state.path().join("keys/tenants/acme.cert.der");
    let committed_ca = std::fs::read(&ca_path).expect("read committed tenant CA");

    runtime.inject_volume_append_failure(true);
    operator_interrupted(
        &runtime,
        "rotation-append-failure",
        spki.clone(),
        selium_identity_onboard::MODE_ROTATE,
        "tenant record append failed",
    );
    runtime.inject_volume_append_failure(false);
    assert_eq!(
        std::fs::read(&ca_path).expect("read CA after rejected rotation"),
        committed_ca,
        "a definite precommit failure must restore the old CA on disk"
    );
    assert_leaf_chains_to(
        &runtime,
        &committed_ca,
        "issue-after-append-rollback",
        &spki,
    );

    runtime.inject_volume_sync_failure(true);
    operator_interrupted(
        &runtime,
        "rotation-sync-failure",
        spki.clone(),
        selium_identity_onboard::MODE_ROTATE,
        "tenant record sync failed",
    );
    runtime.inject_volume_sync_failure(false);
    let appended_ca = std::fs::read(&ca_path).expect("read CA after sync interruption");
    assert_ne!(
        appended_ca, committed_ca,
        "append-success/sync-failure must retain the appended CA"
    );

    let identity = restart_identity(&runtime, &state, identity);
    assert_leaf_chains_to(&runtime, &appended_ca, "issue-after-sync-replay", &spki);

    drop(runtime.stop_process(identity));
    for guest in report.guests {
        drop(runtime.stop_process(guest.process_id));
    }
}

fn operator(runtime: &Runtime, name: &str, spki: Vec<u8>, mode: u64) -> u64 {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![operator_descriptor_named(
                name,
                read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
                spki,
                mode,
                Vec::new(),
            )],
            state_dir: None,
        })
        .expect("bootstrap identity operator");
    find(&report, name)
}

fn operator_interrupted(runtime: &Runtime, name: &str, spki: Vec<u8>, mode: u64, boundary: &str) {
    let report = match runtime.bootstrap_system_guests(RuntimeConfig {
        start_discovery: false,
        system_guests: vec![operator_descriptor_named(
            name,
            read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
            spki,
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
        Err(error) => panic!("unexpected interrupted operator error: {error}"),
    };
    wait_for_logs(
        runtime,
        find(&report, name),
        &[(boundary, 1)],
        CONVERGENCE_BOUND,
    );
}

fn assert_leaf_chains_to(runtime: &Runtime, ca_der: &[u8], name: &str, spki: &[u8]) {
    let issued = operator(
        runtime,
        name,
        spki.to_vec(),
        selium_identity_onboard::MODE_ISSUE,
    );
    wait_for_logs(
        runtime,
        issued,
        &[("issued a further acme certificate after the restart", 1)],
        CONVERGENCE_BOUND,
    );
    let leaf_der = read_manifest(runtime, ONBOARD_STORE, LEAF_AFTER_ROTATION)
        .and_then(|blob_id| read_blob(runtime, ONBOARD_STORE, &blob_id))
        .expect("read leaf issued after rotation interruption");
    let (_, tenant_ca) = x509_parser::parse_x509_certificate(ca_der).expect("parse tenant CA");
    let (_, leaf) = x509_parser::parse_x509_certificate(&leaf_der).expect("parse issued leaf");
    assert_eq!(leaf.issuer(), tenant_ca.subject());
    leaf.verify_signature(Some(tenant_ca.public_key()))
        .expect("issued leaf must chain cryptographically to the expected CA");
}

fn set_grants(runtime: &Runtime, name: &str, mode: u64) -> u64 {
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
        .expect("bootstrap grant driver");
    find(&report, name)
}

fn set_grants_interrupted(runtime: &Runtime, name: &str, mode: u64, boundary: &str) {
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
        Err(error) => panic!("unexpected interrupted grant error: {error}"),
    };
    wait_for_logs(
        runtime,
        find(&report, name),
        &[(boundary, 1)],
        CONVERGENCE_BOUND,
    );
}

fn restart_identity(runtime: &Runtime, state: &StateRoot, identity: u64) -> u64 {
    runtime.stop_process(identity).expect("stop identity");
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![identity_descriptor(read_wasm(
                "selium-identity",
                "selium_identity.wasm",
            ))],
            state_dir: state.config_dir(),
        })
        .expect("restart identity");
    find(&report, "identity")
}

fn assert_served_grants(runtime: &Runtime, name: &str, value: u8) {
    let verifier = set_grants(runtime, name, selium_identity_onboard::MODE_VERIFY_GRANTS);
    wait_for_logs(
        runtime,
        verifier,
        &[(&format!("principal grants Some([{value}])"), 1)],
        CONVERGENCE_BOUND,
    );
}

fn grant_values(runtime: &Runtime) -> Vec<u8> {
    read_log_records(runtime, selium_identity::PRINCIPAL_LOG)
        .into_iter()
        .filter_map(|record| selium_abi::decode_rkyv::<PrincipalRecord>(&record.payload).ok())
        .filter_map(|record| match record {
            PrincipalRecord::Set {
                fingerprint,
                grants,
                ..
            } if fingerprint == FINGERPRINT => grants.into_iter().next(),
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
