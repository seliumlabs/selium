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
#![allow(
    dead_code,
    clippy::panic,
    reason = "shared spine test-support compiled per binary"
)]

use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use selium_abi::{
    ActivityKind, Capability, CapabilityGrant, Namespace, ResourceClass, ResourceSelector,
};
use selium_client::{ClientIdentity, ConnectOptions, FlatMsg as _};
use selium_kernel::Kernel;
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor};

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
/// Reviewed bound on a guest reaching a logged milestone after bootstrap:
/// onboarding, deployment recording, catalogue replay, a connector consuming
/// the anchor set. A spine that has not converged in this window is parked or
/// dead, not merely slow; the bound is generous enough for a loaded CI runner
/// while still failing loudly. Waits use `wait_for_logs(..., CONVERGENCE_BOUND)`
/// rather than an incidental literal, so slowing a test is a reviewed change to
/// one constant.
pub(crate) const CONVERGENCE_BOUND: Duration = Duration::from_secs(120);
/// Reviewed bound on the interpreted-guest QUIC/TLS handshake. The handshake
/// runs inside the wasm connector, so it is an order of magnitude slower than
/// a native one; 240 s separates "loaded runner" from "never completes".
/// Bound on the interpreted connector's mutual-TLS handshake. The connector
/// runs in a WASM guest, a dropped datagram costs a retransmission round, and
/// the server's own idle budget is 300 s; the client must outlast both so the
/// bound distinguishes "never completes" from "slow under load".
pub(crate) const HANDSHAKE_BOUND: Duration = Duration::from_secs(420);
pub(crate) const LEAF_MANIFEST: &str = "acme-leaf";
/// Reviewed bound on observing one state change after it is triggered
/// (anchor-set rebuild, revocation refusal, a probe's catalogue round trip).
pub(crate) const OBSERVATION_BOUND: Duration = Duration::from_secs(60);
/// Blob store + manifest the onboarding guest writes the issued leaf to.
pub(crate) const ONBOARD_STORE: &str = "selium.identity-onboard.out";
/// The day-1 scheduler seam's typed deferred context.
pub(crate) const SCHEDULER_DEFERRED: &str = "scheduler service not yet online";
/// SNI / TLS server name: the bare root wire name for the single-instance
/// bridge route (resolved by the connector to `sel:///bridge`).
pub(crate) const SERVER_NAME: &str = "bridge";
/// Reviewed bound on a short guest milestone (a listener reporting ready, a
/// probe logging its result) where the work is a few interpreted passes.
pub(crate) const SHORT_BOUND: Duration = Duration::from_secs(30);
/// The tenant the onboarding guest mints and the client connects as.
pub(crate) const TENANT: &str = "acme";

/// A temporary state root for one test, removed on drop.
///
/// Spine tests boot against a throwaway root so durable state starts clean and
/// never leaks between runs; the same root can be reused across two boots to
/// assert that state survives a restart.
pub(crate) struct StateRoot {
    path: PathBuf,
}

/// Cross-process guard for the connector's fixed listener port: the spine
/// test binaries (one runtime per process) must not bind `0.0.0.0:4433`
/// concurrently.
///
/// The operating system holds the lock on an open file handle, so a process
/// killed while holding it releases the lock on exit; no stale-file age
/// heuristic or wall-clock comparison is involved. Acquisition is bounded so a
/// wedged holder fails the test instead of hanging the lane.
pub(crate) struct SpinePortGuard(std::fs::File);

impl StateRoot {
    /// Creates a unique temporary state root.
    pub(crate) fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self {
            path: std::env::temp_dir().join(format!(
                "selium-spine-{tag}-{}-{nanos}-{unique}",
                std::process::id()
            )),
        }
    }

    /// The state directory path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Takes ownership of an existing state directory, removing it on drop.
    ///
    /// Used by the second lifetime of a power-cycle test, which runs in a fresh
    /// process and must assert against the directory the first one left behind.
    pub(crate) fn adopt(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The state directory as a [`RuntimeConfig`](selium_runtime::RuntimeConfig)
    /// field.
    pub(crate) fn config_dir(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
}

impl Drop for StateRoot {
    fn drop(&mut self) {
        // Best-effort: a test that fails mid-way still cleans up here.
        drop(std::fs::remove_dir_all(&self.path));
    }
}

impl SpinePortGuard {
    /// Acquires the spine port, blocking until no other spine binary holds it.
    pub(crate) fn acquire() -> Self {
        const ACQUIRE_BOUND: Duration = Duration::from_secs(600);
        let path = std::env::temp_dir().join("selium-quic-spine-4433.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .expect("open spine port lock");
        let deadline = std::time::Instant::now() + ACQUIRE_BOUND;
        loop {
            match file.try_lock() {
                Ok(()) => return Self(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "timed out acquiring the spine port lock after {ACQUIRE_BOUND:?}"
                    );
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    panic!("spine port lock failed: {error}")
                }
            }
        }
    }
}

impl Drop for SpinePortGuard {
    fn drop(&mut self) {
        // Explicit unlock is best-effort; closing the handle releases the OS
        // lock in any case.
        if let Err(error) = self.0.unlock() {
            eprintln!("spine port unlock failed: {error}");
        }
    }
}

/// The single per-platform bridge server, conferring from the identity grant
/// table. Boots as a root guest (`tenant: None`) holding a `Namespace::Root`
/// `DelegateGrants` grant and the route claim for its root route.
pub(crate) fn bridge_server_descriptor(
    module_bytes: Vec<u8>,
    bridge_channel_module: Vec<u8>,
    _dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "bridge-server".to_string(),
        module_id: "bridge-server-module".to_string(),
        module_bytes,
        entrypoint: "bridge_server".to_string(),
        arguments: vec![SystemGuestArg::Pointer(bridge_channel_module)],
        grants: vec![
            CapabilityGrant::new(
                Capability::ProcessLifecycle,
                vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
            ),
            CapabilityGrant::new(
                Capability::DelegateGrants,
                vec![ResourceSelector::Namespace(Namespace::Root)],
            ),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///bridge".to_string())],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
        ],
        tenant: None,
        discovery_service: false,
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
    // Keep-alives hold a control connection open across the interpreted
    // connector's slow phases (identity onboarding, CLI subprocess spines):
    // the server's idle budget is shorter than a long test section, and an
    // idle QUIC connection is reaped as lost.
    transport.keep_alive_interval(Some(Duration::from_secs(10)));

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
    _dependencies: Vec<String>,
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
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            ),
        ],
        tenant: None,
        discovery_service: false,
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
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
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
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///control".to_string())],
            ),
        ],
        tenant: None,
        discovery_service: false,
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

pub(crate) fn drain_logs(runtime: &Runtime, process_id: u64) -> Vec<String> {
    runtime
        .kernel()
        .processes()
        .drain_log_channel(process_id)
        .unwrap_or_else(|error| {
            let tail: Vec<String> = runtime
                .activity_log()
                .iter()
                .rev()
                .take(10)
                .map(|event| event.message.clone())
                .collect();
            panic!("drain log channel {process_id}: {error}; activity tail: {tail:#?}")
        })
        .iter()
        .map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        })
        .collect()
}

/// Drains a process's log channel, or returns nothing when the process has
/// already been reaped (there is no channel left, and no frames coming).
pub(crate) fn drain_logs_if_live(runtime: &Runtime, process_id: u64) -> Vec<String> {
    let Ok(frames) = runtime.kernel().processes().drain_log_channel(process_id) else {
        return Vec::new();
    };
    frames
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
        tenant: None,
        discovery_service: false,
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
    _dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    operator_descriptor_named("identity-onboard", module_bytes, spki, mode, _dependencies)
}

/// The same operator guest under an explicit name.
///
/// A test that drives two legs concurrently boots the guest twice, and two
/// descriptors cannot share a name.
pub(crate) fn operator_descriptor_named(
    name: &str,
    module_bytes: Vec<u8>,
    spki: Vec<u8>,
    mode: u64,
    _dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: name.to_string(),
        module_id: format!("{name}-module"),
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
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            ),
        ],
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// Reads a blob's content out of the store's own layout.
pub(crate) fn read_blob(runtime: &Runtime, store: &str, blob_id: &str) -> Option<Vec<u8>> {
    let volumes = runtime.kernel().volumes();
    let name = selium_store::format::blob_volume(store, blob_id);
    if !volumes.list(None).expect("list volumes").contains(&name) {
        return None;
    }
    let volume = volumes.open(None, &name).expect("open blob volume");
    let bytes = volumes
        .read_at(volume.local_id, 0, u32::MAX as usize)
        .expect("read blob volume");
    drop(volumes.close(volume.local_id));
    Some(bytes)
}

/// Reads the issued leaf the onboarding guest wrote to the blob store.
pub(crate) fn read_issued_leaf(runtime: &Runtime) -> Vec<u8> {
    let leaf_id =
        read_manifest(runtime, ONBOARD_STORE, LEAF_MANIFEST).expect("issued leaf manifest present");
    read_blob(runtime, ONBOARD_STORE, &leaf_id).expect("issued leaf blob present")
}

/// Reads every complete record a log's volumes hold, in sequence order.
pub(crate) fn read_log_records(runtime: &Runtime, log: &str) -> Vec<selium_store::format::Record> {
    let volumes = runtime.kernel().volumes();
    let names = volumes.list(None).expect("list volumes");
    let mut segments: Vec<u64> = names
        .iter()
        .filter_map(|name| selium_store::format::parse_log_volume(name))
        .filter_map(|parsed| parsed.ok())
        .filter(|(parsed_log, _segment)| parsed_log == log)
        .map(|(_log, segment)| segment)
        .collect();
    segments.sort_unstable();

    let mut records = Vec::new();
    for segment in segments {
        let name = selium_store::format::log_volume(log, segment);
        let volume = volumes.open(None, &name).expect("open log volume");
        let bytes = volumes
            .read_at(volume.local_id, 0, u32::MAX as usize)
            .expect("read log volume");
        drop(volumes.close(volume.local_id));
        let (frames, _end) = selium_store::format::scan_frames(&bytes);
        for frame in frames {
            if let Ok(record) = selium_store::format::decode_record(frame) {
                records.push(record);
            }
        }
    }
    records
}

/// Reads a manifest's blob id out of the store's own layout.
pub(crate) fn read_manifest(runtime: &Runtime, store: &str, manifest: &str) -> Option<String> {
    let volumes = runtime.kernel().volumes();
    let name = selium_store::format::manifest_volume(store);
    if !volumes.list(None).expect("list volumes").contains(&name) {
        return None;
    }
    let volume = volumes.open(None, &name).expect("open manifest volume");
    let bytes = volumes
        .read_at(volume.local_id, 0, u32::MAX as usize)
        .expect("read manifest volume");
    drop(volumes.close(volume.local_id));
    let (frames, _end) = selium_store::format::scan_frames(&bytes);
    let mut found = None;
    for frame in frames {
        if let Ok((name, blob_id)) = selium_store::format::decode_manifest(frame)
            && name == manifest
        {
            found = Some(blob_id);
        }
    }
    found
}

pub(crate) fn read_wasm(crate_name: &str, file_name: &str) -> Vec<u8> {
    super::common::read_guest_wasm(crate_name, file_name)
}

/// Creates a runtime whose volume layer is already rooted at `root`.
///
/// A test that seeds durable state (TLS material, a ledger) *before* bootstrap
/// needs the root in place first: the state root may only be set before first
/// use, so seeding into a throwaway root and re-rooting afterwards is refused
/// by design. Passing `root.config_dir()` to the bootstrap is then a no-op.
pub(crate) fn runtime_in_state_root(root: &StateRoot) -> Runtime {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seed = u64::from(std::process::id())
        ^ COUNTER
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .rotate_left(23);
    Runtime::new(Kernel::with_state_dir(seed, root.path()))
}

/// Writes a blob's content into the store's own layout under the platform
/// tenant, exactly as the store's `put` does, and returns the content id.
pub(crate) fn seed_blob(runtime: &Runtime, store: &str, bytes: &[u8]) -> String {
    let id = selium_store::format::blob_id(bytes);
    let volumes = runtime.kernel().volumes();
    let name = selium_store::format::blob_volume(store, &id);
    let volume = volumes.open(None, &name).expect("open blob volume");
    volumes.append(volume.local_id, bytes).expect("append blob");
    volumes.sync(volume.local_id).expect("sync blob volume");
    drop(volumes.close(volume.local_id));
    id
}

/// Points a manifest at a blob id in the store's own manifest layout.
pub(crate) fn seed_manifest(runtime: &Runtime, store: &str, manifest: &str, blob_id: &str) {
    let volumes = runtime.kernel().volumes();
    let name = selium_store::format::manifest_volume(store);
    let volume = volumes.open(None, &name).expect("open manifest volume");
    let framed =
        selium_store::format::frame(&selium_store::format::encode_manifest(manifest, blob_id));
    volumes
        .append(volume.local_id, &framed)
        .expect("append manifest");
    volumes.sync(volume.local_id).expect("sync manifest volume");
    drop(volumes.close(volume.local_id));
}

/// Provisions the connector's own TLS material (server certificate + key)
/// into the `tls-certs` blob store. Client trust anchors are NOT seeded here —
/// they come exclusively from the identity guest's anchor live table.
pub(crate) fn seed_tls_blob_store(runtime: &Runtime) {
    let cert_id = seed_blob(runtime, "tls-certs", BRIDGE_CERT_PEM);
    let key_id = seed_blob(runtime, "tls-certs", BRIDGE_KEY_PEM);
    seed_manifest(runtime, "tls-certs", "cert-pem", &cert_id);
    seed_manifest(runtime, "tls-certs", "key-pem", &key_id);
}

/// The durable store system guest: the platform's only owner of durable-log
/// and blob-store semantics, serving the root-namespace `sel:///store` route
/// over the host's opaque volumes. Every guest that opens a log or blob store
/// declares it as a dependency, so the store is ready first.
pub(crate) fn store_descriptor(_dependencies: Vec<String>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "store".to_string(),
        module_id: "store-module".to_string(),
        module_bytes: read_wasm("selium-store", "selium_store.wasm"),
        entrypoint: "store_main".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            // The store serves a root-namespace route under an exclusive
            // host-issued claim, and probes capability state as the registry.
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///store".to_string())],
            ),
            CapabilityGrant::new(Capability::ProcessProbe, Vec::new()),
        ],
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// Boots a resident probe that lists the current discovery domain table.
pub(crate) fn probe_domains(runtime: &Runtime, name: &str) -> u64 {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: name.to_string(),
                module_id: format!("{name}-module"),
                module_bytes: read_wasm("selium-discovery-probe", "selium_discovery_probe.wasm"),
                entrypoint: "discovery_probe".to_string(),
                arguments: vec![
                    SystemGuestArg::Pointer(Vec::new()),
                    SystemGuestArg::Integer(13),
                ],
                grants: vec![
                    CapabilityGrant::new(
                        Capability::HostQueue,
                        vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
                    ),
                    CapabilityGrant::new(
                        Capability::SharedMemory,
                        vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                    ),
                ],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        })
        .expect("bootstrap domain-list probe");
    report.guests.first().expect("domain-list probe").process_id
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
        // A guest that has already exited has no log channel left. That is not
        // an error for a waiter: it means no further frames are coming, and
        // the interesting diagnosis is what it logged *before* it went. Report
        // the timeout rather than panicking on the drain, so the accumulated
        // lines are what the caller sees.
        seen.extend(drain_logs_if_live(runtime, process_id));
        // The reap path drains a departing guest's remaining frames into its
        // `ProcessExited` activity record before releasing the channel. A
        // short-lived guest can therefore finish before its waiter observes
        // the channel at all; read the record too so the milestone stays
        // observable. (Without this, `power_cycle` lost the quota leg's line
        // whenever that leg completed before the test reached it, which the
        // enforced pack reproduced under load.) The record is a joined summary
        // of the drained lines, so add it once.
        if !runtime.is_guest_resident(process_id) {
            for message in reaped_log_lines(runtime, process_id) {
                if !seen.contains(&message) {
                    seen.push(message);
                }
            }
        }
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

/// Log summaries the reap path moved into the activity log for `process_id`.
///
/// A guest that exits has its remaining log frames drained into the
/// `ProcessExited` activity record before the channel is released, so this is
/// the only place its last lines survive.
fn reaped_log_lines(runtime: &Runtime, process_id: u64) -> Vec<String> {
    runtime
        .activity_log()
        .iter()
        .filter(|event| {
            event.process_id == Some(process_id) && event.kind == ActivityKind::ProcessExited
        })
        .map(|event| event.message.clone())
        .collect()
}
