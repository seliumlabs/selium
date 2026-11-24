use std::sync::Arc;

use selium_service::{StoreRecord, StoreRecordHeader, StoreRequest, StoreResponse};
use selium_shm::rpc::{self, OwnedRpcClient};

use crate::{GuestError, Result, client_session::SessionState};

/// Reply ring capacity for a store session; sized for replay batches.
const RPC_REP_CAPACITY: u64 = 256 * 1024;
/// Request ring capacity for a store session.
const RPC_REQ_CAPACITY: u64 = 64 * 1024;
/// The root-namespace route the durable store service serves on.
pub const STORE_ROUTE: &str = "sel:///store";

/// A typed client of the `selium-store` service.
///
/// The client owns one shared-memory RPC session and is cheap to clone, so
/// every handle obtained from it shares the same session. Requests are
/// serialised cooperatively (a guest task yields rather than blocks), which
/// keeps concurrent appends to one log well-defined — exactly the ordering the
/// pre-change synchronous storage hostcalls provided.
#[derive(Clone)]
pub struct StoreClient {
    inner: Arc<StoreClientInner>,
}

struct StoreClientInner {
    /// Cancellation-safe checkout: one request owns the session for its
    /// duration, and dropping the request future closes or restores it before
    /// reopening the gate.
    session: SessionState<OwnedRpcClient<StoreRequest, StoreResponse>>,
}

/// A handle to a durable log served by the store.
#[derive(Clone)]
pub struct DurableLog {
    client: StoreClient,
    name: String,
}

/// A handle to a blob store served by the store.
#[derive(Clone)]
pub struct BlobStore {
    client: StoreClient,
    name: String,
}

/// A durable slice naming a stored blob's bytes: the volume holding them and
/// the byte range within it. Hand a slice to `ProcessStart` so module bytes
/// never round-trip through a guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slice {
    /// Volume name holding the blob's bytes.
    pub volume: String,
    /// Byte offset of the blob within the volume.
    pub offset: u64,
    /// Byte length of the blob.
    pub len: u64,
}

impl StoreClient {
    /// Wraps an established RPC session to the store service.
    pub fn from_session(session: OwnedRpcClient<StoreRequest, StoreResponse>) -> Self {
        Self {
            inner: Arc::new(StoreClientInner {
                session: SessionState::new(session),
            }),
        }
    }

    /// Sends one request and awaits its reply.
    ///
    /// A store-reported failure is returned as a [`GuestError`]; the session
    /// survives a `StoreResponse::Error`. Cancelling the future closes the
    /// torn session and reopens the gate so later requests are admitted and
    /// fail clearly rather than spinning.
    pub async fn request(&self, request: StoreRequest) -> Result<StoreResponse> {
        let mut guard = self.inner.session.acquire().await;
        let Some(session) = guard.session() else {
            return Err(GuestError::Host("store session unavailable".to_string()));
        };
        let reply = session.request(request).await;
        match reply {
            Ok(reply) => {
                // Restore before reopening the gate: the transport is intact
                // and the session survives an application-level error.
                guard.restore();
                match reply {
                    StoreResponse::Error { code, message } => Err(store_error(&code, &message)),
                    other => Ok(other),
                }
            }
            Err(error) => {
                // A transport failure leaves the session torn; the guard drops
                // it and reopens the gate on the way out.
                Err(GuestError::from(error).context("store request"))
            }
        }
    }

    /// Returns once the store answers a no-op request, so a caller can
    /// distinguish an unreachable store from a later operation failure.
    pub async fn ping(&self) -> Result<()> {
        match self
            .request(StoreRequest::OpenLog { log: String::new() })
            .await?
        {
            StoreResponse::Ok => Ok(()),
            _ => Err(GuestError::Host("unexpected store ping reply".to_string())),
        }
    }
}

impl DurableLog {
    /// Opens or creates a durable log by name.
    pub async fn open(client: StoreClient, name: impl Into<String>) -> Result<Self> {
        let log = Self {
            client,
            name: name.into(),
        };
        match log
            .client
            .request(StoreRequest::OpenLog {
                log: log.name.clone(),
            })
            .await?
        {
            StoreResponse::Ok => Ok(log),
            other => Err(unexpected(&other)),
        }
    }

    /// Returns the log's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Appends a record and returns its sequence number.
    ///
    /// The store batches appends for throughput, so a successful append is not
    /// by itself a durability acknowledgement. A caller that reports success to
    /// someone else (identity, accounting, desired state) must also call
    /// [`Self::sync`] before acknowledging.
    pub async fn append(
        &self,
        timestamp_ms: u64,
        headers: Vec<(String, String)>,
        payload: Vec<u8>,
    ) -> Result<u64> {
        let headers = headers
            .into_iter()
            .map(|(key, value)| StoreRecordHeader { key, value })
            .collect();
        match self
            .client
            .request(StoreRequest::Append {
                log: self.name.clone(),
                timestamp_ms,
                headers,
                payload,
            })
            .await?
        {
            StoreResponse::Appended { sequence } => Ok(sequence),
            other => Err(unexpected(&other)),
        }
    }

    /// Forces the log's appended records to stable storage.
    ///
    /// The durable-commit boundary: a caller that acknowledges a mutation only
    /// after this returns (`Ok`) may claim the record survives a power cycle.
    pub async fn sync(&self) -> Result<()> {
        match self
            .client
            .request(StoreRequest::Sync {
                log: self.name.clone(),
            })
            .await?
        {
            StoreResponse::Ok => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Replays records from an optional sequence with a maximum record count.
    pub async fn replay(&self, from_sequence: Option<u64>, limit: u32) -> Result<Vec<StoreRecord>> {
        match self
            .client
            .request(StoreRequest::Replay {
                log: self.name.clone(),
                from_sequence: from_sequence.unwrap_or(0),
                limit,
            })
            .await?
        {
            StoreResponse::Replay { records } => Ok(records),
            other => Err(unexpected(&other)),
        }
    }

    /// Records a named checkpoint at a sequence number.
    pub async fn checkpoint(&self, name: impl Into<String>, sequence: u64) -> Result<()> {
        match self
            .client
            .request(StoreRequest::WriteCheckpoint {
                log: self.name.clone(),
                checkpoint: name.into(),
                sequence,
            })
            .await?
        {
            StoreResponse::Ok => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Reads the sequence number for a named checkpoint.
    pub async fn checkpoint_sequence(&self, name: impl Into<String>) -> Result<Option<u64>> {
        match self
            .client
            .request(StoreRequest::ReadCheckpoint {
                log: self.name.clone(),
                checkpoint: name.into(),
            })
            .await?
        {
            StoreResponse::Checkpoint { sequence } => Ok(Some(sequence)),
            StoreResponse::CheckpointMissing => Ok(None),
            other => Err(unexpected(&other)),
        }
    }
}

impl std::fmt::Debug for DurableLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableLog")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl BlobStore {
    /// Opens or creates a blob store by name.
    pub async fn open(client: StoreClient, name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            client,
            name: name.into(),
        })
    }

    /// Returns the blob store's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stores bytes and returns the content-derived blob id.
    pub async fn put(&self, bytes: Vec<u8>) -> Result<String> {
        match self
            .client
            .request(StoreRequest::PutBlob {
                store: self.name.clone(),
                payload: bytes,
            })
            .await?
        {
            StoreResponse::BlobId { blob_id } => Ok(blob_id),
            other => Err(unexpected(&other)),
        }
    }

    /// Reads bytes for a blob id, if the store holds it.
    pub async fn get(&self, blob_id: impl Into<String>) -> Result<Option<Vec<u8>>> {
        match self
            .client
            .request(StoreRequest::GetBlob {
                store: self.name.clone(),
                blob_id: blob_id.into(),
            })
            .await?
        {
            StoreResponse::Bytes { payload } => Ok(Some(payload)),
            StoreResponse::NotFound => Ok(None),
            other => Err(unexpected(&other)),
        }
    }

    /// Associates a manifest name with an existing blob id.
    pub async fn set_manifest(
        &self,
        name: impl Into<String>,
        blob_id: impl Into<String>,
    ) -> Result<()> {
        match self
            .client
            .request(StoreRequest::SetManifest {
                store: self.name.clone(),
                manifest: name.into(),
                blob_id: blob_id.into(),
            })
            .await?
        {
            StoreResponse::Ok => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Reads the blob id associated with a manifest name.
    pub async fn manifest(&self, name: impl Into<String>) -> Result<Option<String>> {
        match self
            .client
            .request(StoreRequest::GetManifest {
                store: self.name.clone(),
                manifest: name.into(),
            })
            .await?
        {
            StoreResponse::BlobId { blob_id } => Ok(Some(blob_id)),
            StoreResponse::NotFound => Ok(None),
            other => Err(unexpected(&other)),
        }
    }

    /// Resolves a manifest to a durable slice naming the blob's bytes.
    ///
    /// Blob volumes are append-only and never rewritten, so a slice a caller
    /// holds stays valid and never yields different bytes. Returns `None` when
    /// the manifest is unknown in the caller's tenant.
    pub async fn resolve_slice(&self, manifest: impl Into<String>) -> Result<Option<Slice>> {
        match self
            .client
            .request(StoreRequest::ResolveSlice {
                store: self.name.clone(),
                manifest: Some(manifest.into()),
                blob_id: None,
            })
            .await?
        {
            StoreResponse::Slice {
                volume,
                offset,
                len,
            } => Ok(Some(Slice {
                volume,
                offset,
                len,
            })),
            StoreResponse::NotFound => Ok(None),
            other => Err(unexpected(&other)),
        }
    }
}

impl std::fmt::Debug for BlobStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlobStore")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Establishes a store session over an attached rendezvous.
///
/// Used by [`crate::Context`], which resolves the store route itself.
pub async fn connect<R>(rendezvous: R) -> Result<StoreClient>
where
    R: selium_wire::rpc::Rendezvous,
{
    let session = rpc::connect(rendezvous, RPC_REQ_CAPACITY, RPC_REP_CAPACITY)
        .await
        .map_err(|error| GuestError::from(error).context("store connect"))?;
    Ok(StoreClient::from_session(session))
}

/// Maps a store-reported error code onto a [`GuestError`].
fn store_error(code: &str, message: &str) -> GuestError {
    if code == "PermissionDenied" {
        GuestError::PermissionDenied(message.to_string())
    } else {
        GuestError::Host(format!("{code}: {message}"))
    }
}

fn unexpected(response: &StoreResponse) -> GuestError {
    GuestError::Host(format!("unexpected store response: {response:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> bool {
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        future.poll(&mut context).is_pending()
    }

    // scenario: selium-guest/shared-clients-are-cancellation-safe/store-request-is-cancelled
    #[tokio::test]
    async fn cancelled_store_request_does_not_strand_the_client() {
        let (session, server) =
            crate::client_session::test_session_pair::<StoreRequest, StoreResponse>().await;
        let client = StoreClient::from_session(session);

        let mut request = Box::pin(client.request(StoreRequest::OpenLog {
            log: "cancelled".to_string(),
        }));
        assert!(
            pending_once(request.as_mut()),
            "the request should park awaiting the store's reply"
        );
        drop(request);

        let error = client
            .request(StoreRequest::OpenLog {
                log: "after-cancel".to_string(),
            })
            .await
            .expect_err("the cancelled session is closed");
        assert!(
            matches!(&error, GuestError::Host(message) if message.contains("unavailable")),
            "a cancelled session must fail clearly, not hang: {error:?}"
        );
        server.abort();
    }
}
