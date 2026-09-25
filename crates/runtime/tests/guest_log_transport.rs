#![cfg(test)]

//! Integration tests for the guest log transport change.
//!
//! Tests the end-to-end behaviour of:
//! - Tier-1 discovery registration on AllocRegion
//! - URI revocation on process termination
//! - GuestLogRegister hostcall validation
//! - Deprecated GuestLog::write/read_from still functioning
//! - Drop backpressure channel semantics

use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, ProcessId,
    RegionProt, ResourceClass, ResourceKind, ResourceSelector,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};
use selium_service::DiscoveryRequest;
use selium_shm::{Channel, ChannelBackpressure, transport::ShmTransport};
use selium_wire::{framed::FramedRead, pubsub::Subscriber};

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

#[test]
fn alloc_region_with_log_channel_publishes_discovery_register_events() {
    let (runtime, mut subscriber) = runtime_with_discovery_feed();
    let process_id = spawn_shared_memory_guest(&runtime, "log-channel-guest");

    let region_id = alloc_region(&runtime, process_id, ResourceKind::LogChannel);

    let uris = drain_register_uris(&mut subscriber);

    // Region registrations are typed and tenant-scoped: the guest runs with
    // no tenant (platform), so its region mint under the root tenant.
    assert!(
        uris.contains(&format!("sel:///region/{region_id}")),
        "expected region URI to be published, got: {uris:?}"
    );
    // The runtime SHALL NOT auto-register purpose aliases.
    assert!(
        !uris.iter().any(|uri| uri == "sel:///logs"),
        "no purpose alias shall be published"
    );
}

#[test]
fn deprecated_guest_log_write_still_functions() {
    let runtime = Runtime::default();
    let process_id = spawn_guest(
        &runtime,
        "legacy-log",
        vec![
            CapabilityGrant::new(
                Capability::GuestLogWrite,
                vec![ResourceSelector::ResourceClass(ResourceClass::GuestLog)],
            ),
            CapabilityGrant::new(
                Capability::GuestLogRead,
                vec![ResourceSelector::ResourceClass(ResourceClass::GuestLog)],
            ),
        ],
    );

    // Write a log entry via the legacy hostcall.
    let entry = selium_abi::GuestLogEntry {
        process_id: Some(process_id),
        level: "INFO".to_string(),
        target: "test".to_string(),
        message: "legacy log entry".to_string(),
    };
    let (status, write_op) =
        runtime.begin_hostcall(process_id, HostcallRequest::GuestLogWrite { entry });
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    assert_eq!(
        match runtime.poll_hostcall(process_id, write_op) {
            CompletionState::Ready(output) => output,
            other => panic!("expected ready, got {other:?}"),
        },
        HostcallOutput::Empty
    );

    // Read it back.
    let (status, read_op) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::GuestLogRead {
            cursor: 0,
            process_id: Some(process_id),
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    let HostcallOutput::GuestLogEntries(entries) = (match runtime.poll_hostcall(process_id, read_op)
    {
        CompletionState::Ready(output) => output,
        other => panic!("expected ready, got {other:?}"),
    }) else {
        panic!("expected GuestLogEntries");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, "legacy log entry");
}

/// Drains all currently available Register events from the discovery feed and
/// returns the set of URI strings they contain.
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

/// Drains all currently available Revoke events from the discovery feed and
/// returns the set of URI strings they contain.
fn drain_revoke_uris(
    subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>,
) -> std::collections::HashSet<String> {
    let mut uris = std::collections::HashSet::new();
    loop {
        match subscriber.read_with_tag() {
            Ok((request, _tag)) => {
                if let DiscoveryRequest::Revoke { uri } = request {
                    uris.insert(uri);
                }
            }
            Err(selium_wire::error::Error::BufferEmpty) => break,
            Err(error) => panic!("feed read failed: {error}"),
        }
    }
    uris
}

#[test]
fn drop_backpressure_channel_writer_never_blocks() {
    ensure_heap_provider();
    use selium_abi::ResourceKind;

    let channel =
        Channel::create_with_backpressure(64, ChannelBackpressure::Drop, ResourceKind::LogChannel)
            .expect("create drop channel");

    // Writer should be created successfully.
    let writer = channel.writer().expect("writer");
    assert_eq!(writer.backpressure(), ChannelBackpressure::Drop);

    // blocking_writer should succeed (it protects readers and other blocking writers).
    let _bw = channel
        .blocking_writer()
        .expect("blocking writer on Drop channel");

    // blocking_reader should succeed (writers drop data when it's slow).
    let _br = channel
        .blocking_reader()
        .expect("blocking reader on Drop channel");
}

fn ensure_heap_provider() {
    if selium_memory::region_provider().is_err() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
    }
}

#[test]
fn guest_log_register_end_to_end() {
    let runtime = Runtime::default();
    let process_id = spawn_shared_memory_guest(&runtime, "log-register-e2e");

    // Allocate a region with LogChannel purpose.
    let region_id = alloc_region(&runtime, process_id, ResourceKind::LogChannel);

    // Register it as a log channel.
    let (status, reg_op) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::GuestLogRegister {
            shared_id: region_id,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    assert_eq!(
        (match runtime.poll_hostcall(process_id, reg_op) {
            CompletionState::Ready(output) => output,
            other => panic!("expected ready, got {other:?}"),
        }),
        HostcallOutput::Empty
    );

    // Verify the kernel recorded the log channel.
    assert_eq!(
        runtime
            .kernel()
            .processes()
            .log_channel_shared_id(process_id),
        Some(region_id)
    );
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!("(module (func (export \"{entrypoint}\")))")).expect("compile wat")
}

#[test]
fn park_backpressure_channel_accepts_blocking_writer() {
    ensure_heap_provider();
    use selium_abi::ResourceKind;

    let channel =
        Channel::create_with_backpressure(64, ChannelBackpressure::Park, ResourceKind::RpcRing)
            .expect("create park channel");

    // blocking_writer should succeed.
    let _bw = channel
        .blocking_writer()
        .expect("blocking writer on Park channel");
}

#[test]
fn process_termination_publishes_discovery_revoke_events() {
    let (runtime, mut subscriber) = runtime_with_discovery_feed();
    let process_id = spawn_shared_memory_guest(&runtime, "terminating-guest");

    // Allocate regions to generate discovery URIs.
    let _r1 = alloc_region(&runtime, process_id, ResourceKind::LogChannel);
    let _r2 = alloc_region(&runtime, process_id, ResourceKind::SharedMemory);

    // Drain the Register events published during allocation.
    let registered = drain_register_uris(&mut subscriber);
    assert!(
        !registered.is_empty(),
        "expected Register events to be published before termination"
    );

    // Stop the process — this should publish Revoke operations.
    runtime.stop_process(process_id).expect("stop process");

    let revoked = drain_revoke_uris(&mut subscriber);
    assert!(
        !revoked.is_empty(),
        "expected Revoke events to be published after termination"
    );

    // Every previously registered URI should have a matching revocation.
    for uri in &registered {
        assert!(revoked.contains(uri), "expected revocation for {uri}");
    }
}

/// Creates a runtime with the discovery pub/sub feed enabled and returns a
/// subscriber attached to the feed ring. The runtime installs its own
/// kernel-backed region provider, so this helper does not install the heap
/// provider first.
fn runtime_with_discovery_feed() -> (Runtime, Subscriber<DiscoveryRequest, ShmTransport>) {
    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![],
            domain_table: Vec::new(),
        })
        .expect("bootstrap discovery");
    assert!(report.guests.is_empty());

    let feed_region_id = runtime
        .discovery_feed_region_id()
        .expect("discovery feed region id");
    let channel = Channel::attach(feed_region_id).expect("attach to discovery feed");
    let capacity = channel.ring().capacity();
    let transport = ShmTransport::new(&channel, &channel).expect("feed transport");
    let subscriber = Subscriber::new(FramedRead::new(transport), Some(capacity));
    (runtime, subscriber)
}

fn spawn_guest(runtime: &Runtime, name: &str, grants: Vec<CapabilityGrant>) -> ProcessId {
    runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: name.to_string(),
            module_id: format!("{name}-module"),
            module_bytes: module_with_entrypoint("boot"),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants,
            dependencies: Vec::new(),
            readiness: ReadinessCondition::Immediate,
            tenant: None,
            serving_role: None,
            handlers: Vec::new(),
        })
        .expect("spawn guest")
        .process_id
}

fn spawn_shared_memory_guest(runtime: &Runtime, name: &str) -> ProcessId {
    spawn_guest(
        runtime,
        name,
        vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        )],
    )
}
