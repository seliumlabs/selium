//! Selium durable store system guest.
//!
//! The store owns the platform's durable-log and blob-store semantics over the
//! host's opaque volume primitive: record framing, sequences, checkpoints,
//! content ids, manifests, per-tenant naming and crash-consistent recovery.
//! The host knows only handles, byte offsets, lengths and tenant labels.
//!
//! It serves one per-host instance on the root-namespace route `sel:///store`
//! as a typed FlatBuffers RPC surface ([`selium_service::StoreRequest`]). Every
//! session's tenant is derived from the delivering process's authority — never
//! from a caller-supplied field — so identical names in different tenants never
//! collide and one tenant's data is never served to another.
//!
//! ## Layout
//!
//! A log's records live in `log:<name>#<segment>` volumes, framed with a length
//! and CRC-32; a torn tail seals its segment, and the next append continues in
//! a new segment after the highest existing one. Named checkpoints, manifests
//! and blob content live in `checkpoint:<name>`, `manifest:<store>` and
//! `blob:<store>/<blob-id>` volumes respectively. See [`format`].
//!
//! ## Durability
//!
//! `append` is written and visible, not an fsync barrier: the store syncs in
//! batches and always before recording a checkpoint, so a checkpoint
//! acknowledgement is the log's durability barrier. Blob and manifest writes
//! are synced before they are acknowledged — they carry uploaded modules and
//! TLS material. The host never syncs implicitly.
//!
//! ## Build
//!
//! ```text
//! cargo build --target wasm32-unknown-unknown -p selium-store
//! ```

use std::sync::Arc;

use anyhow::Context as _;
use selium_guest::{
    Context, ResourceClass, ResourceListener, ResourceTarget, Serve, entrypoint, info,
    process_tenant, spawn, warn,
};
use selium_service::{StoreRecord, StoreRecordHeader, StoreRequest, StoreResponse};
use selium_shm::rpc::{RpcConnection, RpcError};

use crate::store::{Failure, Store, VolumeHandle, Volumes};

pub mod format;
pub mod store;

/// The root-namespace route the store serves on.
pub const STORE_ROUTE: &str = "sel:///store";

/// Volume layer backed by the host's volume hostcalls.
pub struct HostVolumes;

/// One open host volume.
pub struct HostVolume {
    volume: selium_guest::volume::Volume,
}

impl Volumes for HostVolumes {
    type Handle = HostVolume;

    fn list(&self, tenant: Option<&str>) -> Result<Vec<String>, Failure> {
        selium_guest::volume::list(tenant).map_err(host_failure)
    }

    fn open(&self, tenant: Option<&str>, name: &str) -> Result<Self::Handle, Failure> {
        let volume = selium_guest::volume::Volume::open(name, tenant).map_err(host_failure)?;
        Ok(HostVolume { volume })
    }

    fn remove(&self, tenant: Option<&str>, name: &str) -> Result<u64, Failure> {
        selium_guest::volume::remove(name, tenant).map_err(host_failure)
    }
}

impl VolumeHandle for HostVolume {
    fn append(&self, bytes: &[u8]) -> Result<u64, Failure> {
        self.volume.append(bytes).map_err(host_failure)
    }

    fn read_all(&self) -> Result<Vec<u8>, Failure> {
        self.volume.read_all().map_err(host_failure)
    }

    fn len(&self) -> Result<u64, Failure> {
        self.volume.len().map_err(host_failure)
    }

    fn sync(&self) -> Result<(), Failure> {
        self.volume.sync().map_err(host_failure)
    }
}

/// Handles one decoded request against the store's state.
fn dispatch(store: &Store<HostVolumes>, tenant: &str, request: StoreRequest) -> StoreResponse {
    let outcome = match request {
        StoreRequest::OpenLog { log } => store.open_log(tenant, &log).map(|()| StoreResponse::Ok),
        StoreRequest::Append {
            log,
            timestamp_ms,
            headers,
            payload,
        } => {
            let headers = headers
                .into_iter()
                .map(|header| (header.key, header.value))
                .collect();
            store
                .append(tenant, &log, timestamp_ms, headers, payload)
                .map(|sequence| StoreResponse::Appended { sequence })
        }
        StoreRequest::Replay {
            log,
            from_sequence,
            limit,
        } => store
            .replay(tenant, &log, from_sequence, limit)
            .map(|records| StoreResponse::Replay {
                records: records.into_iter().map(to_wire_record).collect(),
            }),
        StoreRequest::WriteCheckpoint {
            log,
            checkpoint,
            sequence,
        } => store
            .write_checkpoint(tenant, &log, &checkpoint, sequence)
            .map(|()| StoreResponse::Ok),
        StoreRequest::ReadCheckpoint { log, checkpoint } => store
            .read_checkpoint(tenant, &log, &checkpoint)
            .map(|sequence| match sequence {
                Some(sequence) => StoreResponse::Checkpoint { sequence },
                None => StoreResponse::CheckpointMissing,
            }),
        StoreRequest::Sync { log } => store.sync_log(tenant, &log).map(|()| StoreResponse::Ok),
        StoreRequest::PutBlob {
            store: name,
            payload,
        } => store
            .put_blob(tenant, &name, &payload)
            .map(|blob_id| StoreResponse::BlobId { blob_id }),
        StoreRequest::GetBlob {
            store: name,
            blob_id,
        } => store
            .get_blob(tenant, &name, &blob_id)
            .map(|bytes| match bytes {
                Some(payload) => StoreResponse::Bytes { payload },
                None => StoreResponse::NotFound,
            }),
        StoreRequest::SetManifest {
            store: name,
            manifest,
            blob_id,
        } => store
            .set_manifest(tenant, &name, &manifest, &blob_id)
            .map(|()| StoreResponse::Ok),
        StoreRequest::GetManifest {
            store: name,
            manifest,
        } => store
            .get_manifest(tenant, &name, &manifest)
            .map(|blob_id| match blob_id {
                Some(blob_id) => StoreResponse::BlobId { blob_id },
                None => StoreResponse::NotFound,
            }),
        StoreRequest::ResolveSlice {
            store: name,
            manifest,
            blob_id,
        } => {
            let resolved = match (manifest, blob_id) {
                (Some(manifest), _) => {
                    store
                        .get_manifest(tenant, &name, &manifest)
                        .and_then(|id| match id {
                            Some(id) => store.resolve_slice(tenant, &name, &id),
                            None => Ok(None),
                        })
                }
                (_, Some(blob_id)) => store.resolve_slice(tenant, &name, &blob_id),
                (None, None) => Err(Failure {
                    code: "MalformedRequest",
                    message: "ResolveSlice needs a manifest or blob id".to_string(),
                }),
            };
            resolved.map(|slice| match slice {
                Some((volume, len)) => StoreResponse::Slice {
                    volume,
                    offset: 0,
                    len,
                },
                None => StoreResponse::NotFound,
            })
        }
    };

    outcome.unwrap_or_else(|failure| error_response(failure.code, failure.message))
}

fn error_response(code: &str, message: String) -> StoreResponse {
    StoreResponse::Error {
        code: code.to_string(),
        message,
    }
}

/// Serves one session, deriving its tenant from the delivering process.
async fn handle_connection(
    store: Arc<Store<HostVolumes>>,
    mut connection: RpcConnection<StoreRequest, StoreResponse>,
) {
    let client_process_id = connection.client_process_id();
    let tenant = match process_tenant(client_process_id) {
        Ok(Some(tenant)) => tenant,
        Ok(None) => String::new(),
        Err(error) => {
            // A session whose tenant cannot be resolved is refused, never
            // served under a guessed namespace.
            warn!("store: refusing session from process {client_process_id}: {error}");
            return;
        }
    };
    info!(client_process_id, tenant = %tenant, "store: session opened");

    loop {
        let request = match connection.recv().await {
            Ok(request) => request,
            Err(RpcError::ConnectionClosed) => break,
            Err(error) => {
                warn!("store: session receive failed: {error}");
                break;
            }
        };
        let response = match request.payload() {
            Ok(payload) => dispatch(&store, &tenant, payload),
            Err(error) => error_response("Malformed", error.to_string()),
        };
        if let Err(error) = request.reply(response).await {
            warn!("store: reply failed: {error}");
            break;
        }
    }
}

fn host_failure(error: selium_guest::GuestError) -> Failure {
    Failure {
        code: "Host",
        message: error.to_string(),
    }
}

/// Store entrypoint: registers the route, reports readiness, then serves.
#[entrypoint]
async fn store_main(mut ctx: Context) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("store: started");

    let listener = ResourceListener::create().with_context(|| "store: listener create failed")?;
    let route = ctx
        .serve(Serve {
            path: vec!["store".to_string()],
            target: ResourceTarget {
                uri: String::new(),
                host_id: String::new(),
                resource_id: listener.descriptor().shared_id,
                interface: None,
                tenant: None,
                class: ResourceClass::HostQueue,
                labels: Vec::new(),
            },
            default: false,
        })
        .await
        .with_context(|| "store: route registration failed")?;
    info!(route = %route, "store: route registered");
    // Health is published state: advertise the store's health.
    if let Err(error) = ctx.publish_health("store").await {
        warn!("store: health publish failed: {error}");
    }

    let store = Arc::new(Store::new(HostVolumes));
    loop {
        let incoming = match listener.recv().await {
            Ok(connection) => connection,
            Err(error) => {
                warn!("store: accept failed: {error}");
                continue;
            }
        };
        let connection =
            match selium_shm::rpc::accept::<StoreRequest, StoreResponse>(incoming.into()) {
                Ok(connection) => connection,
                Err(error) => {
                    warn!("store: rpc accept failed: {error}");
                    continue;
                }
            };
        spawn(handle_connection(store.clone(), connection));
    }
}

fn to_wire_record(record: format::Record) -> StoreRecord {
    StoreRecord {
        sequence: record.sequence,
        timestamp_ms: record.timestamp_ms,
        headers: record
            .headers
            .into_iter()
            .map(|(key, value)| StoreRecordHeader { key, value })
            .collect(),
        payload: record.payload,
    }
}

#[cfg(test)]
mod tests;

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `selium-store` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
