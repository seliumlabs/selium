use selium_abi::{HostQueueDescriptor, HostcallOutput, HostcallRequest};

use crate::{
    GuestError, Result,
    hostcall::{hostcall_async, hostcall_ready},
};

/// Trait for accepting raw connections and turning them into typed resources.
pub trait Accept {
    /// The typed resource produced by acceptance.
    type Item;
    /// Accepts a raw incoming connection and produces a typed resource.
    fn accept(connection: IncomingConnection) -> Result<Self::Item>;
}

/// An incoming connection from a client.
#[derive(Debug, Clone)]
pub struct IncomingConnection {
    /// Process id of the connecting client.
    pub client_process_id: u64,
    /// Shared region id of the session.
    pub shared_id: u64,
    /// Opaque metadata payload attached by the sender (e.g. the
    /// authenticated peer identity). Empty when the sender omitted it.
    pub metadata: Vec<u8>,
}

/// A sender that enqueues connections into a host-mediated queue.
#[derive(Clone, Debug)]
pub struct ResourceSender {
    descriptor: selium_abi::HostQueueDescriptor,
}

/// A listener that accepts incoming typed connections from a host-mediated queue.
#[derive(Clone, Debug)]
pub struct ResourceListener {
    descriptor: selium_abi::HostQueueDescriptor,
    expected_sender: Option<selium_abi::ProcessId>,
}

impl ResourceSender {
    /// Creates a new sender by attaching to an existing shared queue.
    pub fn attach(shared_id: u64) -> Result<Self> {
        match hostcall_ready(HostcallRequest::HostQueueAttach { shared_id })? {
            HostcallOutput::HostQueue(descriptor) => Ok(Self { descriptor }),
            _ => Err(GuestError::UnexpectedHostcallOutput),
        }
    }

    /// Returns the queue descriptor.
    pub fn descriptor(&self) -> selium_abi::HostQueueDescriptor {
        self.descriptor
    }

    /// Sends a value to the connection queue, with no metadata.
    pub async fn send(&self, value: u64) -> Result<()> {
        self.send_with_metadata(value, Vec::new()).await
    }

    /// Sends a value to the connection queue, with an opaque metadata payload.
    ///
    /// The runtime bounds the payload (see
    /// [`METADATA_MAX_BYTES`](selium_abi::METADATA_MAX_BYTES)); oversized
    /// payloads are rejected locally before crossing the hostcall boundary.
    pub async fn send_with_metadata(&self, value: u64, metadata: Vec<u8>) -> Result<()> {
        if metadata.len() > selium_abi::METADATA_MAX_BYTES {
            return Err(GuestError::Host(format!(
                "handoff metadata length {} exceeds maximum {}",
                metadata.len(),
                selium_abi::METADATA_MAX_BYTES
            )));
        }
        match hostcall_async(HostcallRequest::HostQueueSend {
            local_id: self.descriptor.local_id,
            value,
            metadata,
        })
        .await?
        {
            HostcallOutput::Empty => Ok(()),
            _ => Err(GuestError::UnexpectedHostcallOutput),
        }
    }
}

impl selium_wire::Rendezvous for ResourceSender {
    async fn send(&self, shared_id: u64) -> selium_wire::error::Result<()> {
        Self::send(self, shared_id)
            .await
            .map_err(|error| selium_wire::error::Error::Guest(error.to_string()))
    }

    async fn recv(&self) -> selium_wire::error::Result<selium_wire::rpc::IncomingConnection> {
        Err(selium_wire::error::Error::Guest(
            "ResourceSender cannot receive connections".to_string(),
        ))
    }
}

impl ResourceListener {
    /// Creates a new host-mediated connection queue, minted under the
    /// calling process's own tenant.
    pub fn create() -> Result<Self> {
        Self::create_for_tenant(None)
    }

    /// Creates a new host-mediated connection queue minted under the
    /// supplied serving tenant, mirroring region allocation's principal
    /// provenance. `None` mints under the caller's own tenant; a tenant
    /// differing from the caller's own requires cross-tenant allocation
    /// authority (root principal or tenant-scoped delegation).
    pub fn create_for_tenant(serving_tenant: Option<&str>) -> Result<Self> {
        let request = HostcallRequest::HostQueueCreate {
            serving_tenant: serving_tenant.map(str::to_string),
        };
        match hostcall_ready(request)? {
            HostcallOutput::HostQueue(descriptor) => Ok(Self {
                descriptor,
                expected_sender: None,
            }),
            _ => Err(GuestError::UnexpectedHostcallOutput),
        }
    }

    /// Creates `Self` from an externally created queue.
    pub fn from_queue(descriptor: HostQueueDescriptor) -> Self {
        Self {
            descriptor,
            expected_sender: None,
        }
    }

    /// Attaches to an existing shared queue.
    pub fn attach(shared_id: u64) -> Result<Self> {
        match hostcall_ready(HostcallRequest::HostQueueAttach { shared_id })? {
            HostcallOutput::HostQueue(descriptor) => Ok(Self {
                descriptor,
                expected_sender: None,
            }),
            _ => Err(GuestError::UnexpectedHostcallOutput),
        }
    }

    /// Pins the only process allowed to deliver handoffs to this listener.
    ///
    /// Handoff metadata is sender-controlled and opaque: without a pin, any
    /// guest able to attach the queue can forge the payload (e.g. a fake
    /// authenticated identity for the bridge-server). Pinning ties the
    /// handoff to a specific trusted sender — typically the protocol
    /// connector resolved via
    /// [`resolve_protocol_handler`](crate::resolve_protocol_handler).
    /// Handoffs from other senders are refused by attaching and immediately
    /// closing the delivered region, so the sender observes EOF instead of
    /// parking on a region nobody attaches.
    pub fn expect_sender(&mut self, sender: selium_abi::ProcessId) {
        self.expected_sender = Some(sender);
    }

    /// Returns the pinned sender, if [`Self::expect_sender`] was called.
    pub fn expected_sender(&self) -> Option<selium_abi::ProcessId> {
        self.expected_sender
    }

    /// Returns the queue descriptor.
    pub fn descriptor(&self) -> selium_abi::HostQueueDescriptor {
        self.descriptor
    }

    /// Accepts the next incoming connection, mapping it through `A::accept`.
    pub async fn accept<A: Accept>(&self) -> Result<A::Item> {
        let connection = self.recv().await?;
        A::accept(connection)
    }

    /// Receives the next pending connection entry.
    ///
    /// When a sender is pinned via [`Self::expect_sender`], handoffs from any
    /// other process are refused (attach-then-close) and never surfaced.
    pub async fn recv(&self) -> Result<IncomingConnection> {
        loop {
            let incoming = match hostcall_async(HostcallRequest::HostQueueRecv {
                local_id: self.descriptor.local_id,
            })
            .await?
            {
                HostcallOutput::ConnectionInfo {
                    client_process_id,
                    value,
                    metadata,
                } => IncomingConnection {
                    client_process_id,
                    shared_id: value,
                    metadata,
                },
                _ => return Err(GuestError::UnexpectedHostcallOutput),
            };

            if let Some(expected) = self.expected_sender
                && incoming.client_process_id != expected
            {
                crate::warn!(
                    expected,
                    sender = incoming.client_process_id,
                    shared_id = incoming.shared_id,
                    "refusing handoff from unpinned sender; closing delivered region"
                );
                refuse_handoff(incoming.shared_id);
                continue;
            }

            return Ok(incoming);
        }
    }
}

impl selium_wire::Rendezvous for ResourceListener {
    async fn send(&self, _shared_id: u64) -> selium_wire::error::Result<()> {
        Err(selium_wire::error::Error::Guest(
            "ResourceListener cannot send connections".to_string(),
        ))
    }

    async fn recv(&self) -> selium_wire::error::Result<selium_wire::rpc::IncomingConnection> {
        let connection = self
            .recv()
            .await
            .map_err(|error| selium_wire::error::Error::Guest(error.to_string()))?;
        Ok(selium_wire::rpc::IncomingConnection {
            client_process_id: connection.client_process_id,
            shared_id: connection.shared_id,
        })
    }
}

impl From<IncomingConnection> for selium_wire::rpc::IncomingConnection {
    fn from(connection: IncomingConnection) -> Self {
        Self {
            client_process_id: connection.client_process_id,
            shared_id: connection.shared_id,
        }
    }
}

/// Refuses a delivered handoff by attaching the region then closing it, so
/// the sender observes EOF instead of parking on a region nobody attaches.
/// Best-effort: a handoff value that is not an attachable region is simply
/// discarded with a warning.
fn refuse_handoff(shared_id: u64) {
    match crate::net::bytes::ByteStream::attach_blocking(shared_id) {
        Ok(stream) => drop(stream),
        Err(error) => {
            crate::warn!(
                shared_id,
                "handoff refusal could not attach region: {error}"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_listener_create_fails_outside_guest() {
        let result = ResourceListener::create();
        let _ = result.unwrap_err();
    }

    #[test]
    fn resource_sender_attach_fails_with_invalid_shared_id() {
        let result = ResourceSender::attach(0);
        let _ = result.unwrap_err();
    }

    #[test]
    fn resource_listener_attach_fails_with_invalid_shared_id() {
        let result = ResourceListener::attach(0);
        let _ = result.unwrap_err();
    }
}
