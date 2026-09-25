//! HTTP connector substrate tests.
//!
//! These tests verify the runtime substrate the HTTP connector runs on, at
//! the hostcall level: host-queue attach via discovery resolve basis,
//! per-connection region handoff with implicit ownership sharing, FIFO
//! delivery for keep-alive ordering, finite ring capacity, and — the
//! capability model's interception guarantee — that a guest holding only a
//! broad shared-memory grant is still denied `AttachRegion` on a
//! connection region it does not own.
//!
//! The HTTP-level behaviour (request parsing, routing, correlation,
//! backpressure pause/resume, chunked streaming) is tested against the
//! real connector pipeline in `selium-connector-http`.
//!
//! ```sh
//! cargo test -p selium-runtime --test http_connector
//! ```

use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, ProcessId,
    RegionProt, ResourceClass, ResourceKind, ResourceSelector,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};

/// Task 4.4: CI — app guest with no Network grants serves successfully;
/// attach attempt by an ungranted guest to a connection region is denied.
///
/// Verifies the capability model:
/// - An app guest holding only channel attach grants (HostQueue + SharedMemory)
///   can create a listener and receive forwarded connections.
/// - A guest without a resolve basis (no discovery recording) is denied
///   attach to the connection region.
#[test]
fn app_guest_without_network_grants() {
    let runtime = Runtime::default();

    // App guest: HostQueue + SharedMemory only. No Network capability.
    let app = spawn_guest(
        &runtime,
        "app-zero-net",
        vec![
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
        ],
    );

    // Connector: SharedMemory + HostQueue (no Network at this level).
    let connector = spawn_guest(
        &runtime,
        "connector-zero-net",
        vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
    );

    // Intruder: HostQueue only, no resolve basis.
    let intruder = spawn_guest(
        &runtime,
        "intruder",
        vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )],
    );

    // Step 1: App creates a listener queue — succeeds with zero Network grants.
    let (_, op_id) = runtime.begin_hostcall(
        app,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(listener)) =
        runtime.poll_hostcall(app, op_id)
    else {
        panic!("app guest should create host queue without Network grant");
    };

    // Step 2: Discovery records the queue for the connector.
    discovery_records_resolve(&runtime, connector, listener.shared_id);

    // Step 3: Connector attaches via resolve basis — succeeds.
    let (status, op_id) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: listener.shared_id,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "connector with resolve basis must be allowed to attach"
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(connector_queue)) =
        runtime.poll_hostcall(connector, op_id)
    else {
        panic!("expected successful attach");
    };

    // Step 4: Connector allocates a region and sends its id through the queue.
    let (_, alloc_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::RpcRing,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) =
        runtime.poll_hostcall(connector, alloc_op)
    else {
        panic!("expected region allocation");
    };

    // Send the region id through the queue to the app guest.
    let (send_status, _send_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueSend {
            local_id: connector_queue.local_id,
            value: alloc.region_id,
            metadata: Vec::new(),
        },
    );
    assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

    // Step 5: App receives and attaches to the region — zero Network grants,
    // only SharedMemory + HostQueue used.
    let (recv_status, recv_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::HostQueueRecv {
            local_id: listener.local_id,
        },
    );
    let received_region_id = match recv_status {
        selium_abi::HOSTCALL_STATUS_READY => match runtime.poll_hostcall(app, recv_op) {
            CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) => value,
            other => panic!("expected ConnectionInfo, got {other:?}"),
        },
        _ => match runtime.poll_hostcall(app, recv_op) {
            CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) => value,
            other => panic!("expected ConnectionInfo, got {other:?}"),
        },
    };
    assert_eq!(received_region_id, alloc.region_id);

    let (attach_status, attach_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::AttachRegion {
            region_id: received_region_id,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    assert_eq!(attach_status, selium_abi::HOSTCALL_STATUS_READY);
    let CompletionState::Ready(HostcallOutput::RegionAttach(_)) =
        runtime.poll_hostcall(app, attach_op)
    else {
        panic!("app should attach to region without Network grant");
    };

    // Step 6: Intruder (no queue basis, no grant for this region) is denied.
    let (deny_status, deny_op) = runtime.begin_hostcall(
        intruder,
        HostcallRequest::HostQueueAttach {
            shared_id: listener.shared_id,
        },
    );
    assert_eq!(
        deny_status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "intruder without queue basis must be denied attach"
    );
    assert!(matches!(
        runtime.poll_hostcall(intruder, deny_op),
        CompletionState::Failed(_)
    ));

    // Cleanup. The handoff transferred ownership to the app, so the app
    // frees the region (the connector no longer can).
    let (_, free_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::FreeRegion {
            region_id: alloc.region_id,
        },
    );
    assert!(matches!(
        runtime.poll_hostcall(app, free_op),
        CompletionState::Ready(_)
    ));
}

fn discovery_records_resolve(runtime: &Runtime, client: ProcessId, shared_id: u64) {
    let discovery = spawn_guest(runtime, "discovery", Vec::new());
    let (status, _op) = runtime.begin_hostcall(
        discovery,
        HostcallRequest::RecordResolvedQueueFor {
            client_process_id: client,
            shared_id,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "discovery service must be able to record resolve results"
    );
}

/// Task 4.3 substrate layer: a full Park ring parks writers until the
/// reader drains; no frames are lost.
///
/// This is the channel mechanism the connector's backpressure honesty is
/// built on: the session rings use `ChannelBackpressure::Park`, so when a
/// serving channel's ring is full the writing side parks rather than
/// buffering without bound. The connector-level behaviour this produces —
/// pausing socket reads until capacity frees — is asserted in
/// `selium-connector-http`'s pipeline tests.
#[tokio::test]
async fn full_ring_parks_writes_until_consumed() {
    drop(selium_memory::set_region_provider(Box::new(
        selium_memory::HeapRegionProvider::new(),
    )));

    // A small Park ring: 128 bytes of data area.
    let capacity: u64 = 128;
    let channel = selium_shm::Channel::create_with_backpressure(
        capacity,
        selium_shm::ChannelBackpressure::Park,
        selium_abi::ResourceKind::SharedMemory,
    )
    .expect("create park channel");
    let dummy = selium_shm::Channel::create_with_backpressure(
        64,
        selium_shm::ChannelBackpressure::Park,
        selium_abi::ResourceKind::SharedMemory,
    )
    .expect("create dummy channel");

    let writer_transport =
        selium_shm::ShmTransport::new(&dummy, &channel).expect("writer transport");
    let reader_transport =
        selium_shm::ShmTransport::new(&channel, &dummy).expect("reader transport");

    let mut writer = selium_wire::FramedWrite::new(writer_transport);
    let mut reader = selium_wire::FramedRead::new(reader_transport);

    let payload = [0xABu8; 16];
    let frame_size = selium_memory::FrameHeader::ENCODED_SIZE as u64 + payload.len() as u64;
    let max_frames = (capacity / frame_size) as u32;

    // Fill the ring: exactly `max_frames` frames fit.
    for tag in 0..max_frames {
        writer
            .write_frame_with_flags_async(&payload, tag, selium_memory::FrameHeader::FLAG_READY)
            .await
            .expect("write within capacity");
    }

    // The next write must park: the ring is full and nobody is reading.
    let mut write_handle = tokio::spawn(async move {
        writer
            .write_frame_with_flags_async(
                &payload,
                max_frames,
                selium_memory::FrameHeader::FLAG_READY,
            )
            .await
    });
    let parked = tokio::time::timeout(std::time::Duration::from_millis(100), &mut write_handle)
        .await
        .is_err();
    assert!(
        parked,
        "a write past ring capacity must park, not complete or overflow"
    );

    // Consume one frame: capacity frees, and the parked write resumes.
    let (frame, tag, flags) = reader.read_frame().expect("read first frame");
    assert_eq!(frame, payload);
    assert_eq!(tag, 0);
    assert_ne!(flags & selium_memory::FrameHeader::FLAG_READY, 0);

    let resumed = tokio::time::timeout(std::time::Duration::from_secs(5), &mut write_handle)
        .await
        .expect("parked write must resume once the reader drains capacity");
    assert!(resumed.is_ok(), "resumed write must succeed");

    // Drain everything: all frames arrive intact and in tag order —
    // backpressure delayed the writer but lost nothing.
    let mut tags = vec![0u32];
    for expected in 1..=max_frames {
        loop {
            match reader.read_frame() {
                Ok((frame, tag, _)) => {
                    assert_eq!(frame, payload);
                    assert_eq!(tag, expected);
                    tags.push(tag);
                    break;
                }
                Err(selium_wire::Error::BufferEmpty) => {
                    tokio::task::yield_now().await;
                }
                Err(e) => panic!("unexpected read error: {e}"),
            }
        }
    }
    let expected_tags: Vec<u32> = (0..=max_frames).collect();
    assert_eq!(tags, expected_tags, "no frames may be lost to backpressure");
}

/// Task 4.1 substrate layer: the connector ↔ app-guest channel handoff.
///
/// The HTTP-level golden path (bytes → typed forward → typed response) is
/// covered by `selium-connector-http`'s pipeline tests. This test verifies
/// the runtime substrate that path runs on:
/// 1. App guest creates a listener host queue.
/// 2. Discovery records the queue for the connector.
/// 3. Connector attaches, allocates an RPC ring region, and sends the
///    region id through the queue.
/// 4. App guest receives the region id and attaches to the region.
/// 5. Both guests communicate through the shared region.
#[test]
fn http_connector_golden_path() {
    let runtime = Runtime::default();

    // App guest: holds HostQueue + SharedMemory grants. Zero Network grants
    // — networking is handled by the connector.
    let app = spawn_guest(
        &runtime,
        "app-guest",
        vec![
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
        ],
    );

    // Connector guest: holds SharedMemory + HostQueue grants (no Network
    // grant at this level — the runtime kernel provides the network).
    let connector = spawn_guest(
        &runtime,
        "connector",
        vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
    );

    // Step 1: App creates a listener host queue.
    let (_, op_id) = runtime.begin_hostcall(
        app,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(listener)) =
        runtime.poll_hostcall(app, op_id)
    else {
        panic!("expected host queue");
    };

    // Step 2: Discovery records the queue for the connector.
    discovery_records_resolve(&runtime, connector, listener.shared_id);

    // Step 3: Connector attaches to the queue via resolve basis.
    let (attach_status, op_id) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: listener.shared_id,
        },
    );
    assert_eq!(
        attach_status,
        selium_abi::HOSTCALL_STATUS_READY,
        "connector with resolve basis must attach"
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(connector_queue)) =
        runtime.poll_hostcall(connector, op_id)
    else {
        panic!("connector should attach via resolve basis");
    };

    // Step 4: Connector allocates an RPC ring region.
    let (_, alloc_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::RpcRing,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) =
        runtime.poll_hostcall(connector, alloc_op)
    else {
        panic!("expected region allocation");
    };

    // Step 5: Connector sends the region id through the queue.
    let (send_status, _send_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueSend {
            local_id: connector_queue.local_id,
            value: alloc.region_id,
            metadata: Vec::new(),
        },
    );
    assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

    // Step 6: App receives the region id.
    let (recv_status, recv_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::HostQueueRecv {
            local_id: listener.local_id,
        },
    );
    let received_region_id = match recv_status {
        selium_abi::HOSTCALL_STATUS_READY => match runtime.poll_hostcall(app, recv_op) {
            CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) => value,
            other => panic!("expected ConnectionInfo, got {other:?}"),
        },
        _ => match runtime.poll_hostcall(app, recv_op) {
            CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) => value,
            other => panic!("expected ConnectionInfo, got {other:?}"),
        },
    };
    assert_eq!(received_region_id, alloc.region_id);

    // Step 7: App attaches to the region.
    let (attach_status, attach_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::AttachRegion {
            region_id: received_region_id,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    assert_eq!(attach_status, selium_abi::HOSTCALL_STATUS_READY);
    let CompletionState::Ready(HostcallOutput::RegionAttach(_)) =
        runtime.poll_hostcall(app, attach_op)
    else {
        panic!("app should attach to the region");
    };

    // Step 8: Verify the region is shared — both guests can access it.
    let memory = runtime.kernel().memory();
    let conn_mapping = memory
        .attach_shared_region(alloc.region_id)
        .expect("connector kernel attach");

    memory
        .write_shared_memory(conn_mapping, 0, b"hello from http")
        .expect("write");

    let app_mapping = memory
        .attach_shared_region(alloc.region_id)
        .expect("app kernel attach");
    let bytes = memory.read_shared_memory(app_mapping, 0, 15).expect("read");
    assert_eq!(bytes, b"hello from http");

    memory.detach_shared_region(conn_mapping).expect("detach");
    memory.detach_shared_region(app_mapping).expect("detach");

    // Cleanup. The handoff transferred ownership to the app, so the app
    // frees the region (the connector no longer can).
    let (_, free_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::FreeRegion {
            region_id: alloc.region_id,
        },
    );
    assert!(matches!(
        runtime.poll_hostcall(app, free_op),
        CompletionState::Ready(_)
    ));
}

/// Task 4.2 substrate layer: ordered delivery underpins keep-alive
/// correlation.
///
/// Multiple items sent through a host queue must be received in FIFO
/// order. This is the delivery guarantee the connector's in-flight
/// correlation map relies on to emit responses in request order; the
/// wire-level ordering and distinct-tag behaviour is asserted in
/// `selium-connector-http`'s pipeline tests.
#[test]
fn keep_alive_ordering_preserved() {
    let runtime = Runtime::default();

    let owner = spawn_guest(
        &runtime,
        "owner",
        vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )],
    );

    let receiver = spawn_guest(
        &runtime,
        "receiver",
        vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )],
    );

    // Owner creates a queue.
    let (_, op_id) = runtime.begin_hostcall(
        owner,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
        runtime.poll_hostcall(owner, op_id)
    else {
        panic!("expected host queue");
    };

    // Receiver resolves and attaches.
    discovery_records_resolve(&runtime, receiver, queue.shared_id);
    let (_, op_id) = runtime.begin_hostcall(
        receiver,
        HostcallRequest::HostQueueAttach {
            shared_id: queue.shared_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(recv_queue)) =
        runtime.poll_hostcall(receiver, op_id)
    else {
        panic!("receiver should attach");
    };

    // Send 5 values in order. These represent consecutive keep-alive
    // requests on a single connection.
    for value in [100u64, 200, 300, 400, 500] {
        let (status, _) = runtime.begin_hostcall(
            owner,
            HostcallRequest::HostQueueSend {
                local_id: queue.local_id,
                value,
                metadata: Vec::new(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    }

    // Receive all 5 values — they must arrive in FIFO order.
    for expected in [100u64, 200, 300, 400, 500] {
        let (status, op_id) = runtime.begin_hostcall(
            receiver,
            HostcallRequest::HostQueueRecv {
                local_id: recv_queue.local_id,
            },
        );
        let value = match status {
            selium_abi::HOSTCALL_STATUS_READY => match runtime.poll_hostcall(receiver, op_id) {
                CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) => value,
                other => panic!("expected ConnectionInfo, got {other:?}"),
            },
            _ => match runtime.poll_hostcall(receiver, op_id) {
                CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) => value,
                other => panic!("expected ConnectionInfo, got {other:?}"),
            },
        };
        assert_eq!(
            value, expected,
            "keep-alive ordering violated: expected {expected}, got {value}"
        );
    }
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!(
        "(module (memory 1) (func (export \"{entrypoint}\")))"
    ))
    .expect("compile wat")
}

fn spawn_guest(runtime: &Runtime, name: &str, grants: Vec<CapabilityGrant>) -> ProcessId {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
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
            }],
            domain_table: Vec::new(),
        })
        .expect("bootstrap guest");
    report
        .guests
        .first()
        .expect("bootstrap report contains the requested guest")
        .process_id
}

/// Task 4.4: a guest without a grant for a connection region is denied
/// `AttachRegion` on it.
///
/// The intruder here holds a *broad* shared-memory capability (class-level
/// `ResourceClass::SharedRegion` selector — the anti-pattern the spec
/// calls out), so the coarse capability check passes. Denial must come
/// from the region-level authorisation: the intruder neither owns the
/// connection region (it was not the allocator and received no queue
/// handoff) nor holds an `ExplicitResource` grant for it. That is the
/// guarantee that keeps connector-served channels private to exactly
/// {connector, app guest}.
#[test]
fn ungranted_region_attach_denied() {
    let runtime = Runtime::default();

    let app = spawn_guest(
        &runtime,
        "app-owner",
        vec![
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
        ],
    );

    let connector = spawn_guest(
        &runtime,
        "connector-owner",
        vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
    );

    // Intruder: broad shared-memory class grant, but no ownership of and
    // no ExplicitResource grant for the connection region.
    let intruder = spawn_guest(
        &runtime,
        "intruder",
        vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        )],
    );

    // The connector allocates a connection region.
    let (_, alloc_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::RpcRing,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) =
        runtime.poll_hostcall(connector, alloc_op)
    else {
        panic!("expected region allocation");
    };

    // Handoff: the region id is delivered to the app through the queue,
    // which shares ownership with the receiver (and only the receiver).
    let (_, queue_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
        runtime.poll_hostcall(app, queue_op)
    else {
        panic!("expected host queue");
    };
    discovery_records_resolve(&runtime, connector, queue.shared_id);
    let (_, attach_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: queue.shared_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(connector_queue)) =
        runtime.poll_hostcall(connector, attach_op)
    else {
        panic!("connector should attach via resolve basis");
    };
    let (send_status, _) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueSend {
            local_id: connector_queue.local_id,
            value: alloc.region_id,
            metadata: Vec::new(),
        },
    );
    assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

    // The app receives the region id (implicit ownership sharing) and
    // attaches successfully.
    let (_, recv_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::HostQueueRecv {
            local_id: queue.local_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) =
        runtime.poll_hostcall(app, recv_op)
    else {
        panic!("expected ConnectionInfo");
    };
    assert_eq!(value, alloc.region_id);
    let (attach_status, attach_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::AttachRegion {
            region_id: value,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    assert_eq!(attach_status, selium_abi::HOSTCALL_STATUS_READY);
    assert!(matches!(
        runtime.poll_hostcall(app, attach_op),
        CompletionState::Ready(HostcallOutput::RegionAttach(_))
    ));

    // The intruder — despite its broad class-level shared-memory grant —
    // is denied attach to the connection region: no ownership, no
    // ExplicitResource grant for this region.
    let (deny_status, deny_op) = runtime.begin_hostcall(
        intruder,
        HostcallRequest::AttachRegion {
            region_id: alloc.region_id,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    assert_eq!(
        deny_status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "ungranted attach to a connection region must be denied"
    );
    assert!(
        matches!(
            runtime.poll_hostcall(intruder, deny_op),
            CompletionState::Failed(_)
        ),
        "the attach operation must fail, not defer or succeed"
    );

    // Cleanup. The handoff transferred ownership to the app, so the app
    // frees the region (the connector no longer can).
    let (_, free_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::FreeRegion {
            region_id: alloc.region_id,
        },
    );
    assert!(matches!(
        runtime.poll_hostcall(app, free_op),
        CompletionState::Ready(_)
    ));
}
