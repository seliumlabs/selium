//! Accounting golden-path integration test (task 7.2).
//!
//! Drives the single-host revenue control loop end to end with the real guests:
//!
//! - the discovery service, the `bookkeeper` entrypoint, and the `accountant`
//!   entrypoint boot in dependency order (bookkeeper serves the bucket topic,
//!   accountant attaches it and serves the narrowing table + control surface);
//! - a tenant-scoped workload is booted, its metering is projected on the
//!   host, and the bookkeeper reduces it into a bucket on the shared-memory
//!   topic;
//! - the accountant merges the bucket and, on the minute boundary, rolls it
//!   into a durable ledger window — asserting the ledger records the tenant's
//!   per-dimension usage;
//! - the host's quota enforcement denies an over-ceiling shared-memory
//!   allocation, naming the tenant and dimension (the accountant's own quota
//!   authoring from account state is unit-tested at the crate level; this leg
//!   exercises the synchronous host-side enforcement the loop controls).
//!
//! `#[ignore]`d by default — it requires the guests built for
//! `wasm32-unknown-unknown` first:
//!
//! ```sh
//! cargo build --release --target wasm32-unknown-unknown \
//!   -p selium-discovery -p selium-accountant
//! cargo test -p selium-runtime --test accountant_spine -- --ignored
//! ```

use std::time::Duration;

use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, RegionProt,
    ResourceClass, ResourceKind, ResourceSelector,
};
use selium_accountant::{
    ACCOUNTANT_ENTRYPOINT, BOOKKEEPER_ENTRYPOINT, LEDGER_LOG, LedgerRecord, accountant_grants,
    bookkeeper_grants,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};
use spine_common::{discovery_descriptor, read_wasm};

mod common;
mod spine_common;

/// The tenant the workload runs as and usage accrues for.
const TENANT: &str = "acme";

fn accounting_descriptor(
    name: &str,
    entrypoint: &str,
    grants: Vec<CapabilityGrant>,
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
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the discovery and accountant guests built for wasm32-unknown-unknown"]
async fn accounting_loop_reaches_the_ledger_and_denies_over_ceiling() {
    let runtime = Runtime::default();

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
                workload_descriptor(),
            ],
        })
        .unwrap_or_else(|error| panic!("bootstrap accounting spine: {error}"));

    let find = |name: &str| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == name)
            .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
            .process_id
    };
    let workload = find("workload");

    // A workload runs: its engine-fed instruction counter accrues, bandwidth
    // is instrumented, and it allocates shared memory; the host projects the
    // per-process observation into the kernel for the bookkeeper to sample.
    runtime.record_bandwidth_usage(workload, 1024);
    let (status, op) = runtime.begin_hostcall(
        workload,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    assert!(matches!(
        runtime.poll_hostcall(workload, op),
        CompletionState::Ready(HostcallOutput::RegionAlloc(_))
    ));
    runtime.metering_tick();

    // Buckets flow and the ledger records usage on the minute boundary.
    let guest_pids: Vec<u64> = report.guests.iter().map(|guest| guest.process_id).collect();
    let window = wait_for_ledger_window(&runtime, TENANT, Duration::from_secs(90), &guest_pids);
    match window {
        LedgerRecord::Window { usage, .. } => {
            assert!(
                usage.cpu_instructions > 0,
                "engine-fed cpu usage in ledger: {usage:?}"
            );
            assert!(
                usage.memory_bytes >= 65_536,
                "memory usage in ledger: {usage:?}"
            );
            assert!(
                usage.bandwidth_bytes >= 1024,
                "bandwidth usage in ledger: {usage:?}"
            );
        }
        other => panic!("expected a window record, got {other:?}"),
    }

    // Over-ceiling denial: author a one-page shared-memory ceiling (the
    // enforcement value the accountant writes when a tenant's usage reaches
    // its budget), then a second page on top of the already-allocated one
    // exceeds the ceiling and must be denied naming the tenant and dimension.
    runtime
        .kernel()
        .quota()
        .set(TENANT, ResourceClass::SharedRegion, 65_536);
    let (second_status, second_op) = runtime.begin_hostcall(
        workload,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );
    assert_eq!(second_status, selium_abi::HOSTCALL_STATUS_FAILED);
    match runtime.poll_hostcall(workload, second_op) {
        CompletionState::Failed(error) => {
            assert!(
                error.message.contains(TENANT),
                "error names the tenant: {}",
                error.message
            );
            assert!(
                error.message.contains("SharedRegion"),
                "error names the dimension: {}",
                error.message
            );
        }
        other => panic!("expected quota denial, got {other:?}"),
    }

    for guest in report.guests {
        runtime.stop_process(guest.process_id).expect("stop guest");
    }
}

/// Polls the usage ledger until a `Window` record for `tenant` appears.
#[expect(clippy::panic, reason = "test helper")]
fn wait_for_ledger_window(
    runtime: &Runtime,
    tenant: &str,
    timeout: Duration,
    guest_pids: &[u64],
) -> LedgerRecord {
    let storage = runtime.kernel().storage();
    let memory = runtime.kernel().memory();
    let ledger = storage.open_log(&memory, LEDGER_LOG);
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let records = storage
            .replay_log(ledger.local_id, None, u32::MAX as usize)
            .expect("replay usage ledger");
        for record in records {
            if let Ok(decoded) = selium_abi::decode_rkyv::<LedgerRecord>(&record.payload)
                && matches!(&decoded, LedgerRecord::Window { tenant: t, .. } if t == tenant)
            {
                return decoded;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // Surface every guest's retained logs on the way out: warnings are
    // dropped when a guest log channel fills, so the failure mode would
    // otherwise be invisible.
    for pid in guest_pids {
        for line in spine_common::drain_logs(runtime, *pid) {
            println!("GUESTLOG {pid}: {line}");
        }
    }
    panic!("timed out waiting for an {tenant} window in the usage ledger");
}

fn workload_descriptor() -> SystemGuestDescriptor {
    // The stub entrypoint takes the bootstrap-prepended discovery-handle slot
    // (every non-discovery guest's leading Context parameter).
    let module_bytes = wat::parse_str("(module (memory 1) (func (export \"boot\") (param i64)))")
        .expect("compile workload");
    SystemGuestDescriptor {
        name: "workload".to_string(),
        module_id: "workload-module".to_string(),
        module_bytes,
        entrypoint: "boot".to_string(),
        arguments: Vec::new(),
        grants: vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![
                ResourceSelector::Tenant(TENANT.to_string()),
                ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
            ],
        )],
        dependencies: Vec::new(),
        readiness: ReadinessCondition::Immediate,
        tenant: Some(TENANT.to_string()),
        serving_role: None,
        handlers: Vec::new(),
    }
}
