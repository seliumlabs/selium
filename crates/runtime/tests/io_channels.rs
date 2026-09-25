//! Integration tests for shared memory channels through the runtime.
//!
//! Tests the `AllocRegion`, `AttachRegion`, `FreeRegion` hostcalls and
//! verifies shared memory communication works end-to-end.

use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, ProcessId,
    RegionProt,
};
use selium_memory::FrameHeader;
use selium_runtime::{ReadinessCondition, Runtime, SystemGuestDescriptor};

/// Tests AllocRegion and FreeRegion lifecycle.
#[test]
fn alloc_and_free_region_lifecycle() {
    let runtime = Runtime::default();
    let process_id = spawn_guest(&runtime, "region-lifecycle");

    let region_id = alloc_region(&runtime, process_id, 1);
    free_region(&runtime, process_id, region_id);
}

/// Allocates a shared region via the `AllocRegion` hostcall.
#[expect(clippy::panic, reason = "test helper")]
fn alloc_region(runtime: &Runtime, process_id: ProcessId, pages: u32) -> u64 {
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::AllocRegion {
            pages,
            prot: RegionProt::ReadWrite,
            purpose: selium_abi::ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) => alloc.region_id,
        other => panic!("expected RegionAlloc, got {other:?}"),
    }
}

/// Tests that protection and reader_slot parameters are correctly accepted by the hostcall
/// and that the region is mapped into guest linear memory with the requested protection.
/// The per-page mprotect enforcement is handled by wasmtiny's `map_shared_region`.
#[test]
fn attach_accepts_protection_and_reader_slot() {
    let runtime = Runtime::default();
    let process_id = spawn_guest(&runtime, "prot-slot");

    let region_id = alloc_region(&runtime, process_id, 1);

    // Attach with specific protection and reader slot - verify hostcall succeeds
    // and returns a non-zero page offset (meaning it was mapped into guest memory).
    let reader_slot = Some(0u32);
    let prot = RegionProt::ReadOnly;
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::AttachRegion {
            region_id,
            reader_slot,
            prot,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    let attachment = match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::RegionAttach(att)) => att,
        other => panic!("expected RegionAttach, got {other:?}"),
    };
    assert_ne!(
        attachment.page_offset, 0,
        "page_offset should be non-zero after mapping"
    );

    free_region(&runtime, process_id, region_id);
}

/// Tests that a region can be allocated, attached, and the pages are
/// accessible through the kernel.
#[test]
fn attach_reads_and_writes_through_kernel() {
    let runtime = Runtime::default();
    let process_id = spawn_guest(&runtime, "attach-rw");

    let region_id = alloc_region(&runtime, process_id, 1);

    // Attach the region via hostcall (returns page_offset).
    let attachment = attach_region(&runtime, process_id, region_id);
    assert_ne!(attachment.page_offset, 0);

    // Use kernel internals to get a local mapping id for verification.
    let memory = runtime.kernel().memory();
    let local_id = memory
        .attach_shared_region(region_id)
        .expect("kernel attach");

    // Write via kernel.
    memory
        .write_shared_memory(local_id, 0, b"hello region")
        .expect("write");

    // Read back via kernel.
    let bytes = memory.read_shared_memory(local_id, 0, 12).expect("read");
    assert_eq!(bytes, b"hello region");

    // Detach the kernel mapping.
    memory.detach_shared_region(local_id).expect("detach");

    free_region(&runtime, process_id, region_id);
}

/// Attaches to a shared region via the `AttachRegion` hostcall.
#[expect(clippy::panic, reason = "test helper")]
fn attach_region(
    runtime: &Runtime,
    process_id: ProcessId,
    region_id: u64,
) -> selium_abi::RegionAttachment {
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::AttachRegion {
            region_id,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::RegionAttach(attachment)) => attachment,
        other => panic!("expected RegionAttach, got {other:?}"),
    }
}

/// Tests the selium-io frame header wire format through kernel shared memory.
#[test]
fn frame_header_round_trip() {
    let runtime = Runtime::default();
    let process_id = spawn_guest(&runtime, "frame");

    let region_id = alloc_region(&runtime, process_id, 1);
    let memory = runtime.kernel().memory();
    let mapping = memory
        .attach_shared_region(region_id)
        .expect("kernel attach");

    let payload = b"hello";
    let header = FrameHeader {
        len: payload.len() as u32,
        tag: 1,
        flags: 0,
    };
    let header_bytes = header.encode();

    // Write header and payload.
    memory
        .write_shared_memory(mapping, 0, &header_bytes)
        .expect("write header");
    memory
        .write_shared_memory(mapping, FrameHeader::ENCODED_SIZE as u64, payload)
        .expect("write payload");

    // Read back header.
    let read_header = memory
        .read_shared_memory(mapping, 0, FrameHeader::ENCODED_SIZE)
        .expect("read header");
    let decoded = FrameHeader::decode(&read_header).expect("valid frame header");
    assert_eq!(decoded.len, 5);
    assert_eq!(decoded.tag, 1);
    assert_eq!(decoded.frame_size(), (FrameHeader::ENCODED_SIZE as u64) + 5);

    memory.detach_shared_region(mapping).expect("detach");
    free_region(&runtime, process_id, region_id);
}

/// Tests that FreeRegion cleans up active kernel mappings and succeeds.
#[test]
fn free_fails_when_mapped() {
    let runtime = Runtime::default();
    let process_id = spawn_guest(&runtime, "free-mapped");

    let region_id = alloc_region(&runtime, process_id, 1);

    // Attach via kernel (creates a mapping).
    let memory = runtime.kernel().memory();
    let local_id = memory.attach_shared_region(region_id).expect("attach");

    // FreeRegion should succeed by cleaning up all kernel mappings first.
    let (_, op_id) = runtime.begin_hostcall(process_id, HostcallRequest::FreeRegion { region_id });
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::Empty) => {} // expected
        other => panic!("expected Ready(Empty), got {other:?}"),
    }

    // Kernel mapping should have been cleaned up — detach should fail.
    let result = memory.detach_shared_region(local_id);
    assert!(
        result.is_err(),
        "detach should fail after FreeRegion cleaned up kernel mappings"
    );
}

/// Frees a shared region via the `FreeRegion` hostcall.
#[expect(clippy::panic, reason = "test helper")]
fn free_region(runtime: &Runtime, process_id: ProcessId, region_id: u64) {
    let (status, op_id) =
        runtime.begin_hostcall(process_id, HostcallRequest::FreeRegion { region_id });
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::Empty) => {}
        other => panic!("expected Empty, got {other:?}"),
    }
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!(
        "(module (memory 1) (func (export \"{entrypoint}\")))"
    ))
    .expect("compile wat")
}

#[expect(clippy::indexing_slicing, reason = "test helper")]
fn spawn_guest(runtime: &Runtime, name: &str) -> ProcessId {
    let report = runtime
        .bootstrap_system_guests(selium_runtime::RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: name.to_string(),
                module_id: format!("{name}-module"),
                module_bytes: module_with_entrypoint("boot"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![selium_abi::ResourceSelector::ResourceClass(
                        selium_abi::ResourceClass::SharedRegion,
                    )],
                )],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            }],
            domain_table: Vec::new(),
        })
        .expect("bootstrap");
    report.guests[0].process_id
}

/// Tests that two attachments to the same region share data.
#[test]
fn two_attachments_share_region() {
    let runtime = Runtime::default();
    let process_id = spawn_guest(&runtime, "two-attach");

    let region_id = alloc_region(&runtime, process_id, 1);

    let memory = runtime.kernel().memory();

    // First attachment via kernel.
    let left = memory.attach_shared_region(region_id).expect("left attach");
    memory
        .write_shared_memory(left, 0, b"shared!")
        .expect("left write");

    // Second attachment via kernel.
    let right = memory
        .attach_shared_region(region_id)
        .expect("right attach");
    let bytes = memory.read_shared_memory(right, 0, 7).expect("right read");
    assert_eq!(bytes, b"shared!");

    memory.detach_shared_region(left).expect("detach left");
    memory.detach_shared_region(right).expect("detach right");

    free_region(&runtime, process_id, region_id);
}
