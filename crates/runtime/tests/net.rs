//! Network integration tests.
//!
//! Validates network hostcall address validation (literals-only enforcement)
//! and URI-scoped capability grants via the runtime API path.

use selium_abi::{
    AbiErrorCode, Capability, CapabilityGrant, CompletionState, HostcallRequest, ResourceClass,
    ResourceSelector,
};
use selium_runtime::{ReadinessCondition, Runtime, SystemGuestDescriptor};

#[expect(clippy::panic, reason = "test helper")]
fn assert_failed_with_code(
    runtime: &Runtime,
    process_id: u64,
    op: u64,
    expected_code: AbiErrorCode,
    context: &str,
) {
    match runtime.poll_hostcall(process_id, op) {
        CompletionState::Failed(error) => {
            assert_eq!(
                error.code, expected_code,
                "{context}: expected {:?}, got {:?}",
                expected_code, error.code
            );
        }
        other => panic!("{context}: expected failed hostcall, got {other:?}"),
    }
}

#[test]
fn class_only_network_grant_retains_allow_all_endpoints() {
    let runtime = Runtime::default();
    let guest = spawn_with_grants(
        &runtime,
        vec![CapabilityGrant::new(
            Capability::Network,
            vec![ResourceSelector::ResourceClass(ResourceClass::TcpStream)],
        )],
    );

    // Multiple loopback ports — none should be denied with PermissionDenied.
    for port in &["1", "8080", "9999"] {
        let address = format!("127.0.0.1:{port}");
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::TcpConnect {
                address: address.clone(),
            },
        );
        if status == selium_abi::HOSTCALL_STATUS_FAILED {
            match runtime.poll_hostcall(guest.process_id, op) {
                CompletionState::Failed(error) => {
                    assert_ne!(
                        error.code,
                        AbiErrorCode::PermissionDenied,
                        "class-only grant must not deny {address}"
                    );
                }
                other => panic!("expected failed, got {other:?}"),
            }
        }
    }
}

fn empty_module() -> Vec<u8> {
    wat::parse_str("(module (func (export \"boot\")))").expect("compile wat")
}

#[test]
fn ip_literal_passes_validation() {
    let runtime = Runtime::default();
    let guest = spawn_with_grants(
        &runtime,
        vec![CapabilityGrant::new(
            Capability::Network,
            vec![ResourceSelector::ResourceClass(ResourceClass::TcpStream)],
        )],
    );
    let (status, op) = runtime.begin_hostcall(
        guest.process_id,
        HostcallRequest::TcpConnect {
            address: "127.0.0.1:1".to_string(),
        },
    );
    if status == selium_abi::HOSTCALL_STATUS_FAILED {
        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_ne!(
                    error.code,
                    AbiErrorCode::MalformedPayload,
                    "IP literal must not be rejected as MalformedPayload"
                );
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }
}

fn spawn_with_grants(
    runtime: &Runtime,
    grants: Vec<CapabilityGrant>,
) -> selium_runtime::BootstrappedGuest {
    runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: "net-test".to_string(),
            module_id: "net-test-module".to_string(),
            module_bytes: empty_module(),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants,
            dependencies: Vec::new(),
            readiness: ReadinessCondition::Immediate,
            tenant: None,
            serving_role: None,
            handlers: Vec::new(),
        })
        .expect("spawn net test guest")
}

#[test]
fn tcp_bind_rejects_hostname() {
    let runtime = Runtime::default();
    let guest = spawn_with_grants(
        &runtime,
        vec![CapabilityGrant::new(
            Capability::Network,
            vec![ResourceSelector::ResourceClass(ResourceClass::TcpListener)],
        )],
    );
    let (status, op) = runtime.begin_hostcall(
        guest.process_id,
        HostcallRequest::TcpBind {
            address: "example.com:0".to_string(),
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
    assert_failed_with_code(
        &runtime,
        guest.process_id,
        op,
        AbiErrorCode::MalformedPayload,
        "hostname bind",
    );
}

#[test]
fn tcp_connect_rejects_hostname() {
    let runtime = Runtime::default();
    let guest = spawn_with_grants(
        &runtime,
        vec![CapabilityGrant::new(
            Capability::Network,
            vec![ResourceSelector::ResourceClass(ResourceClass::TcpStream)],
        )],
    );
    let (status, op) = runtime.begin_hostcall(
        guest.process_id,
        HostcallRequest::TcpConnect {
            address: "localhost:80".to_string(),
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
    assert_failed_with_code(
        &runtime,
        guest.process_id,
        op,
        AbiErrorCode::MalformedPayload,
        "hostname connect",
    );
}

#[test]
fn udp_bind_rejects_hostname() {
    let runtime = Runtime::default();
    let guest = spawn_with_grants(
        &runtime,
        vec![CapabilityGrant::new(
            Capability::Network,
            vec![ResourceSelector::ResourceClass(ResourceClass::UdpSocket)],
        )],
    );
    let (status, op) = runtime.begin_hostcall(
        guest.process_id,
        HostcallRequest::UdpBind {
            address: "myhost.local:8080".to_string(),
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
    assert_failed_with_code(
        &runtime,
        guest.process_id,
        op,
        AbiErrorCode::MalformedPayload,
        "hostname udp bind",
    );
}

#[test]
fn uri_prefix_grant_allows_loopback_denies_other_hosts() {
    let runtime = Runtime::default();
    let guest = spawn_with_grants(
        &runtime,
        vec![CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                ResourceSelector::UriPrefix("tcp://127.0.0.1:*".to_string()),
            ],
        )],
    );

    // Loopback connect — permission check should pass.
    let (status, op) = runtime.begin_hostcall(
        guest.process_id,
        HostcallRequest::TcpConnect {
            address: "127.0.0.1:1".to_string(),
        },
    );
    if status == selium_abi::HOSTCALL_STATUS_FAILED {
        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_ne!(
                    error.code,
                    AbiErrorCode::PermissionDenied,
                    "loopback connect must not be permission-denied under 127.0.0.1:* grant"
                );
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }

    // Non-loopback connect — must be denied with PermissionDenied.
    let (status, op) = runtime.begin_hostcall(
        guest.process_id,
        HostcallRequest::TcpConnect {
            address: "93.184.216.34:443".to_string(),
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
    match runtime.poll_hostcall(guest.process_id, op) {
        CompletionState::Failed(error) => {
            assert_eq!(
                error.code,
                AbiErrorCode::PermissionDenied,
                "non-loopback connect must be permission-denied"
            );
            assert!(
                error.message.contains("tcp://93.184.216.34:443"),
                "denial error must include canonical URI, got: {}",
                error.message
            );
        }
        other => panic!("expected failed, got {other:?}"),
    }
}

#[test]
fn uri_prefix_without_network_class_rejected_at_spawn() {
    let runtime = Runtime::default();
    let result = runtime.spawn_system_guest(SystemGuestDescriptor {
        name: "bad-grant-test".to_string(),
        module_id: "bad-grant-module".to_string(),
        module_bytes: empty_module(),
        entrypoint: "boot".to_string(),
        arguments: Vec::new(),
        grants: vec![CapabilityGrant::new(
            Capability::Network,
            vec![ResourceSelector::UriPrefix(
                "tcp://10.0.0.5:443".to_string(),
            )],
        )],
        dependencies: Vec::new(),
        readiness: ReadinessCondition::Immediate,
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    });

    assert!(
        result.is_err(),
        "grant with UriPrefix but no network ResourceClass must be rejected at spawn"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("UriPrefix"),
        "error must mention UriPrefix, got: {err}"
    );
}
