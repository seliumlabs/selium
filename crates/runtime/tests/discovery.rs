//! Discovery bootstrap integration test.
//!
//! Deploys the real `selium-discovery` and `selium-discovery-probe` WASM
//! guests together and asserts on the control-plane slice end-to-end:
//! bootstrap with discovery wiring, Tier-1 registration events via the
//! discovery feed, guest→discovery rendezvous, readiness signalling, and
//! URI revocation on process exit.
//!
//! Cross-guest shared-memory RPC wake is not yet implemented (tracked by
//! `channel-wake-wait`), so Tier-2 register/lookup is deferred. The
//! existing `shm_transport` tests cover the RPC codec paths.
//!
//! This test is `#[ignore]`d by default because it requires both WASM
//! guests to be built for `wasm32-unknown-unknown` first:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-discovery -p selium-discovery-probe
//! cargo test -p selium-runtime --test discovery -- --ignored
//! ```

use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, ProcessId,
    RegionProt, ResourceClass, ResourceKind, ResourceSelector,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};
use selium_service::{DiscoveryRequest, FlatMsg};
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{framed::FramedRead, pubsub::Subscriber};

mod common;

#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn alloc_region(runtime: &Runtime, process_id: ProcessId, purpose: ResourceKind) -> u64 {
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose,
            serving_tenant: None,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) => alloc.region_id,
        other => panic!("expected RegionAlloc, got {other:?}"),
    }
}

fn attach_feed_subscriber(runtime: &Runtime) -> Subscriber<DiscoveryRequest, ShmTransport> {
    let feed_region_id = runtime
        .discovery_feed_region_id()
        .expect("discovery feed region id");
    let channel = Channel::attach(feed_region_id).expect("attach to discovery feed");
    let capacity = channel.ring().capacity();
    let transport = ShmTransport::new(&channel, &channel).expect("feed transport");
    Subscriber::new(FramedRead::new(transport), Some(capacity))
}

#[test]
#[ignore = "requires both discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn discovery_bootstrap_slice_end_to_end() {
    let discovery_wasm = discovery_wasm();
    let probe_wasm = discovery_probe_wasm();

    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            domain_table: Vec::new(),
            system_guests: vec![
                discovery_descriptor(discovery_wasm),
                discovery_probe_descriptor(probe_wasm),
            ],
        })
        .expect("bootstrap system guests");

    // All guests should be in the report.
    assert_eq!(report.guests.len(), 2);
    let discovery_guest = report
        .guests
        .iter()
        .find(|g| g.name == "discovery")
        .expect("discovery guest in report");
    let probe_guest = report
        .guests
        .iter()
        .find(|g| g.name == "discovery-probe")
        .expect("probe guest in report");

    // --- 2.2: Assert both guests reached readiness ---
    let activity = runtime.activity_log();
    assert!(
        activity
            .iter()
            .any(|event| event.process_id == Some(discovery_guest.process_id)
                && event.message.contains("guest ready")),
        "expected discovery GuestReady, got: {activity:?}"
    );
    assert!(
        activity
            .iter()
            .any(|event| event.process_id == Some(probe_guest.process_id)
                && event.message.contains("guest ready")),
        "expected probe GuestReady, got: {activity:?}"
    );

    // --- Drain probe log and verify the guest ran ---
    let probe_messages = drain_log_messages(&runtime, probe_guest.process_id);
    assert!(
        probe_messages.iter().any(|message| message == "booting"),
        "expected 'booting' in probe log, got: {probe_messages:?}"
    );
    assert!(
        probe_messages
            .iter()
            .any(|message| message.contains("region allocated")),
        "expected 'region allocated' in probe log, got: {probe_messages:?}"
    );

    // --- Drain discovery log to confirm wiring ---
    let discovery_messages = drain_log_messages(&runtime, discovery_guest.process_id);
    assert!(
        discovery_messages
            .iter()
            .any(|message| message.contains("feed and listener attached")),
        "expected discovery feed/listener attach, got: {discovery_messages:?}"
    );

    // --- 2.3: Assert Tier-1 flow ---
    // Attach to the discovery feed and allocate a region from the host for
    // the probe process to observe Tier-1 register events.
    let mut subscriber = attach_feed_subscriber(&runtime);
    let host_region_id = alloc_region(&runtime, probe_guest.process_id, ResourceKind::SharedMemory);
    // The probe runs with no tenant (platform), so its region mints under the
    // root tenant as a typed `sel:///region/<id>` URI.
    let expected_uri = format!("sel:///region/{host_region_id}");

    let registered = drain_register_uris(&mut subscriber);
    assert!(
        registered.contains(&expected_uri),
        "expected Tier-1 register URI {expected_uri}, got: {registered:?}"
    );

    // --- 2.4: Assert revocation ---
    // Stop the probe process — the runtime must publish Revoke events for
    // the typed registrations it minted, plus a single owner-keyed
    // RevokeByOwner (revocation of any routes the process registered itself
    // lives in discovery, keyed by the recorded owner — there is no runtime
    // side map of guest-registered routes).
    runtime
        .stop_process(probe_guest.process_id)
        .expect("stop probe process");

    let (revoked, revoked_owners) = drain_revoke_events(&mut subscriber);
    assert!(
        revoked.contains(&expected_uri),
        "expected revoke for {expected_uri}, got: {revoked:?}"
    );
    assert!(
        revoked_owners.contains(&probe_guest.process_id),
        "expected owner-keyed revoke for process {}, got: {:?}",
        probe_guest.process_id,
        revoked_owners
    );

    // Verify the probe process is fully gone.
    assert_eq!(runtime.loaded_guest_count(), 1); // only discovery remains

    // Cleanup: stop discovery too.
    runtime
        .stop_process(discovery_guest.process_id)
        .expect("stop discovery process");
    assert_eq!(runtime.loaded_guest_count(), 0);
}

fn discovery_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery".to_string(),
        module_id: "discovery-module".to_string(),
        module_bytes,
        entrypoint: "discovery_main".to_string(),
        arguments: Vec::new(), // populated by bootstrap via set_discovery_feed_and_handle
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
        dependencies: Vec::new(),
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

fn discovery_probe_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery-probe".to_string(),
        module_id: "discovery-probe-module".to_string(),
        module_bytes,
        entrypoint: "discovery_probe".to_string(),
        arguments: Vec::new(), // populated by bootstrap via set_discovery_handle
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
        dependencies: vec!["discovery".to_string()],
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

fn discovery_probe_wasm() -> Vec<u8> {
    common::read_guest_wasm_debug("selium-discovery-probe", "selium_discovery_probe.wasm")
}

fn discovery_wasm() -> Vec<u8> {
    // The shared reader rebuilds the guest if its inputs changed (a no-op
    // when fresh) and fails loudly if the build or the read fails.
    common::read_guest_wasm_debug("selium-discovery", "selium_discovery.wasm")
}

fn drain_log_messages(runtime: &Runtime, process_id: u64) -> Vec<String> {
    let frames = runtime
        .kernel()
        .processes()
        .drain_log_channel(process_id)
        .expect("drain log channel");
    frames
        .iter()
        .map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        })
        .collect()
}

#[expect(clippy::panic, reason = "feed read errors in test indicate a bug")]
fn drain_register_uris(
    subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>,
) -> std::collections::HashSet<String> {
    let mut uris = std::collections::HashSet::new();
    loop {
        match subscriber.read_with_tag() {
            Ok((request, _tag)) => {
                if let DiscoveryRequest::Register { uri, .. } = request {
                    uris.insert(uri);
                }
            }
            Err(selium_wire::error::Error::BufferEmpty) => break,
            Err(error) => panic!("feed read failed: {error}"),
        }
    }
    uris
}

#[expect(clippy::panic, reason = "feed read errors in test indicate a bug")]
fn drain_revoke_events(
    subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<ProcessId>,
) {
    let mut uris = std::collections::HashSet::new();
    let mut owners = std::collections::HashSet::new();
    loop {
        match subscriber.read_with_tag() {
            Ok((request, _tag)) => match request {
                DiscoveryRequest::Revoke { uri } => {
                    uris.insert(uri);
                }
                DiscoveryRequest::RevokeByOwner { process_id } => {
                    owners.insert(process_id);
                }
                _ => {}
            },
            Err(selium_wire::error::Error::BufferEmpty) => break,
            Err(error) => panic!("feed read failed: {error}"),
        }
    }
    (uris, owners)
}
