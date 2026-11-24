//! Power-cycle integration test (task 6.1).
//!
//! Creates a platform, exercises its durable surfaces, stops it, boots a fresh
//! one against the same state directory, and asserts every surface survived:
//! onboarded tenants and recorded principals, the ceilings the accountant
//! re-authors from its replayed ledger, an uploaded module's bytes, a recorded
//! deployment, and the certificate chain's continuity.
//!
//! The second lifetime runs in a **fresh process**. That is not incidental:
//! the runtime installs a process-global region provider pinned to the first
//! kernel it sees, so two runtimes in one process would share one kernel — and
//! a power cycle is exactly a fresh process over the same durable directory.
//!
//! `#[ignore]`d by default — it requires the discovery, store, identity,
//! accountant, control-plane and identity-onboard guests built for
//! `wasm32-unknown-unknown`:
//!
//! ```sh
//! cargo test -p selium-runtime --test power_cycle -- --ignored
//! ```

use selium_abi::ResourceClass;
use selium_runtime::RuntimeConfig;
use spine_common::{
    CONVERGENCE_BOUND, StateRoot, control_plane_descriptor, discovery_descriptor,
    identity_descriptor, operator_descriptor_named, read_blob, read_log_records, read_manifest,
    read_wasm, store_descriptor, wait_for_logs,
};

mod common;
mod spine_common;

/// Manifest the post-restart leaf is published under.
const LEAF_MANIFEST_AFTER_RESTART: &str = "acme-leaf-after-restart";
/// Blob store the onboarding guest publishes the issued leaf to.
const ONBOARD_STORE: &str = "selium.identity-onboard.out";
/// The second lifetime's test name, re-invoked in a fresh process.
const SECOND_LIFETIME: &str = "second_lifetime_asserts_survival";
/// Environment variable handing the state directory to the second lifetime.
const STATE_ROOT_ENV: &str = "SELIUM_POWER_CYCLE_ROOT";
/// The tenant the test onboards.
const TENANT: &str = "acme";
/// The identity guest's tenant log, as the guest names it.
const TENANT_LOG: &str = "selium.identity.tenants";

/// The accounting guest: two entrypoints over one module.
fn accounting_descriptor(
    name: &str,
    grants: Vec<selium_abi::CapabilityGrant>,
    _dependencies: Vec<String>,
) -> selium_runtime::SystemGuestDescriptor {
    selium_runtime::SystemGuestDescriptor {
        name: name.to_string(),
        module_id: "accounting-module".to_string(),
        module_bytes: read_wasm("selium-accountant", "selium_accountant.wasm"),
        entrypoint: name.to_string(),
        arguments: Vec::new(),
        grants,
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// Boot order-guest set for both lifetimes: discovery, the durable store, the
/// identity mint authority, the accounting pair, and the control plane.
fn platform_guests() -> Vec<selium_runtime::SystemGuestDescriptor> {
    vec![
        discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
        store_descriptor(vec!["discovery".to_string()]),
        identity_descriptor(read_wasm("selium-identity", "selium_identity.wasm")),
        accounting_descriptor(
            selium_accountant::BOOKKEEPER_ENTRYPOINT,
            selium_accountant::bookkeeper_grants(),
            vec!["discovery".to_string(), "store".to_string()],
        ),
        accounting_descriptor(
            selium_accountant::ACCOUNTANT_ENTRYPOINT,
            selium_accountant::accountant_grants(),
            vec![
                "discovery".to_string(),
                "store".to_string(),
                selium_accountant::BOOKKEEPER_ENTRYPOINT.to_string(),
            ],
        ),
        control_plane_descriptor(read_wasm(
            "selium-control-plane",
            "selium_control_plane.wasm",
        )),
    ]
}
// scenario: selium-store/store-owned-durability-policy/durable-acknowledgement-survives-a-power-cycle

// scenario: control-plane/module-upload-to-the-durable-store/upload-stores-a-module
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the platform guests built for wasm32-unknown-unknown"]
async fn platform_state_survives_a_power_cycle() {
    let state = StateRoot::new("power-cycle");

    // ── First lifetime ────────────────────────────────────────────────────
    // No keyring is installed by hand: the state root's first boot generates
    // and persists the host PKI hierarchy, which is the surface the restart
    // must reuse.
    let runtime = spine_common::runtime_in_state_root(&state);

    let client_key =
        rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("client key");
    let client_spki = rcgen::PublicKeyData::subject_public_key_info(&client_key);

    let mut guests = platform_guests();
    guests.push(operator_descriptor_named(
        "identity-onboard",
        read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
        client_spki.clone(),
        selium_identity_onboard::MODE_ONBOARD,
        vec![
            "discovery".to_string(),
            "store".to_string(),
            "identity".to_string(),
        ],
    ));
    guests.push(operator_descriptor_named(
        "power-cycle-op",
        read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
        Vec::new(),
        selium_identity_onboard::MODE_DEPLOY,
        vec![
            "discovery".to_string(),
            "store".to_string(),
            "control-plane".to_string(),
        ],
    ));
    guests.push(operator_descriptor_named(
        "power-cycle-quota",
        read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
        Vec::new(),
        selium_identity_onboard::MODE_SET_QUOTA,
        vec![
            "discovery".to_string(),
            "store".to_string(),
            "accountant".to_string(),
        ],
    ));

    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: guests,
            state_dir: state.config_dir(),
        })
        .unwrap_or_else(|error| panic!("first lifetime bootstrap: {error}"));

    let find = |name: &str| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == name)
            .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
            .process_id
    };
    let onboard = find("identity-onboard");
    let deploy = find("power-cycle-op");
    let quota = find("power-cycle-quota");

    wait_for_logs(
        &runtime,
        onboard,
        &[("onboarded and client certificate issued", 1)],
        CONVERGENCE_BOUND,
    );
    wait_for_logs(
        &runtime,
        deploy,
        &[("power-cycle: deployed power-cycle-worker", 1)],
        CONVERGENCE_BOUND,
    );
    wait_for_logs(
        &runtime,
        quota,
        &[("power-cycle: authored acme process ceiling", 1)],
        CONVERGENCE_BOUND,
    );

    // The tenant's durable record landed in the identity guest's log.
    assert!(
        !read_log_records(&runtime, TENANT_LOG).is_empty(),
        "the identity guest wrote the tenant record to its durable log"
    );

    // Stop every guest: this is the power cycle's first half.
    for guest in &report.guests {
        drop(runtime.stop_process(guest.process_id));
    }
    drop(runtime);

    // ── Second lifetime, in a fresh process ────────────────────────────────
    let exe = std::env::current_exe().expect("test binary path");
    let status = std::process::Command::new(exe)
        .args(["--ignored", "--exact", "--nocapture", SECOND_LIFETIME])
        .env(STATE_ROOT_ENV, state.path())
        .status()
        .expect("spawn the second lifetime");
    assert!(
        status.success(),
        "the second lifetime asserts the platform's durable state survived"
    );
}

/// The second lifetime: a fresh process booting against the directory the first
/// left behind. Skipped when run standalone (the parent process sets
/// [`STATE_ROOT_ENV`]).
// scenario: control-plane/module-upload-to-the-durable-store/uploaded-module-survives-a-restart
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "driven by platform_state_survives_a_power_cycle in a fresh process"]
async fn second_lifetime_asserts_survival() {
    let Ok(root) = std::env::var(STATE_ROOT_ENV) else {
        // Standalone run: nothing to assert without a first lifetime's
        // directory.
        return;
    };
    let state = StateRoot::adopt(root);

    let runtime = spine_common::runtime_in_state_root(&state);

    // A certificate minted after the restart must chain to the tenant CA the
    // first lifetime recorded: the issue succeeds only if the tenant survived
    // in the identity guest's replayed log *and* the host keyring still holds
    // its CA key.
    let client_key =
        rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("client key");
    let client_spki = rcgen::PublicKeyData::subject_public_key_info(&client_key);

    let mut guests = platform_guests();
    guests.push(operator_descriptor_named(
        "identity-onboard",
        read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
        client_spki,
        selium_identity_onboard::MODE_ISSUE,
        vec![
            "discovery".to_string(),
            "store".to_string(),
            "identity".to_string(),
        ],
    ));
    guests.push(operator_descriptor_named(
        "power-cycle-op",
        read_wasm("selium-identity-onboard", "selium_identity_onboard.wasm"),
        Vec::new(),
        selium_identity_onboard::MODE_VERIFY_DEPLOYMENT,
        vec![
            "discovery".to_string(),
            "store".to_string(),
            "control-plane".to_string(),
        ],
    ));

    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: guests,
            state_dir: state.config_dir(),
        })
        .unwrap_or_else(|error| panic!("second lifetime bootstrap: {error}"));

    let find = |name: &str| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == name)
            .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
            .process_id
    };
    let issued = find("identity-onboard");
    let verify = find("power-cycle-op");

    // The certificate chain continues: the fresh leaf is signed by the tenant
    // CA recorded before the restart.
    wait_for_logs(
        &runtime,
        issued,
        &[("issued a further acme certificate after the restart", 1)],
        CONVERGENCE_BOUND,
    );
    let ca_before = std::fs::read(state.path().join("keys/tenants/acme.cert.der"))
        .expect("the tenant CA recorded before the restart is still on disk");
    let leaf_after = read_manifest(&runtime, ONBOARD_STORE, LEAF_MANIFEST_AFTER_RESTART)
        .and_then(|blob_id| read_blob(&runtime, ONBOARD_STORE, &blob_id))
        .expect("the post-restart leaf is readable from the blob store");
    let (_, tenant_ca) = x509_parser::parse_x509_certificate(&ca_before).expect("parse tenant ca");
    let (_, leaf) = x509_parser::parse_x509_certificate(&leaf_after).expect("parse leaf");
    assert_eq!(
        leaf.issuer().to_string(),
        tenant_ca.subject().to_string(),
        "the post-restart leaf chains to the tenant CA recorded before the restart"
    );

    // The deployment recorded before the restart is still served.
    wait_for_logs(
        &runtime,
        verify,
        &[("power-cycle: deployment power-cycle-worker replicas=2", 1)],
        CONVERGENCE_BOUND,
    );

    // The uploaded module's bytes survived.
    let store = selium_control_plane::CONTROL_BLOB_STORE;
    let module = read_manifest(&runtime, store, "power-cycle/module")
        .and_then(|blob_id| read_blob(&runtime, store, &blob_id))
        .expect("the uploaded module is readable from the blob store");
    assert_eq!(module, b"selium power-cycle module".to_vec());

    // The accountant re-authored the tenant's ceilings from the ledger the
    // first lifetime appended to: the process ceiling is the one the operator
    // authored, and the storage dimension is authored with it.
    let quota = runtime.kernel().quota();
    assert_eq!(
        quota.lookup(TENANT, ResourceClass::Process),
        Some(selium_identity_onboard::QUOTA_PROCESSES),
        "the replayed ledger re-authored the tenant's process ceiling"
    );
    assert!(
        quota.lookup(TENANT, ResourceClass::Volume).is_some(),
        "the replayed ledger re-authored the tenant's volume ceiling"
    );

    for guest in &report.guests {
        drop(runtime.stop_process(guest.process_id));
    }
}
