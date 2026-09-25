//! Host-queue handoff metadata substrate tests.
//!
//! Verify the `resource-handshake` delta: `HostQueueSend` optionally carries an
//! opaque metadata payload, and the corresponding `HostQueueRecv` surfaces it
//! in `HostcallOutput::ConnectionInfo.metadata` (which `selium-guest` exposes
//! as `IncomingConnection.metadata`). Sending without metadata yields an empty
//! value at the receiver.
//!
//! ```sh
//! cargo test -p selium-runtime --test handoff_metadata
//! ```

use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, ResourceClass,
    ResourceSelector,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};

fn backpressure_queue_grants() -> Vec<CapabilityGrant> {
    vec![
        CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        ),
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        ),
    ]
}

fn discovery_records_resolve(runtime: &Runtime, client: selium_abi::ProcessId, shared_id: u64) {
    let discovery = spawn_guest(runtime, "discovery", Vec::new());
    let (status, _op) = runtime.begin_hostcall(
        discovery,
        HostcallRequest::RecordResolvedQueueFor {
            client_process_id: client,
            shared_id,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
}

/// A sender enqueues a connection with an opaque metadata payload; the
/// receiver's `ConnectionInfo` surfaces exactly that payload.
#[test]
fn handoff_metadata_delivered() {
    let runtime = Runtime::default();

    let sender = spawn_guest(&runtime, "meta-sender", backpressure_queue_grants());
    let receiver = spawn_guest(&runtime, "meta-receiver", backpressure_queue_grants());

    let (_, op_id) = runtime.begin_hostcall(
        sender,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
        runtime.poll_hostcall(sender, op_id)
    else {
        panic!("sender should create its listener queue");
    };

    // The receiver attaches to the shared queue to obtain its own handle.
    discovery_records_resolve(&runtime, receiver, queue.shared_id);
    let (_, attach_op) = runtime.begin_hostcall(
        receiver,
        HostcallRequest::HostQueueAttach {
            shared_id: queue.shared_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(receiver_queue)) =
        runtime.poll_hostcall(receiver, attach_op)
    else {
        panic!("receiver should attach to the shared queue");
    };

    let metadata: Vec<u8> = b"tenant=acme,fingerprint=abcdef".to_vec();
    let (send_status, _) = runtime.begin_hostcall(
        sender,
        HostcallRequest::HostQueueSend {
            local_id: queue.local_id,
            value: 42,
            metadata: metadata.clone(),
        },
    );
    assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

    let (_, recv_op) = runtime.begin_hostcall(
        receiver,
        HostcallRequest::HostQueueRecv {
            local_id: receiver_queue.local_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::ConnectionInfo {
        client_process_id,
        value,
        metadata: received,
    }) = runtime.poll_hostcall(receiver, recv_op)
    else {
        panic!("receiver should receive the delivered handoff");
    };

    assert_eq!(client_process_id, sender);
    assert_eq!(value, 42);
    assert_eq!(received, metadata, "metadata must survive the handoff");
}

/// A sender enqueues a connection without metadata; the receiver sees an
/// empty metadata value.
#[test]
fn handoff_no_metadata_yields_empty() {
    let runtime = Runtime::default();

    let sender = spawn_guest(&runtime, "meta-sender-empty", backpressure_queue_grants());
    let receiver = spawn_guest(&runtime, "meta-receiver-empty", backpressure_queue_grants());

    let (_, op_id) = runtime.begin_hostcall(
        sender,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
        runtime.poll_hostcall(sender, op_id)
    else {
        panic!("sender should create its listener queue");
    };

    // The receiver attaches to the shared queue to obtain its own handle.
    discovery_records_resolve(&runtime, receiver, queue.shared_id);
    let (_, attach_op) = runtime.begin_hostcall(
        receiver,
        HostcallRequest::HostQueueAttach {
            shared_id: queue.shared_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(receiver_queue)) =
        runtime.poll_hostcall(receiver, attach_op)
    else {
        panic!("receiver should attach to the shared queue");
    };

    let (send_status, _) = runtime.begin_hostcall(
        sender,
        HostcallRequest::HostQueueSend {
            local_id: queue.local_id,
            value: 99,
            metadata: Vec::new(),
        },
    );
    assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

    let (_, recv_op) = runtime.begin_hostcall(
        receiver,
        HostcallRequest::HostQueueRecv {
            local_id: receiver_queue.local_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::ConnectionInfo { metadata, .. }) =
        runtime.poll_hostcall(receiver, recv_op)
    else {
        panic!("receiver should receive the delivered handoff");
    };

    assert!(metadata.is_empty(), "absent metadata must surface as empty");
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!(
        "(module (memory 1) (func (export \"{entrypoint}\")))"
    ))
    .expect("compile wat")
}

fn spawn_guest(
    runtime: &Runtime,
    name: &str,
    grants: Vec<CapabilityGrant>,
) -> selium_abi::ProcessId {
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
