use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use rustls::{
    crypto::ring::sign::any_supported_type,
    pki_types::{CertificateDer, PrivateKeyDer},
    sign,
};
use rustls_pki_types::{PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, pem::SliceIter};
use selium_abi::Capability;
use selium_filesystem_store::{FilesystemStore, FilesystemStoreReadDriver};
use selium_kernel::{
    Kernel, drivers, guest_async::GuestAsync, operation::LinkableOperation,
    session::SessionLifecycleDriver,
};
use selium_messaging::{ChannelDriver, ChannelStrongIoDriver, ChannelWeakIoDriver};
use selium_net_quinn::QuinnDriver;
use selium_wasmtime::{WasmRuntime, WasmtimeDriver};
use tokio::sync::Notify;

/// Where certificates are stored
const CERTS_SUBDIR: &str = "certs";
/// Where WASM modules are stored
const MODULES_SUBDIR: &str = "modules";

pub fn build(work_dir: impl AsRef<Path>) -> Result<(Kernel, Arc<Notify>)> {
    let certs_dir: PathBuf = work_dir.as_ref().join(CERTS_SUBDIR);
    let modules_dir: PathBuf = work_dir.as_ref().join(MODULES_SUBDIR);

    let mut builder = Kernel::build();
    let mut capability_ops: HashMap<Capability, Vec<Arc<dyn LinkableOperation>>> = HashMap::new();

    // Session Lifecycle
    let drv = builder.add_capability(SessionLifecycleDriver::new());
    let session = drivers::session::operations(drv);
    capability_ops
        .entry(Capability::SessionLifecycle)
        .or_default()
        .extend([
            session.0.as_linkable(),
            session.1.as_linkable(),
            session.2.as_linkable(),
            session.3.as_linkable(),
            session.4.as_linkable(),
            session.5.as_linkable(),
        ]);

    // Channel Lifecycle
    let chan_drv = builder.add_capability(ChannelDriver::new());
    let channel = drivers::channel::lifecycle_ops(chan_drv.clone());
    let handoff = drivers::channel::handoff_ops();
    capability_ops
        .entry(Capability::ChannelLifecycle)
        .or_default()
        .extend([
            channel.0.as_linkable(),
            channel.1.as_linkable(),
            channel.2.as_linkable(),
            handoff.0.as_linkable(),
            handoff.1.as_linkable(),
            handoff.2.as_linkable(),
        ]);

    // Channel Reader
    let chan_strong_drv = builder.add_capability(ChannelStrongIoDriver::new());
    let chan_weak_drv = builder.add_capability(ChannelWeakIoDriver::new());
    let reader = drivers::channel::read_ops(chan_strong_drv.clone(), chan_weak_drv.clone());
    capability_ops
        .entry(Capability::ChannelReader)
        .or_default()
        .extend([
            reader.0.as_linkable(),
            reader.1.as_linkable(),
            reader.2.as_linkable(),
            reader.3.as_linkable(),
        ]);

    // Channel Writer
    let writer = drivers::channel::write_ops(chan_strong_drv, chan_weak_drv);
    let writer_downgrade = drivers::channel::writer_downgrade_op(chan_drv.clone());
    capability_ops
        .entry(Capability::ChannelWriter)
        .or_default()
        .extend([
            writer.0.as_linkable(),
            writer.1.as_linkable(),
            writer.2.as_linkable(),
            writer.3.as_linkable(),
            writer_downgrade.as_linkable(),
        ]);

    let process_logs = drivers::process::log_ops::<WasmtimeDriver>();
    capability_ops
        .entry(Capability::ChannelLifecycle)
        .or_default()
        .push(process_logs.0.as_linkable());

    // Network
    let cert_path = certs_dir.join("server.crt");
    let key_path = certs_dir.join("server.key");
    let server_certified_key = load_certified_key(&cert_path, &key_path)
        .context("load QUIC listener certificate and key")?;
    let drv = builder.add_capability(QuinnDriver::new(Arc::new(server_certified_key)));
    capability_ops
        .entry(Capability::NetBind)
        .or_default()
        .push(drivers::net::listener_op(drv.clone()).as_linkable());
    capability_ops
        .entry(Capability::NetAccept)
        .or_default()
        .push(drivers::net::accept_op(drv.clone()).as_linkable());
    capability_ops
        .entry(Capability::NetConnect)
        .or_default()
        .push(drivers::net::connect_op(drv.clone()).as_linkable());
    capability_ops
        .entry(Capability::NetRead)
        .or_default()
        .push(drivers::net::read_op(drv.clone()).as_linkable());
    capability_ops
        .entry(Capability::NetWrite)
        .or_default()
        .push(drivers::net::write_op(drv).as_linkable());

    // Module Filesystem Store
    let fs_store = FilesystemStore::new(&modules_dir);
    let shutdown = Arc::new(Notify::new());
    let guest_async_cap = builder.add_capability(Arc::new(GuestAsync::new(Arc::clone(&shutdown))));
    let fs_store_drv = builder.add_capability(FilesystemStoreReadDriver::new(fs_store));
    let wasm_runtime = Arc::new(WasmRuntime::new(
        capability_ops.clone(),
        Arc::clone(&guest_async_cap),
    )?);
    let drv = builder.add_capability(WasmtimeDriver::new(Arc::clone(&wasm_runtime), fs_store_drv));
    let process = drivers::process::lifecycle_ops(drv.clone());
    wasm_runtime
        .extend_capability(
            Capability::ProcessLifecycle,
            vec![
                process.0.as_linkable(),
                process.1.as_linkable(),
                process_logs.1.as_linkable(),
            ],
        )
        .map_err(anyhow::Error::from)?;

    Ok((builder.build()?, shutdown))
}

fn load_certified_key(cert_path: &Path, key_path: &Path) -> Result<sign::CertifiedKey> {
    let certificates = load_certificate_chain(cert_path)
        .with_context(|| format!("load certificate {cert_path:?}"))?;
    let private_key =
        load_private_key(key_path).with_context(|| format!("load private key {key_path:?}"))?;
    let signing_key = any_supported_type(&private_key).context("build signing key")?;
    let mut certified_key = sign::CertifiedKey::new(certificates, signing_key);
    certified_key.ocsp = None;
    Ok(certified_key)
}

fn load_certificate_chain(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let bytes = fs::read(path).with_context(|| format!("read certificate file {path:?}"))?;
    let parsed = SliceIter::new(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .context("parse PEM certificate chain")?;
    if !parsed.is_empty() {
        return Ok(parsed);
    }

    Ok(vec![CertificateDer::from(bytes)])
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let bytes = fs::read(path).with_context(|| format!("read private key {path:?}"))?;
    let pkcs8_keys = SliceIter::new(&bytes)
        .collect::<Result<Vec<PrivatePkcs8KeyDer>, _>>()
        .context("parse PKCS#8 private keys")?;
    if let Some(key) = pkcs8_keys.into_iter().next() {
        return Ok(key.into());
    }

    let rsa_keys = SliceIter::new(&bytes)
        .collect::<Result<Vec<PrivatePkcs1KeyDer>, _>>()
        .context("parse RSA private keys")?;
    if let Some(key) = rsa_keys.into_iter().next() {
        return Ok(key.into());
    }

    PrivateKeyDer::try_from(bytes).map_err(|_| anyhow!("no private key found in provided material"))
}
