//! Hostile hostcall-envelope handling across the WASM boundary.
//!
//! A guest can declare any `(ptr, len)` pair for `hostcall_create`. The host
//! must bound the declaration before allocating a decode buffer and return a
//! failed operation (or a zero id at the operation bound) without trapping.

use selium_abi::{
    HOSTCALL_STATUS_FAILED, HOSTCALL_STATUS_READY, HostcallEnvelope, HostcallRequest, decode_rkyv,
    encode_hostcall_request, encode_rkyv, hostcall_request_payload,
};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestDescriptor};

/// Appends the passive ABI declaration consumed by the runtime's byte probe.
fn with_abi_version(mut module: Vec<u8>, version: u32) -> Vec<u8> {
    let name = selium_abi::SELIUM_ABI_VERSION_SECTION.as_bytes();
    module.push(0); // custom section
    module.push((1 + name.len() + std::mem::size_of::<u32>()) as u8);
    module.push(name.len() as u8);
    module.extend_from_slice(name);
    module.extend_from_slice(&version.to_le_bytes());
    module
}

/// A guest that submits an oversized length and an out-of-range pointer, then
/// traps unless the host rejects both with a nonzero operation id (the guest
/// can poll it for the structured reason).
fn hostile_envelope_module() -> Vec<u8> {
    let module = wat::parse_str(
        r#"
        (module
          (import "selium" "hostcall_create"
            (func $create (param i32 i32) (result i64)))
          (memory (export "memory") 1)
          ;; A rejection names a full-width operation the guest can poll for
          ;; the structured reason. Only an exhausted operation budget returns
          ;; zero without an operation.
          (func $expect_rejected (param $operation i64)
            (if (i64.eqz (local.get $operation))
              (then (unreachable))))
          (func (export "boot")
            (local $status i64)
            ;; u32::MAX bytes: above HOSTCALL_ENVELOPE_MAX_BYTES.
            (local.set $status (call $create (i32.const 0) (i32.const -1)))
            (call $expect_rejected (local.get $status))
            ;; A pointer near the end of one page with a length past it.
            (local.set $status (call $create (i32.const 65530) (i32.const 100)))
            (call $expect_rejected (local.get $status))
            ;; A readable but preamble-less request must also fail before
            ;; the host attempts rkyv enum dispatch.
            (local.set $status (call $create (i32.const 0) (i32.const 0)))
            (call $expect_rejected (local.get $status))
          )
        )
        "#,
    )
    .expect("compile hostile-envelope module");
    with_abi_version(module, selium_abi::SELIUM_ABI_VERSION)
}

fn wat_data(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("\\{byte:02x}")).collect()
}

/// A legacy guest that embeds a valid pre-preamble rkyv envelope.
fn legacy_guest_module() -> Vec<u8> {
    let envelope = HostcallEnvelope {
        request: HostcallRequest::SelfInfo,
        task_id: None,
    };
    let legacy = encode_rkyv(&envelope).expect("encode legacy envelope");
    wat::parse_str(format!(
        r#"
        (module
          (import "selium" "hostcall_create"
            (func $create (param i32 i32) (result i64)))
          (memory (export "memory") 1)
          (data (i32.const 0) "{data}")
          (func (export "boot")
            (local $status i64)
            (local.set $status (call $create (i32.const 0) (i32.const {len})))
            (if (i64.ne
                  (i64.shr_u (local.get $status) (i64.const 32))
                  (i64.const {failed}))
              (then (unreachable)))
            (if (i64.eqz (i64.and (local.get $status) (i64.const {mask})))
              (then (unreachable))))
        )
        "#,
        data = wat_data(&legacy),
        len = legacy.len(),
        failed = HOSTCALL_STATUS_FAILED as u64,
        mask = u32::MAX as u64,
    ))
    .expect("compile legacy guest module")
}

// scenario: selium-runtime/hostcall-dispatch-is-total-over-untrusted-input/malformed-extreme-inputs
// scenario: selium-abi/hostcall-envelope-has-a-bounded-representation/declared-length-exceeds-guest-memory
// scenario: selium-abi/hostcall-envelope-has-a-bounded-representation/declared-length-exceeds-protocol-maximum
#[test]
fn hostile_envelope_lengths_are_rejected_without_bringing_down_the_host() {
    let runtime = Runtime::default();
    let guest = runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: "hostile-envelope".to_string(),
            module_id: "hostile-envelope-module".to_string(),
            module_bytes: hostile_envelope_module(),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: Vec::new(),
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        })
        .expect("the host must survive hostile envelope declarations");

    // The runtime is still able to dispatch ordinary requests afterwards, so
    // no hostile declaration consumed the process or its memory budget.
    let (status, _operation) =
        runtime.begin_hostcall(guest.process_id, HostcallRequest::RandomBytes { len: 16 });
    assert_eq!(status, HOSTCALL_STATUS_READY);

    // Bootstrap under the same runtime still works after the hostile guest.
    let healthy = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: "healthy".to_string(),
                module_id: "healthy-module".to_string(),
                module_bytes: wat::parse_str("(module (memory 1) (func (export \"boot\")))")
                    .expect("compile healthy module"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            ..RuntimeConfig::default()
        })
        .expect("ordinary guests still bootstrap");
    assert_eq!(healthy.guests.len(), 1);
}

/// Old guest / new host: without module-level negotiation, bootstrap fails
/// before the pre-preamble request can be dispatched.
#[test]
fn legacy_guest_without_a_preamble_is_rejected() {
    let runtime = Runtime::default();
    let error = runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: "legacy-guest".to_string(),
            module_id: "legacy-guest-module".to_string(),
            module_bytes: legacy_guest_module(),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: Vec::new(),
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        })
        .expect_err("a legacy module must fail bootstrap before its first request");
    assert!(
        error.to_string().contains("does not advertise ABI"),
        "unexpected bootstrap error: {error}"
    );
    assert_eq!(runtime.loaded_guest_count(), 0);
}

fn negotiated_guest_module(version: u32) -> Vec<u8> {
    let module = wat::parse_str(
        r#"(module
            (import "selium" "hostcall_create"
              (func $create (param i32 i32) (result i64)))
            (memory 1)
            ;; Reaching this entrypoint proves negotiation accepted the module.
            (func (export "boot")))"#,
    )
    .expect("compile negotiated guest");
    with_abi_version(module, version)
}

fn descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "negotiated".to_string(),
        module_id: "negotiated-module".to_string(),
        module_bytes,
        entrypoint: "boot".to_string(),
        arguments: Vec::new(),
        grants: Vec::new(),
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

#[test]
fn older_and_newer_module_abi_versions_fail_before_the_entrypoint() {
    for version in [
        selium_abi::SELIUM_ABI_VERSION - 1,
        selium_abi::SELIUM_ABI_VERSION + 1,
    ] {
        let runtime = Runtime::default();
        let error = runtime
            .spawn_system_guest(descriptor(negotiated_guest_module(version)))
            .expect_err("an incompatible module must fail bootstrap");
        let message = error.to_string();
        assert!(
            message.contains(&selium_abi::SELIUM_ABI_VERSION.to_string())
                && message.contains(&version.to_string()),
            "the mismatch must name both versions: {message}"
        );
        assert_eq!(runtime.loaded_guest_count(), 0);
    }
}

#[test]
fn matching_module_abi_negotiates_before_requests() {
    let runtime = Runtime::default();
    let guest = runtime
        .spawn_system_guest(descriptor(negotiated_guest_module(
            selium_abi::SELIUM_ABI_VERSION,
        )))
        .expect("matching module bootstraps");
    let (status, _operation) =
        runtime.begin_hostcall(guest.process_id, HostcallRequest::RandomBytes { len: 16 });
    assert_eq!(status, HOSTCALL_STATUS_READY);
}

#[test]
fn an_abi_version_export_cannot_execute_a_hostcall_during_negotiation() {
    // This export is deliberately hostile: the old executable negotiation
    // would call it and register a rejected operation before reading its
    // return value. Passive negotiation must never invoke it.
    let module = wat::parse_str(format!(
        r#"(module
            (import "selium" "hostcall_create"
              (func $create (param i32 i32) (result i64)))
            (memory 1)
            (func (export "__selium_abi_version") (result i64)
              (drop (call $create (i32.const 0) (i32.const 0)))
              i64.const {version})
            (func (export "boot")))"#,
        version = selium_abi::SELIUM_ABI_VERSION,
    ))
    .expect("compile adversarial negotiation guest");
    let runtime = Runtime::default();
    let guest = runtime
        .spawn_system_guest(descriptor(with_abi_version(
            module,
            selium_abi::SELIUM_ABI_VERSION,
        )))
        .expect("the passive declaration negotiates without running the export");

    let (status, operation_id) =
        runtime.begin_hostcall(guest.process_id, HostcallRequest::SelfInfo);
    assert_eq!(status, HOSTCALL_STATUS_READY);
    assert_eq!(
        operation_id, 1,
        "the first real request owns id 1; negotiation created no operation"
    );
}

/// New guest / old host: the preamble is a fixed prefix, so a legacy host
/// that reads the request as a bare envelope is outside the supported
/// contract. The current host validates the prefix and rejects any version
/// mismatch explicitly, naming both sides.
#[test]
fn new_preamble_is_validated_and_version_mismatches_are_explicit() {
    let envelope = HostcallEnvelope {
        request: HostcallRequest::SelfInfo,
        task_id: None,
    };
    let encoded = encode_hostcall_request(&envelope).expect("encode");

    // The fixed prefix is present before the rkyv archive.
    assert_eq!(
        encoded.get(..4),
        Some(selium_abi::HOSTCALL_MAGIC.to_le_bytes().as_slice())
    );
    assert_eq!(
        encoded.get(4..selium_abi::HOSTCALL_PREAMBLE_SIZE),
        Some(selium_abi::SELIUM_ABI_VERSION.to_le_bytes().as_slice())
    );

    // The current host strips the prefix and decodes the same request.
    let payload = hostcall_request_payload(&encoded).expect("matching version");
    assert_eq!(
        decode_rkyv::<HostcallEnvelope>(payload).expect("decode"),
        envelope
    );

    // A mismatched guest version is rejected before decoding and names both.
    let mut mismatched = selium_abi::HOSTCALL_MAGIC.to_le_bytes().to_vec();
    mismatched.extend_from_slice(&(selium_abi::SELIUM_ABI_VERSION + 1).to_le_bytes());
    mismatched.extend_from_slice(
        encoded
            .get(selium_abi::HOSTCALL_PREAMBLE_SIZE..)
            .expect("encoded request carries a payload"),
    );
    let error = hostcall_request_payload(&mismatched).expect_err("newer guest must be rejected");
    let message = error.to_string();
    assert!(
        message.contains(&selium_abi::SELIUM_ABI_VERSION.to_string())
            && message.contains(&(selium_abi::SELIUM_ABI_VERSION + 1).to_string()),
        "the mismatch must name both versions: {message}"
    );
}
