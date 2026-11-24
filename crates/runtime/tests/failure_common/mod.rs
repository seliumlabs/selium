//! Shared setup for the failure-injection test binaries.
//!
//! Each failure mode lives in its own test binary: the runtime installs a
//! **process-global** region provider on first use (pinned to the first
//! kernel it sees), so two runtimes that both boot discovery cannot share one
//! process. One binary per scenario keeps that invariant while still sharing
//! the descriptor builders and wait helpers here.

#![allow(
    dead_code,
    clippy::panic,
    reason = "shared failure-injection support compiled per binary"
)]

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use selium_abi::{
    ActivityKind, Capability, CapabilityGrant, ResourceClass, ResourceIdentity, ResourceSelector,
};
use selium_runtime::{
    BootstrapReport, Runtime, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor,
};
use selium_service::{DiscoveryRequest, FlatMsg};
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{framed::FramedRead, pubsub::Subscriber};

/// Bound on a guest reaching a logged milestone (registration, resolution).
pub(crate) const CONVERGENCE_BOUND: Duration = Duration::from_secs(60);
/// Bound on a guest process exiting (or a failed bootstrap returning) when the
/// failure is immediate, not a convergence wait.
pub(crate) const EXIT_BOUND: Duration = Duration::from_secs(30);
/// The wake-probe publisher role (mirrors the fixture).
pub(crate) const MODE_WAKE_PUBLISHER: u64 = 0;
/// The wake-probe watcher role.
pub(crate) const MODE_WAKE_WATCHER: u64 = 1;
/// Bound on a revocation reaching the discovery feed.
pub(crate) const REVOCATION_BOUND: Duration = Duration::from_secs(30);
pub(crate) const ROLE_CLAIM_ROUTE: u64 = 7;
pub(crate) const ROLE_FORGE: u64 = 6;
/// Discovery-probe roles, mirrored from the fixture.
pub(crate) const ROLE_PROVIDER: u64 = 1;
pub(crate) const ROLE_REGISTER_NO_RENEW: u64 = 4;
pub(crate) const ROLE_RESOLVER: u64 = 3;
pub(crate) const ROLE_RESOLVER_UNTIL_GONE: u64 = 8;
pub(crate) const ROLE_TRAP_AFTER_REGISTER: u64 = 5;

/// Attaches a host-side subscriber to the discovery feed.
pub(crate) fn attach_feed(runtime: &Runtime) -> Subscriber<DiscoveryRequest, ShmTransport> {
    let feed_region_id = runtime
        .discovery_feed_region_id()
        .expect("discovery feed region id");
    let channel = Channel::attach_with_provider(runtime.region_provider().as_ref(), feed_region_id)
        .expect("attach to discovery feed");
    let capacity = channel.ring().capacity();
    let transport = ShmTransport::new_replay(&channel, &channel).expect("feed transport");
    Subscriber::new(FramedRead::new(transport), Some(capacity))
}

/// Boots the discovery system guest and returns its process id.
pub(crate) fn bootstrap_discovery(runtime: &Runtime) -> u64 {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![discovery_descriptor(discovery_wasm())],
            state_dir: None,
        })
        .expect("bootstrap discovery");
    process_id_of(&report, "discovery")
}

/// Spawns a discovery-probe role wired to the running discovery listener.
///
/// The leading argument is the listener shared id (the `Context` handle), and
/// the descriptor carries the explicit listener grant the bootstrap would
/// otherwise add — the same shape `route_watch_convergence` uses.
pub(crate) fn bootstrap_probe(
    runtime: &Runtime,
    name: &str,
    role: u64,
    uri: &str,
    handlers: Vec<String>,
) -> selium_runtime::Result<BootstrapReport> {
    let listener = runtime
        .discovery_listener_shared_id()
        .expect("discovery listener shared id");
    let descriptor = SystemGuestDescriptor {
        name: name.to_string(),
        module_id: format!("{name}-module"),
        module_bytes: probe_wasm(),
        entrypoint: "discovery_probe".to_string(),
        arguments: vec![
            SystemGuestArg::Integer(listener),
            SystemGuestArg::Pointer(uri.as_bytes().to_vec()),
            SystemGuestArg::Integer(role),
        ],
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ExplicitResource(
                    ResourceIdentity::Shared(listener),
                )],
            ),
        ],
        tenant: Some("probe".to_string()),
        discovery_service: false,
        handlers,
    };

    runtime.bootstrap_system_guests(RuntimeConfig {
        start_discovery: false,
        system_guests: vec![descriptor],
        state_dir: None,
    })
}

/// Bootstraps a probe whose discovery handle is absent/zero, for the
/// "discovery unavailable" scenario. Returns the bootstrap outcome so the test
/// can accept either a fast error or a fast guest exit.
pub(crate) fn bootstrap_probe_without_discovery(
    runtime: &Runtime,
    name: &str,
    role: u64,
    uri: &str,
) -> selium_runtime::Result<BootstrapReport> {
    let descriptor = SystemGuestDescriptor {
        name: name.to_string(),
        module_id: format!("{name}-module"),
        module_bytes: probe_wasm(),
        entrypoint: "discovery_probe".to_string(),
        arguments: vec![
            SystemGuestArg::Integer(0),
            SystemGuestArg::Pointer(uri.as_bytes().to_vec()),
            SystemGuestArg::Integer(role),
        ],
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
        tenant: Some("probe".to_string()),
        discovery_service: false,
        handlers: Vec::new(),
    };

    runtime.bootstrap_system_guests(RuntimeConfig {
        start_discovery: false,
        system_guests: vec![descriptor],
        state_dir: None,
    })
}

/// The discovery system guest descriptor used by every failure scenario.
pub(crate) fn discovery_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery".to_string(),
        module_id: "discovery-module".to_string(),
        module_bytes,
        entrypoint: "discovery_main".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(Capability::ProcessProbe, Vec::new()),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///discovery-routes".to_string())],
            ),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///discovery".to_string())],
            ),
        ],
        tenant: None,
        discovery_service: true,
        handlers: Vec::new(),
    }
}

pub(crate) fn discovery_wasm() -> Vec<u8> {
    super::common::read_guest_wasm_debug("selium-discovery", "selium_discovery.wasm")
}

/// Drains every event currently readable from the feed.
pub(crate) fn drain_feed(
    subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>,
) -> Vec<DiscoveryRequest> {
    let mut events = Vec::new();
    loop {
        match subscriber.read_with_tag() {
            Ok((request, _tag)) => events.push(request),
            Err(selium_wire::error::Error::BufferEmpty) => break,
            Err(error) => panic!("discovery feed read failed: {error}"),
        }
    }
    events
}

/// Drains a process's log channel, returning nothing when it is already gone.
pub(crate) fn drain_logs(runtime: &Runtime, process_id: u64) -> Vec<String> {
    let Ok(frames) = runtime.kernel().processes().drain_log_channel(process_id) else {
        return Vec::new();
    };
    frames
        .iter()
        .map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        })
        .collect()
}

pub(crate) fn probe_wasm() -> Vec<u8> {
    super::common::read_guest_wasm_debug("selium-discovery-probe", "selium_discovery_probe.wasm")
}

/// Bootstraps a probe that is expected to stay resident (it parks after its
/// role's work), returning its process id.
pub(crate) fn spawn_probe(
    runtime: &Runtime,
    name: &str,
    role: u64,
    uri: &str,
    handlers: Vec<String>,
) -> u64 {
    let report = bootstrap_probe(runtime, name, role, uri, handlers).unwrap_or_else(|error| {
        panic!(
            "bootstrap probe {name}: {error}; activity: {:#?}",
            runtime.activity_log()
        )
    });
    report
        .guests
        .first()
        .expect("probe in bootstrap report")
        .process_id
}

/// A root-tenant probe (tenant `None`), for root-namespace routes such as a
/// handler claim. Guest custom routes use [`spawn_probe`].
pub(crate) fn spawn_root_probe(
    runtime: &Runtime,
    name: &str,
    role: u64,
    uri: &str,
    handlers: Vec<String>,
) -> u64 {
    let report = bootstrap_probe_with_tenant(runtime, name, role, uri, handlers, None)
        .unwrap_or_else(|error| {
            panic!(
                "bootstrap root probe {name}: {error}; activity: {:#?}",
                runtime.activity_log()
            )
        });
    report
        .guests
        .first()
        .expect("root probe in bootstrap report")
        .process_id
}

/// Spawns the wake-probe fixture in `mode`; `region` is its pointer argument.
pub(crate) fn spawn_wake_probe(runtime: &Runtime, name: &str, mode: u64, region: &str) -> u64 {
    runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: name.to_string(),
            module_id: format!("{name}-module"),
            module_bytes: super::common::read_guest_wasm_debug(
                "selium-wake-probe",
                "selium_wake_probe.wasm",
            ),
            entrypoint: "wake_probe".to_string(),
            arguments: vec![
                SystemGuestArg::Integer(mode),
                SystemGuestArg::Pointer(region.as_bytes().to_vec()),
            ],
            grants: vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        })
        .unwrap_or_else(|error| {
            panic!(
                "spawn wake probe {name}: {error}; activity: {:#?}",
                runtime.activity_log()
            )
        })
        .process_id
}

/// Best-effort stop for a guest that may already have exited on its own.
pub(crate) fn stop_if_resident(runtime: &Runtime, process_id: u64) {
    if runtime.is_guest_resident(process_id) {
        // Best-effort teardown: an exited guest may be reaped between the
        // residency check and the stop.
        drop(runtime.stop_process(process_id));
    }
}

/// Waits until a process is no longer resident (it exited, crashed, or was
/// reaped). Bounded: a client that parks forever fails the test.
pub(crate) fn wait_for_exit(runtime: &Runtime, process_id: u64, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        // The loaded-guest table is transiently empty while a guest is being
        // polled, so exit is observed from the durable activity record.
        if runtime.activity_log().iter().any(|event| {
            event.process_id == Some(process_id) && event.kind == ActivityKind::ProcessExited
        }) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("process {process_id} did not exit within {timeout:?}");
}

/// Waits for a log line containing `needle`, returning every line seen.
pub(crate) fn wait_for_log(
    runtime: &Runtime,
    process_id: u64,
    needle: &str,
    timeout: Duration,
) -> Vec<String> {
    let mut seen = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        seen.extend(drain_logs(runtime, process_id));
        // A guest that exits before its waiter observes the channel has its
        // remaining frames drained into the `ProcessExited` activity record.
        // Read it too, so a one-shot guest's last milestone stays observable.
        if !runtime.is_guest_resident(process_id) {
            for message in reaped_log_lines(runtime, process_id) {
                if !seen.contains(&message) {
                    seen.push(message);
                }
            }
        }
        if seen.iter().any(|message| message.contains(needle)) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {needle:?}; got {seen:?}");
}

/// Waits until the feed carries an owner-keyed `ProcessGone` (or a `Revoke` of
/// `uri`) for the stopped/crashed process.
pub(crate) fn wait_for_revocation(
    runtime: &Runtime,
    subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>,
    process_id: u64,
    uri: Option<&str>,
    timeout: Duration,
) -> Vec<DiscoveryRequest> {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    let mut seen_owners: HashSet<u64> = HashSet::new();
    while Instant::now() < deadline {
        for event in drain_feed(subscriber) {
            let matched = match &event {
                DiscoveryRequest::ProcessGone { process_id: gone } => {
                    seen_owners.insert(*gone);
                    *gone == process_id
                }
                DiscoveryRequest::Revoke { uri: revoked } => {
                    uri.is_some_and(|expected| revoked == expected)
                }
                _ => false,
            };
            seen.push(event);
            if matched {
                return seen;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "timed out waiting for revocation of process {process_id}/{uri:?}; \
         owners seen: {seen_owners:?}; discovery resident: {}",
        runtime.is_guest_resident(process_id)
    );
}

/// Bootstraps a probe under an explicit tenant (`None` is the root namespace).
fn bootstrap_probe_with_tenant(
    runtime: &Runtime,
    name: &str,
    role: u64,
    uri: &str,
    handlers: Vec<String>,
    tenant: Option<String>,
) -> selium_runtime::Result<BootstrapReport> {
    let listener = runtime
        .discovery_listener_shared_id()
        .expect("discovery listener shared id");
    let descriptor = SystemGuestDescriptor {
        name: name.to_string(),
        module_id: format!("{name}-module"),
        module_bytes: probe_wasm(),
        entrypoint: "discovery_probe".to_string(),
        arguments: vec![
            SystemGuestArg::Integer(listener),
            SystemGuestArg::Pointer(uri.as_bytes().to_vec()),
            SystemGuestArg::Integer(role),
        ],
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ExplicitResource(
                    ResourceIdentity::Shared(listener),
                )],
            ),
        ],
        tenant,
        discovery_service: false,
        handlers,
    };

    runtime.bootstrap_system_guests(RuntimeConfig {
        start_discovery: false,
        system_guests: vec![descriptor],
        state_dir: None,
    })
}

fn process_id_of(report: &BootstrapReport, name: &str) -> u64 {
    report
        .guests
        .iter()
        .find(|guest| guest.name == name)
        .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
        .process_id
}

/// Log summaries the reap path moved into the activity log for `process_id`.
fn reaped_log_lines(runtime: &Runtime, process_id: u64) -> Vec<String> {
    runtime
        .activity_log()
        .iter()
        .filter(|event| {
            event.process_id == Some(process_id) && event.kind == ActivityKind::ProcessExited
        })
        .map(|event| event.message.clone())
        .collect()
}
