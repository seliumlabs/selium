//! QUIC connector substrate tests.
//!
//! These tests verify the runtime substrate the QUIC connector's per-stream
//! byte channels run on, at the hostcall level: the golden-path channel
//! handoff (host queue → resolve basis → region allocation → delivery →
//! attach), per-stream channel isolation, and the capability model's
//! interception guarantee. The QUIC-level behaviour (handshake, SNI routing,
//! relay, backpressure, lifecycle) is tested in `selium-connector-quic`.
//!
//! ```sh
//! cargo test -p selium-runtime --test quic_connector
//! ```

use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, ProcessId,
    RegionProt, ResourceClass, ResourceKind, ResourceSelector,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};

fn discovery_records_resolve(runtime: &Runtime, client: ProcessId, shared_id: u64) {
    let discovery = spawn_guest(runtime, "discovery", Vec::new());
    let (status, _) = runtime.begin_hostcall(
        discovery,
        HostcallRequest::RecordResolvedQueueFor {
            client_process_id: client,
            shared_id,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "discovery service must record resolve results"
    );
}

/// Task 6.2 substrate layer: a full Park ring parks writers (no loss), the
/// channel mechanism the connector's backpressure honesty is built on.
#[tokio::test]
async fn full_ring_parks_writes_until_consumed() {
    drop(selium_memory::set_region_provider(Box::new(
        selium_memory::HeapRegionProvider::new(),
    )));

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

    for tag in 0..max_frames {
        writer
            .write_frame_with_flags_async(&payload, tag, selium_memory::FrameHeader::FLAG_READY)
            .await
            .expect("write within capacity");
    }

    // A write past capacity must park, not overflow.
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
    assert!(parked, "a write past ring capacity must park, not overflow");

    let (frame, tag, flags) = reader.read_frame().expect("read first frame");
    assert_eq!(frame, payload);
    assert_eq!(tag, 0);
    assert_ne!(flags & selium_memory::FrameHeader::FLAG_READY, 0);

    let resumed = tokio::time::timeout(std::time::Duration::from_secs(5), &mut write_handle)
        .await
        .expect("parked write must resume once capacity frees");
    assert!(resumed.is_ok(), "resumed write must succeed");

    // Drain everything: no bytes lost to backpressure.
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
    assert_eq!(tags, (0..=max_frames).collect::<Vec<u32>>());
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!(
        "(module (memory 1) (func (export \"{entrypoint}\")))"
    ))
    .expect("compile wat")
}

/// Task 6.1 substrate layer: the connector ↔ app-guest channel handoff.
///
/// Mirrors the HTTP connector golden path: the app guest creates a listener
/// queue, discovery records the resolve basis, the connector attaches and
/// allocates a channel region, delivers the `shared_id` through the queue,
/// and the app guest attaches to it. This is the per-stream byte-channel
/// handoff the QUIC connector performs once per accepted bidirectional stream.
#[test]
fn quic_channel_handoff_golden_path() {
    let runtime = Runtime::default();

    let app = spawn_guest(
        &runtime,
        "quic-app",
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
        "quic-connector",
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

    // 1. App creates its listener queue.
    let (_, op_id) = runtime.begin_hostcall(
        app,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(listener)) =
        runtime.poll_hostcall(app, op_id)
    else {
        panic!("app guest should create its listener queue");
    };

    // 2. Discovery records the resolve basis for the connector.
    discovery_records_resolve(&runtime, connector, listener.shared_id);

    // 3. Connector attaches to the queue via the resolve basis.
    let (_, op_id) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: listener.shared_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(connector_queue)) =
        runtime.poll_hostcall(connector, op_id)
    else {
        panic!("connector with resolve basis must attach");
    };

    // 4. Connector allocates a per-stream channel region.
    let (_, alloc_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) =
        runtime.poll_hostcall(connector, alloc_op)
    else {
        panic!("expected region allocation");
    };

    // 5. Connector delivers the region id through the queue.
    let (send_status, _) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueSend {
            local_id: connector_queue.local_id,
            value: alloc.region_id,
            metadata: Vec::new(),
        },
    );
    assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

    // 6. App receives the region id and attaches.
    let (_, recv_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::HostQueueRecv {
            local_id: listener.local_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) =
        runtime.poll_hostcall(app, recv_op)
    else {
        panic!("app should receive the delivered region id");
    };
    assert_eq!(value, alloc.region_id);

    let (_, attach_op) = runtime.begin_hostcall(
        app,
        HostcallRequest::AttachRegion {
            region_id: value,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    assert!(matches!(
        runtime.poll_hostcall(app, attach_op),
        CompletionState::Ready(HostcallOutput::RegionAttach(_))
    ));

    // 7. The region is shared between connector and app guest.
    let memory = runtime.kernel().memory();
    let conn_mapping = memory
        .attach_shared_region(alloc.region_id)
        .expect("connector attach");
    memory
        .write_shared_memory(conn_mapping, 0, b"quic stream bytes")
        .expect("write");
    let app_mapping = memory
        .attach_shared_region(alloc.region_id)
        .expect("app attach");
    let bytes = memory.read_shared_memory(app_mapping, 0, 17).expect("read");
    assert_eq!(bytes, b"quic stream bytes");

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

/// Task 6.3 substrate layer: concurrent streams become distinct regions, and
/// an ungranted third party is denied attach to a stream region.
///
/// Each accepted QUIC stream is a separate allocation, so bytes on one stream
/// cannot be delivered on another stream's region. An intruder — even one
/// holding a broad shared-memory class grant — is denied attach to a stream
/// region it neither allocated nor received through a queue handoff.
#[test]
fn quic_stream_isolation_and_ungranted_attach_denied() {
    let runtime = Runtime::default();

    let app = spawn_guest(
        &runtime,
        "quic-app",
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
        "quic-connector",
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

    let intruder = spawn_guest(
        &runtime,
        "intruder",
        vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        )],
    );

    // The connector allocates two distinct per-stream regions.
    let (_, alloc_a) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(region_a)) =
        runtime.poll_hostcall(connector, alloc_a)
    else {
        panic!("expected first allocation");
    };

    let (_, alloc_b) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(region_b)) =
        runtime.poll_hostcall(connector, alloc_b)
    else {
        panic!("expected second allocation");
    };

    // Concurrent streams are distinct regions.
    assert_ne!(
        region_a.region_id, region_b.region_id,
        "each stream must have its own region"
    );

    // The ungranted third party is denied attach to a stream region.
    let (deny_status, deny_op) = runtime.begin_hostcall(
        intruder,
        HostcallRequest::AttachRegion {
            region_id: region_a.region_id,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    assert_eq!(
        deny_status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "ungranted attach to a stream region must be denied"
    );
    assert!(matches!(
        runtime.poll_hostcall(intruder, deny_op),
        CompletionState::Failed(_)
    ));

    // Cleanup.
    for region_id in [region_a.region_id, region_b.region_id] {
        let (_, free_op) =
            runtime.begin_hostcall(connector, HostcallRequest::FreeRegion { region_id });
        assert!(matches!(
            runtime.poll_hostcall(connector, free_op),
            CompletionState::Ready(_)
        ));
    }

    let _ = app;
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
