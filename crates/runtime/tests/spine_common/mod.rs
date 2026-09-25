//! Shared support for the QUIC-spine integration test binaries: the guest
//! descriptor builders, native client option builders, and log helpers the
//! golden-path identity spine and the no-mTLS spine share.
//!
//! Each spine test lives in its **own test binary**: the runtime installs a
//! process-global region provider (one runtime per process), and every test
//! binds the connector's fixed listener (`0.0.0.0:4433`, see its
//! `QUIC_LISTEN_ADDR`), so the binaries serialise on a cross-process port
//! lock rather than parallelising.

// Each integration test binary compiles this module independently and uses
// only the helpers its guests need; per-binary, some helpers are dead code.
#![allow(dead_code, reason = "shared spine test-support compiled per binary")]

use std::{
    fs::OpenOptions,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use selium_abi::{Capability, CapabilityGrant, Namespace, ResourceClass, ResourceSelector};
use selium_client::{ClientIdentity, ConnectOptions, FlatMsg as _};
use selium_runtime::{ReadinessCondition, Runtime, SystemGuestArg, SystemGuestDescriptor};

/// The single-instance bridge route's server certificate (SAN `bridge`),
/// provisioned into the `tls-certs` blob store before the connector guest
/// boots.
pub(crate) const BRIDGE_CERT_PEM: &[u8] =
    include_bytes!("../../../../guests/connector-quic/tests/fixtures/bridge_cert.pem");
pub(crate) const BRIDGE_KEY_PEM: &[u8] =
    include_bytes!("../../../../guests/connector-quic/tests/fixtures/bridge_key.pem");
/// The connector's fixed listener (its `QUIC_LISTEN_ADDR` const).
pub(crate) const CONNECTOR_ADDR: &str = "127.0.0.1:4433";
/// The single-instance control plane's root served route, named in the bridge
/// handshake.
pub(crate) const CONTROL_URI: &str = "sel:///control";
pub(crate) const LEAF_MANIFEST: &str = "acme-leaf";
/// Blob store + manifest the onboarding guest writes the issued leaf to.
pub(crate) const ONBOARD_STORE: &str = "selium.identity-onboard.out";
/// The day-1 scheduler seam's typed deferred context.
pub(crate) const SCHEDULER_DEFERRED: &str = "scheduler service not yet online";
/// SNI / TLS server name: the bare root wire name for the single-instance
/// bridge route (resolved by the connector to `sel:///bridge`).
pub(crate) const SERVER_NAME: &str = "bridge";
/// The tenant the onboarding guest mints and the client connects as.
pub(crate) const TENANT: &str = "acme";

/// Cross-process guard for the connector's fixed listener port: the spine
/// test binaries (one runtime per process) must not bind `0.0.0.0:4433`
/// concurrently. Spin-acquired via an exclusive lock file, released on
/// drop.
pub(crate) struct SpinePortGuard(PathBuf);

impl SpinePortGuard {
    /// Acquires the spine port, blocking until no other spine binary holds
    /// it.
    pub(crate) fn acquire() -> Self {
        let path = std::env::temp_dir().join("selium-quic-spine-4433.lock");
        loop {
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(_) => return Self(path),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}

impl Drop for SpinePortGuard {
    fn drop(&mut self) {
        // Best-effort release; a crash between create and drop leaves a
        // stale lock file (tests are expected to run on a clean temp dir).
        drop(std::fs::remove_file(&self.0));
    }
}

/// The single per-platform bridge server, conferring from the identity grant
/// table. Boots as a root guest (`tenant: None`) holding a `Namespace::Root`
/// `DelegateGrants` grant and `SystemRegistration` for its root route.
pub(crate) fn bridge_server_descriptor(
    module_bytes: Vec<u8>,
    dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "bridge-server".to_string(),
        module_id: "bridge-server-module".to_string(),
        module_bytes,
        entrypoint: "bridge_server".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::ProcessLifecycle,
                vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
            ),
            CapabilityGrant::new(
                Capability::DelegateGrants,
                vec![ResourceSelector::Namespace(Namespace::Root)],
            ),
            CapabilityGrant::new(Capability::SystemRegistration, Vec::new()),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
        ],
        dependencies,
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

/// Builds the native client options: trust the connector's certificate and
/// present the issued leaf + its generated key for mTLS.
pub(crate) fn client_options(leaf_der: Vec<u8>, client_pkcs8: Vec<u8>) -> ConnectOptions {
    ConnectOptions {
        identity: Some(mtls_identity(leaf_der, client_pkcs8)),
        ..client_options_no_identity()
    }
}

/// Builds the native client options without a client identity (the mTLS-off
/// deployment shape: no identity guest, no client authentication).
pub(crate) fn client_options_no_identity() -> ConnectOptions {
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from(
        300_000u32,
    ))));
    transport.initial_rtt(Duration::from_millis(250));

    ConnectOptions {
        server_name: SERVER_NAME.to_string(),
        server_root: selium_client::certificates_from_pem(BRIDGE_CERT_PEM)
            .expect("parse bridge certificate PEM"),
        identity: None,
        transport: Some(Arc::new(transport)),
    }
}

/// The QUIC connector: Tier-1 `sel-quic` protocol handler, own TLS material
/// from blobs, client anchors from the identity anchor live table.
pub(crate) fn connector_descriptor(
    module_bytes: Vec<u8>,
    dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "quic-connector".to_string(),
        module_id: "quic-connector-module".to_string(),
        module_bytes,
        entrypoint: "connector_quic".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::UdpSocket)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::BlobStore)],
            ),
        ],
        dependencies,
        readiness: ReadinessCondition::Immediate,
        tenant: None,
        serving_role: None,
        handlers: vec!["sel-quic".to_string()],
    }
}

/// The single-instance control plane, serving the typed `deploy`/`status`
/// surface for every tenant from the root `sel:///control` route.
pub(crate) fn control_plane_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "control-plane".to_string(),
        module_id: "control-plane-module".to_string(),
        module_bytes,
        entrypoint: "control_plane_main".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::DurableLog)],
            ),
            CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::BlobStore)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(Capability::SystemRegistration, Vec::new()),
        ],
        dependencies: vec!["discovery".to_string()],
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

/// The discovery system guest: RPC listener + registration feed.
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
        ],
        dependencies: Vec::new(),
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

pub(crate) fn drain_logs(runtime: &Runtime, process_id: u64) -> Vec<String> {
    runtime
        .kernel()
        .processes()
        .drain_log_channel(process_id)
        .expect("drain log channel")
        .iter()
        .map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        })
        .collect()
}

/// The identity system guest: sole mint authority, keyring-backed signing,
/// durable registries, and the anchor/grant live tables.
pub(crate) fn identity_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "identity".to_string(),
        module_id: "identity-module".to_string(),
        module_bytes,
        entrypoint: "identity_main".to_string(),
        arguments: Vec::new(),
        grants: selium_identity::identity_grants(),
        dependencies: vec!["discovery".to_string()],
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

/// Builds the client identity payload from the issued leaf and its key.
pub(crate) fn mtls_identity(leaf_der: Vec<u8>, client_pkcs8: Vec<u8>) -> ClientIdentity {
    use quinn::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    ClientIdentity {
        cert_chain: vec![CertificateDer::from(leaf_der)],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(client_pkcs8)),
    }
}

/// The onboarding operator guest: receives the client SPKI pointer and an
/// integer entry mode, and drives the identity guest's mint/issue/grant tiers
/// (`MODE_ONBOARD`) or the revocation leg (`MODE_REVOKE`).
pub(crate) fn operator_descriptor(
    module_bytes: Vec<u8>,
    spki: Vec<u8>,
    mode: u64,
    dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "identity-onboard".to_string(),
        module_id: "identity-onboard-module".to_string(),
        module_bytes,
        entrypoint: "onboard".to_string(),
        arguments: vec![SystemGuestArg::Pointer(spki), SystemGuestArg::Integer(mode)],
        grants: vec![
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::BlobStore)],
            ),
        ],
        dependencies,
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

/// Reads the issued leaf the onboarding guest wrote to the blob store.
pub(crate) fn read_issued_leaf(runtime: &Runtime) -> Vec<u8> {
    let storage = runtime.kernel().storage();
    let store = storage.open_blob_store(&runtime.kernel().memory(), ONBOARD_STORE);
    let leaf_id = storage
        .get_manifest(store.local_id, LEAF_MANIFEST)
        .expect("leaf manifest lookup")
        .expect("issued leaf manifest present");
    storage
        .get_blob(store.local_id, &leaf_id)
        .expect("leaf blob lookup")
        .expect("issued leaf blob present")
}

pub(crate) fn read_wasm(crate_name: &str, file_name: &str) -> Vec<u8> {
    super::common::read_guest_wasm(crate_name, file_name)
}

/// Provisions the connector's own TLS material (server certificate + key)
/// into the `tls-certs` blob store. Client trust anchors are NOT seeded here —
/// they come exclusively from the identity guest's anchor live table.
pub(crate) fn seed_tls_blob_store(runtime: &Runtime) {
    let storage = runtime.kernel().storage();
    let store = storage.open_blob_store(&runtime.kernel().memory(), "tls-certs");
    let cert_id = storage
        .put_blob(store.local_id, BRIDGE_CERT_PEM.to_vec())
        .expect("put bridge cert blob");
    let key_id = storage
        .put_blob(store.local_id, BRIDGE_KEY_PEM.to_vec())
        .expect("put bridge key blob");
    storage
        .set_manifest(store.local_id, "cert-pem", cert_id)
        .expect("cert manifest");
    storage
        .set_manifest(store.local_id, "key-pem", key_id)
        .expect("key manifest");
}

#[expect(clippy::panic, reason = "test helper")]
pub(crate) fn wait_for_logs(
    runtime: &Runtime,
    process_id: u64,
    needles: &[(&str, usize)],
    timeout: Duration,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        seen.extend(drain_logs(runtime, process_id));
        if needles.iter().all(|(needle, count)| {
            seen.iter()
                .filter(|message| message.contains(needle))
                .count()
                >= *count
        }) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {needles:?} in guest log; got {seen:?}");
}
