//! Network hostcall family.
//!
//! The dispatcher in the parent module routes `TcpBind`, `TcpConnect`, and
//! `UdpBind` here. Each handler keeps its authorisation and ownership
//! bookkeeping beside the primitive it grants, so the ring/queue substrate in
//! `crate::network` stays policy-free. Behaviour is unchanged by the split:
//! the dispatcher's arms are the only callers, and the hostcall suite asserts
//! the same codes, messages, and ownership records.

use selium_abi::{AbiError, AbiErrorCode, Capability, HostcallOutput, ProcessId, ResourceClass};

use super::HostOperationState;
use crate::Runtime;

impl Runtime {
    /// Binds a TCP listener and claims its queue, listener, and shared-region
    /// handles for the calling process.
    pub(super) fn hostcall_tcp_bind(
        &self,
        process_id: ProcessId,
        address: String,
    ) -> std::result::Result<HostOperationState, AbiError> {
        let addr: std::net::SocketAddr = address.parse().map_err(|_e| {
            AbiError::new(
                AbiErrorCode::MalformedPayload,
                format!("address must be an IP literal, got: {address}"),
            )
        })?;
        let uri = format!("tcp://{addr}");
        self.require_with_uri(
            process_id,
            Capability::Network,
            ResourceClass::TcpListener,
            None,
            uri,
        )?;
        let descriptor = crate::network::tcp_bind(self, process_id, address)
            .map_err(|e| AbiError::new(AbiErrorCode::Internal, e.to_string()))?;
        self.claim_local_handle(process_id, ResourceClass::TcpListener, descriptor.local_id);
        // The descriptor doubles as a connection queue: HostQueueRecv
        // checks ownership under the HostQueue class, so claim both.
        self.claim_local_handle(process_id, ResourceClass::HostQueue, descriptor.local_id);
        self.claim_shared_resource(process_id, ResourceClass::TcpListener, descriptor.shared_id);
        Ok(HostOperationState::Ready(HostcallOutput::HostQueue(
            descriptor,
        )))
    }

    /// Opens a TCP stream, claiming the stream and its ring region.
    pub(super) fn hostcall_tcp_connect(
        &self,
        process_id: ProcessId,
        address: String,
    ) -> std::result::Result<HostOperationState, AbiError> {
        let addr: std::net::SocketAddr = address.parse().map_err(|_e| {
            AbiError::new(
                AbiErrorCode::MalformedPayload,
                format!("address must be an IP literal, got: {address}"),
            )
        })?;
        let uri = format!("tcp://{addr}");
        self.require_with_uri(
            process_id,
            Capability::Network,
            ResourceClass::TcpStream,
            None,
            uri,
        )?;
        let descriptor = crate::network::tcp_connect(self, process_id, address)
            .map_err(|e| AbiError::new(AbiErrorCode::Internal, e.to_string()))?;
        self.claim_local_handle(process_id, ResourceClass::TcpStream, descriptor.shared_id);
        self.claim_shared_resource(process_id, ResourceClass::TcpStream, descriptor.shared_id);
        // The guest attaches the stream's ring region via AttachRegion,
        // whose authorisation is keyed on SharedRegion ownership.
        self.claim_shared_resource(
            process_id,
            ResourceClass::SharedRegion,
            descriptor.shared_id,
        );
        Ok(HostOperationState::Ready(HostcallOutput::SharedRegion(
            descriptor,
        )))
    }

    /// Binds a UDP socket, claiming the socket and its ring region.
    pub(super) fn hostcall_udp_bind(
        &self,
        process_id: ProcessId,
        address: String,
    ) -> std::result::Result<HostOperationState, AbiError> {
        let addr: std::net::SocketAddr = address.parse().map_err(|_e| {
            AbiError::new(
                AbiErrorCode::MalformedPayload,
                format!("address must be an IP literal, got: {address}"),
            )
        })?;
        let uri = format!("udp://{addr}");
        self.require_with_uri(
            process_id,
            Capability::Network,
            ResourceClass::UdpSocket,
            None,
            uri,
        )?;
        let descriptor = crate::network::udp_bind(self, address)
            .map_err(|e| AbiError::new(AbiErrorCode::Internal, e.to_string()))?;
        self.claim_local_handle(process_id, ResourceClass::UdpSocket, descriptor.shared_id);
        self.claim_shared_resource(process_id, ResourceClass::UdpSocket, descriptor.shared_id);
        // The guest attaches the socket's ring region via AttachRegion,
        // whose authorisation is keyed on SharedRegion ownership.
        self.claim_shared_resource(
            process_id,
            ResourceClass::SharedRegion,
            descriptor.shared_id,
        );
        Ok(HostOperationState::Ready(HostcallOutput::SharedRegion(
            descriptor,
        )))
    }
}
