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

use std::time::Duration;

use selium_abi::ResourceClass;
use selium_accountant::{
    ACCOUNTANT_ENTRYPOINT, BOOKKEEPER_ENTRYPOINT, LEDGER_LOG, LedgerRecord, Usage,
    accountant_grants, bookkeeper_grants,
};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestDescriptor};
use spine_common::{discovery_descriptor, read_wasm};

mod common;
mod spine_common;

/// The healthy tenant: plan + overage authored, active.
const ACTIVE: &str = "beta";
/// The delinquent tenant: plan + overage authored, then suspended.
const DELINQUENT: &str = "acme";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the discovery and accountant guests built for wasm32-unknown-unknown"]
async fn accountant_ledger_survives_restart_and_reauthors_enforcement() {
    let runtime = Runtime::default();

    // Seed the durable ledger before the accountant has ever booted.
    seed_policy(&runtime);

    // Boot the accounting spine: the accountant replays the ledger and
    // re-authors enforcement at boot.
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            domain_table: Vec::new(),
            system_guests: vec![
                discovery_descriptor(read_wasm("selium-discovery", "selium_discovery.wasm")),
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
                    vec!["discovery".to_string(), BOOKKEEPER_ENTRYPOINT.to_string()],
                ),
            ],
        })
        .expect("bootstrap accounting spine");

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
            domain_table: Vec::new(),
            system_guests: vec![accounting_descriptor(
                ACCOUNTANT_ENTRYPOINT,
                ACCOUNTANT_ENTRYPOINT,
                accountant_grants(),
                // Dependencies resolve against this config's guest set;
                // discovery is already running, so the respawn declares none
                // (the bootstrap loop still wires the discovery handle in).
                Vec::new(),
            )],
        })
        .expect("respawn accountant");
    let respawn = respawn_report.guests.first().expect("respawned accountant");

    // Wait for readiness (the respawned guest re-publishes its narrowing
    // table and control surface), then re-verify the re-authoring.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let ready = runtime
            .kernel()
            .processes()
            .read_activity_from(0)
            .iter()
            .any(|event| {
                event.process_id == Some(respawn.process_id)
                    && event.message.contains("guest ready")
            });
        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "respawned accountant never reached readiness"
        );
        std::thread::sleep(Duration::from_millis(500));
    }

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
    dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: name.to_string(),
        module_id: "accounting-module".to_string(),
        module_bytes: read_wasm("selium-accountant", "selium_accountant.wasm"),
        entrypoint: entrypoint.to_string(),
        arguments: Vec::new(),
        grants,
        dependencies,
        readiness: selium_runtime::ReadinessCondition::ActivityLogContains(
            "guest ready".to_string(),
        ),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

/// Asserts the accountant re-authored enforcement from the replayed ledger:
/// the delinquent tenant's quotas are zeroed; the active tenant is capped at
/// its hard ceiling (plan + overage) for both storage classes.
fn assert_enforcement_reauthored(runtime: &Runtime) {
    let quota = runtime.kernel().quota();

    // Delinquent: narrowing-to-nothing and zeroed quotas.
    for class in [
        ResourceClass::SharedRegion,
        ResourceClass::DurableLog,
        ResourceClass::BlobStore,
    ] {
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
    for class in [ResourceClass::DurableLog, ResourceClass::BlobStore] {
        assert_eq!(
            quota.lookup(ACTIVE, class.clone()),
            Some(20 * 65_536),
            "active tenant's {class:?} quota must be its hard ceiling"
        );
    }
}

/// Seeds one encoded ledger record into the durable usage log, host-side, in
/// the exact wire format the accountant appends.
fn seed_ledger_record(runtime: &Runtime, record: &LedgerRecord) {
    let storage = runtime.kernel().storage();
    let memory = runtime.kernel().memory();
    let ledger = storage.open_log(&memory, LEDGER_LOG);
    let payload = selium_abi::encode_rkyv(record).expect("encode ledger record");
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    storage
        .append_log(ledger.local_id, timestamp_ms, Vec::new(), payload)
        .expect("append ledger record");
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
