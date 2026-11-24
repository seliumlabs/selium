use std::sync::Arc;

use selium_service::{ControlRequest, ControlResponse};
use selium_shm::rpc::{self, OwnedRpcClient};

use crate::{GuestError, Result, client_session::SessionState};

/// The root-namespace route the control-plane service serves on.
pub const CONTROL_ROUTE: &str = "sel:///control";
/// Reply ring capacity for a control-plane session; sized for catalogue reads.
const RPC_REP_CAPACITY: u64 = 64 * 1024;
/// Request ring capacity for a control-plane session.
const RPC_REQ_CAPACITY: u64 = 64 * 1024;

/// A typed client of the `selium-control-plane` service.
///
/// The client owns one shared-memory RPC session and is cheap to clone. The
/// resource catalogue lives behind this surface: registration and lookup are
/// RPCs scoped to the session's tenant, so a guest never sees host-side naming
/// metadata.
#[derive(Clone)]
pub struct ControlClient {
    inner: Arc<ControlClientInner>,
}

struct ControlClientInner {
    /// Cancellation-safe checkout: one request owns the session for its
    /// duration, and dropping the request future closes or restores it before
    /// reopening the gate.
    session: SessionState<OwnedRpcClient<ControlRequest, ControlResponse>>,
}

impl ControlClient {
    /// Wraps an established RPC session to the control-plane service.
    pub fn from_session(session: OwnedRpcClient<ControlRequest, ControlResponse>) -> Self {
        Self {
            inner: Arc::new(ControlClientInner {
                session: SessionState::new(session),
            }),
        }
    }

    /// Sends one request and awaits its reply.
    ///
    /// A control-plane-reported failure is returned as a [`GuestError`]; the
    /// session survives a `ControlResponse::Error`. Cancelling the future
    /// closes the torn session and reopens the gate so later requests are
    /// admitted and fail clearly rather than spinning.
    pub async fn request(&self, request: ControlRequest) -> Result<ControlResponse> {
        let mut guard = self.inner.session.acquire().await;
        let Some(session) = guard.session() else {
            return Err(GuestError::Host(
                "control-plane session unavailable".to_string(),
            ));
        };
        let reply = session.request(request).await;
        match reply {
            Ok(reply) => {
                // Restore before reopening the gate: the transport is intact
                // and the session survives an application-level error.
                guard.restore();
                match reply {
                    ControlResponse::Error { step, context } => {
                        Err(GuestError::Host(format!("{step}: {context}")))
                    }
                    other => Ok(other),
                }
            }
            Err(error) => Err(GuestError::from(error).context("control request")),
        }
    }
}

/// Establishes a control-plane session over an attached rendezvous.
///
/// Used by [`crate::Context`], which resolves the control route itself.
pub async fn connect<R>(rendezvous: R) -> Result<ControlClient>
where
    R: selium_wire::rpc::Rendezvous,
{
    let session = rpc::connect(rendezvous, RPC_REQ_CAPACITY, RPC_REP_CAPACITY)
        .await
        .map_err(|error| GuestError::from(error).context("control connect"))?;
    Ok(ControlClient::from_session(session))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> bool {
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        future.poll(&mut context).is_pending()
    }

    #[tokio::test]
    async fn cancelled_control_request_does_not_strand_the_client() {
        let (session, server) =
            crate::client_session::test_session_pair::<ControlRequest, ControlResponse>().await;
        let client = ControlClient::from_session(session);

        let mut request = Box::pin(client.request(ControlRequest::Status {
            workload_id: "cancelled".to_string(),
        }));
        assert!(
            pending_once(request.as_mut()),
            "the request should park awaiting the control plane's reply"
        );
        drop(request);

        let error = client
            .request(ControlRequest::Status {
                workload_id: "after-cancel".to_string(),
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
