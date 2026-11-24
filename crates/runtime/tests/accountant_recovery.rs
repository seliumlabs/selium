//! Accounting ledger-recovery test (task 5.2).
//!
//! Verifies the durable usage ledger survives an accountant restart and
//! that the rebooted guest re-authors enforcement state from the replayed
//! projection:
//!
//! - the ledger is seeded host-side in `LedgerRecord` wire format (the exact
//!   bytes the accountant appends) before the guest ever boots;
//! - the accountant replays it at boot and re-authors quotas (readable
//!   host-side via the kernel quota table) — proving replay + recovery;
//! - the accountant process is then stopped and re-spawned from the same
//!   descriptor, and the re-authoring is verified again — proving the
//!   ledger's durability across a real guest restart.
//!
//! `#[ignore]`d by default — it requires the guests built for
//! `wasm32-unknown-unknown` first:
//!
//! ```sh
//! cargo build --release --target wasm32-unknown-unknown \
//!   -p selium-discovery -p selium-accountant
//! cargo test -p selium-runtime --test accountant_recovery -- --ignored
//! ```

use std::time::{Duration, Instant};

use selium_abi::ResourceClass;
use selium_accountant::{
    ACCOUNTANT_ENTRYPOINT, BOOKKEEPER_ENTRYPOINT, LEDGER_LOG, LedgerRecord, Usage,
    accountant_grants, bookkeeper_grants,
};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestDescriptor};
use spine_common::{
    StateRoot, discovery_descriptor, read_wasm, runtime_in_state_root, store_descriptor,
};

mod common;
mod spine_common;

/// The healthy tenant: plan + overage authored, active.
const ACTIVE: &str = "beta";
/// The delinquent tenant: plan + overage authored, then suspended.
const DELINQUENT: &str = "acme";
/// Reviewed bound on a respawned accountant replaying the ledger and
/// re-authoring enforcement. The replay is guest work plus host polling, so a
/// loaded runner only delays it.
const RECOVERY_BOUND: Duration = Duration::from_secs(60);

// scenario: selium-guest/durable-storage-handles/log-operations-served-by-the-store
// scenario: guest-context/context-store-client/durable-log-through-the-context
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the discovery and accountant guests built for wasm32-unknown-unknown"]
async fn accountant_ledger_survives_restart_and_reauthors_enforcement() {
    let state = StateRoot::new("accountant-recovery");
    let runtime = runtime_in_state_root(&state);

    // Seed the durable ledger before the accountant has ever booted.
    seed_policy(&runtime);

    // Boot the accounting spine: the accountant replays the ledger and
    // re-authors enforcement at boot.
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
                store_descriptor(vec!["discovery".to_string()]),
                accounting_descriptor(
                    BOOKKEEPER_ENTRYPOINT,
                    BOOKKEEPER_ENTRYPOINT,
                    bookkeeper_grants(),
                    vec!["discovery".to_string()],
                ),
                accounting_descriptor(
                    ACCOUNTANT_ENTRYPOINT,
                    ACCOUNTANT_ENTRYPOINT,
                    accountant_grants(),
                    vec![
                        "discovery".to_string(),
                        "store".to_string(),
                        BOOKKEEPER_ENTRYPOINT.to_string(),
                    ],
                ),
            ],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap accounting spine");

    wait_for_enforcement_reauthored(&runtime, None, RECOVERY_BOUND);
    assert_enforcement_reauthored(&runtime);

    // Stop the accountant and re-spawn it from the same descriptor: the
    // ledger (already recorded) must rebuild the same projection and
    // re-author the same enforcement state.
    let accountant = report
        .guests
        .iter()
        .find(|guest| guest.name == ACCOUNTANT_ENTRYPOINT)
        .expect("accountant guest");
    runtime
        .stop_process(accountant.process_id)
        .expect("stop accountant");

    // Re-spawn the accountant through the bootstrap path (a follow-up spawn
    // against a runtime whose discovery is already running): the ledger
    // must rebuild the same projection and re-author the same enforcement.
    let respawn_report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![accounting_descriptor(
                ACCOUNTANT_ENTRYPOINT,
                ACCOUNTANT_ENTRYPOINT,
                accountant_grants(),
                // Dependencies resolve against this config's guest set;
                // discovery and the store are already running, so the respawn
                // declares none (the bootstrap loop still wires the discovery
                // handle in).
                Vec::new(),
            )],
            state_dir: state.config_dir(),
        })
        .expect("respawn accountant");
    let respawn = respawn_report.guests.first().expect("respawned accountant");

    // There is no readiness signal: drive the respawned guest and wait for the
    // re-authored enforcement to become observable.
    wait_for_enforcement_reauthored(&runtime, Some(respawn.process_id), RECOVERY_BOUND);
    assert_enforcement_reauthored(&runtime);

    for guest in report.guests {
        drop(runtime.stop_process(guest.process_id));
    }
    drop(runtime.stop_process(respawn.process_id));
}

fn accounting_descriptor(
    name: &str,
    entrypoint: &str,
    grants: Vec<selium_abi::CapabilityGrant>,
    _dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: name.to_string(),
        module_id: "accounting-module".to_string(),
        module_bytes: read_wasm("selium-accountant", "selium_accountant.wasm"),
        entrypoint: entrypoint.to_string(),
        arguments: Vec::new(),
        grants,
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// Asserts the accountant re-authored enforcement from the replayed ledger:
/// the delinquent tenant's quotas are zeroed; the active tenant is capped at
/// its hard ceiling (plan + overage) for memory and its resident bytes.
fn assert_enforcement_reauthored(runtime: &Runtime) {
    let quota = runtime.kernel().quota();

    // Delinquent: narrowing-to-nothing and zeroed quotas.
    for class in [ResourceClass::SharedRegion, ResourceClass::Volume] {
        assert_eq!(
            quota.lookup(DELINQUENT, class.clone()),
            Some(0),
            "delinquent tenant's {class:?} quota must be re-authored to zero"
        );
    }

    // Active: the hard ceiling (plan + overage) for memory and storage.
    assert_eq!(
        quota.lookup(ACTIVE, ResourceClass::SharedRegion),
        Some(40 * 65_536),
        "active tenant's memory quota must be its hard ceiling"
    );
    assert_eq!(
        quota.lookup(ACTIVE, ResourceClass::Volume),
        Some(20 * 65_536),
        "active tenant's volume quota must be its hard ceiling"
    );
}

/// Every enforcement fact the assertion below checks is present.
fn enforcement_reauthored(runtime: &Runtime) -> bool {
    let quota = runtime.kernel().quota();
    quota.lookup(DELINQUENT, ResourceClass::SharedRegion) == Some(0)
        && quota.lookup(DELINQUENT, ResourceClass::Volume) == Some(0)
        && quota.lookup(ACTIVE, ResourceClass::SharedRegion) == Some(40 * 65_536)
        && quota.lookup(ACTIVE, ResourceClass::Volume) == Some(20 * 65_536)
}

/// Seeds one encoded ledger record into the durable usage log, host-side, in
/// the store's own record format: the volume layer is written directly, so the
/// store's recovery path reads it exactly as it reads its own appends.
fn seed_ledger_record(runtime: &Runtime, record: &LedgerRecord) {
    let payload = selium_abi::encode_rkyv(record).expect("encode ledger record");
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();

    let volumes = runtime.kernel().volumes();
    let name = selium_store::format::log_volume(LEDGER_LOG, 0);
    let volume = volumes.open(None, &name).expect("open ledger volume");
    let existing = volumes
        .read_at(volume.local_id, 0, u32::MAX as usize)
        .expect("read ledger volume");
    let (frames, _end) = selium_store::format::scan_frames(&existing);
    let sequence = frames.len() as u64 + 1;
    let record = selium_store::format::Record {
        sequence,
        timestamp_ms,
        headers: Vec::new(),
        payload,
    };
    let framed = selium_store::format::frame(&selium_store::format::encode_record(&record));
    volumes
        .append(volume.local_id, &framed)
        .expect("append ledger record");
    volumes.sync(volume.local_id).expect("sync ledger volume");
}

fn seed_policy(runtime: &Runtime) {
    // A window of usage for the active tenant, so the replayed projection
    // carries real history alongside the policy records.
    seed_ledger_record(
        runtime,
        &LedgerRecord::SetPlan {
            tenant: DELINQUENT.to_string(),
            plan: Usage {
                cpu_instructions: 1_000_000,
                memory_bytes: 64 * 65_536,
                storage_bytes: 32 * 65_536,
                bandwidth_bytes: 1024,
            },
        },
    );
    seed_ledger_record(
        runtime,
        &LedgerRecord::SetOverage {
            tenant: DELINQUENT.to_string(),
            overage: Usage {
                cpu_instructions: 0,
                memory_bytes: 16 * 65_536,
                storage_bytes: 8 * 65_536,
                bandwidth_bytes: 0,
            },
        },
    );
    seed_ledger_record(
        runtime,
        &LedgerRecord::Delinquent {
            tenant: DELINQUENT.to_string(),
        },
    );
    seed_ledger_record(
        runtime,
        &LedgerRecord::SetPlan {
            tenant: ACTIVE.to_string(),
            plan: Usage {
                cpu_instructions: 500_000,
                memory_bytes: 32 * 65_536,
                storage_bytes: 16 * 65_536,
                bandwidth_bytes: 512,
            },
        },
    );
    seed_ledger_record(
        runtime,
        &LedgerRecord::SetOverage {
            tenant: ACTIVE.to_string(),
            overage: Usage {
                cpu_instructions: 0,
                memory_bytes: 8 * 65_536,
                storage_bytes: 4 * 65_536,
                bandwidth_bytes: 0,
            },
        },
    );
}

/// Waits, bounded, for the accountant to re-author every quota the assertion
/// below checks.
///
/// The accountant authors `SharedRegion`, `Volume`, `Process`, and `Cpu`
/// through separate hostcalls, and may park before it starts (converging on the
/// bookkeeper's topic). Observing one class is therefore not evidence that the
/// projection is complete: under pack load an intermediate state failed the
/// immediate assertion even though the guest was still working.
fn wait_for_enforcement_reauthored(runtime: &Runtime, poll: Option<u64>, bound: Duration) {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(process_id) = poll {
            runtime.poll_guest(process_id);
        }
        if enforcement_reauthored(runtime) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "accountant never re-authored enforcement within {bound:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
