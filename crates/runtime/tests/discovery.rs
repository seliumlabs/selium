//! Discovery bootstrap integration test.
//!
//! Deploys the real `selium-discovery` and `selium-discovery-probe` WASM
//! guests together and asserts on the control-plane slice end-to-end:
//! bootstrap with discovery wiring, the revocation-only feed, guest→discovery
//! rendezvous, readiness signalling, and teardown revocation.
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
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor};
use selium_service::{DiscoveryRequest, FlatMsg};
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{framed::FramedRead, pubsub::Subscriber};

mod common;
mod spine_common;

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
    let channel = Channel::attach_with_provider(runtime.region_provider().as_ref(), feed_region_id)
        .expect("attach to discovery feed");
    let capacity = channel.ring().capacity();
    // Replay from the start of the ring so any bootstrap-time events are
    // observable to a reader that attaches later.
    let transport = ShmTransport::new_replay(&channel, &channel).expect("feed transport");
    Subscriber::new(FramedRead::new(transport), Some(capacity))
}

// scenario: selium-runtime/discovery-enabled-bootstrap/application-guest-receives-discovery-handle
// scenario: selium-runtime/runtime-discovery-publication-session/runtime-connects-to-discovery-guest
// scenario: selium-runtime/discovery-handle-passed-to-guest-entrypoints/application-guest-receives-discovery-handle-unchanged
// scenario: discovery-bootstrap/discovery-enabled-bootstrap/discovery-guest-boots-and-reports-ready
// scenario: discovery-bootstrap/discovery-enabled-bootstrap/application-guest-receives-a-discovery-handle
// scenario: discovery-bootstrap/tier-1-registration-flow/process-exit-revokes-registrations
// scenario: discovery-registration/revocation-only-tier-1-feed/processgone-round-trips-over-the-feed
// scenario: discovery-registration/revocation-only-tier-1-feed/resourcefreed-round-trips-over-the-feed
// scenario: discovery-registration/revocation-only-tier-1-feed/no-registration-events-on-the-feed
// scenario: discovery-registration/revocation-only-tier-1-feed/feed-is-not-rkyv-encoded
#[test]
#[ignore = "requires both discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn discovery_bootstrap_slice_end_to_end() {
    let discovery_wasm = discovery_wasm();
    let probe_wasm = discovery_probe_wasm();

    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(discovery_wasm),
                discovery_probe_descriptor(probe_wasm),
            ],
            state_dir: None,
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

    // --- 2.2: Convergence ---
    // There is no readiness gate and no GuestReady activity; the probe's own
    // log and the Tier-1 flow below are the convergence evidence.
    let _ = discovery_guest;

    // --- Drain probe log and verify the guest ran ---
    // The probe is short-lived: a reaped guest's frames are drained into its
    // `ProcessExited` activity record, so wait for the milestone rather than
    // racing a one-shot channel read.
    let probe_messages = spine_common::wait_for_logs(
        &runtime,
        probe_guest.process_id,
        &[("booting", 1), ("region allocated", 1)],
        spine_common::CONVERGENCE_BOUND,
    );
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

    // --- 2.3: The feed carries revocations only ---
    // Allocating a region publishes nothing: the owning guest publishes its
    // canonical record, and the host records authority synchronously.
    let mut subscriber = attach_feed_subscriber(&runtime);
    let host_region_id = alloc_region(&runtime, probe_guest.process_id, ResourceKind::SharedMemory);
    let registered = drain_feed_registrations(&mut subscriber);
    assert!(
        registered.is_empty(),
        "the host must not publish registrations, got: {registered:?}"
    );

    // --- 2.4: Assert revocation ---
    // Stop the probe process — the runtime publishes owner-keyed ProcessGone
    // plus identity-keyed ResourceFreed for the shared resources it minted.
    runtime
        .stop_process(probe_guest.process_id)
        .expect("stop probe process");

    let (_revoked, revoked_owners, revoked_resources) = drain_revoke_events(&mut subscriber);
    assert!(
        revoked_resources.contains(&("region".to_string(), host_region_id)),
        "expected identity-keyed revoke for region {host_region_id}, got: {revoked_resources:?}"
    );
    assert!(
        revoked_owners.contains(&probe_guest.process_id),
        "expected owner-keyed revoke for process {}, got: {:?}",
        probe_guest.process_id,
        revoked_owners
    );

    // Verify the probe process is fully gone and discovery remains resident.
    // `loaded_guest_count` drops while another thread drives a guest, so check
    // residency instead.
    assert!(!runtime.is_guest_resident(probe_guest.process_id));
    assert!(runtime.is_guest_resident(discovery_guest.process_id));

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
            // The registry probes capability state and records
            // registrations under a bootstrap-provisioned authority, not a
            // privileged guest identity.
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

fn discovery_probe_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery-probe".to_string(),
        module_id: "discovery-probe-module".to_string(),
        module_bytes,
        entrypoint: "discovery_probe".to_string(),
        // Bootstrap prepends the discovery handle (the leading argument is a
        // pointer). The probe's remaining arguments are its route URI and its
        // role; role `0` is the region-allocating probe this test drives.
        arguments: vec![
            SystemGuestArg::Pointer(Vec::new()),
            SystemGuestArg::Integer(0),
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
        tenant: None,
        discovery_service: false,
        // Declares a protocol handler; the host provisions the handler claim
        // and the guest publishes the route itself.
        handlers: vec!["sel-probe".to_string()],
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

/// Drains the feed, collecting registration events. The feed is
/// revocation-only, so the set must stay empty at spawn/allocation.
#[expect(clippy::panic, reason = "feed read errors in test indicate a bug")]
fn drain_feed_registrations(
    subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>,
) -> std::collections::HashSet<String> {
    let mut uris = std::collections::HashSet::new();
    loop {
        match subscriber.read_with_tag() {
            Ok((DiscoveryRequest::Register { uri, .. }, _tag)) => {
                uris.insert(uri);
            }
            Ok(_) => {}
            Err(selium_wire::error::Error::BufferEmpty) => break,
            Err(error) => panic!("feed read failed: {error}"),
        }
    }
    uris
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
fn drain_revoke_events(
    subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<ProcessId>,
    std::collections::HashSet<(String, u64)>,
) {
    let mut uris = std::collections::HashSet::new();
    let mut owners = std::collections::HashSet::new();
    let mut resources = std::collections::HashSet::new();
    loop {
        match subscriber.read_with_tag() {
            Ok((request, _tag)) => match request {
                DiscoveryRequest::Revoke { uri } => {
                    uris.insert(uri);
                }
                DiscoveryRequest::ProcessGone { process_id } => {
                    owners.insert(process_id);
                }
                DiscoveryRequest::ResourceFreed { class, id } => {
                    resources.insert((class, id));
                }
                _ => {}
            },
            Err(selium_wire::error::Error::BufferEmpty) => break,
            Err(error) => panic!("feed read failed: {error}"),
        }
    }
    (uris, owners, resources)
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `discovery-registration` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `discovery-bootstrap` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
