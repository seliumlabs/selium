## Why

Selium has no RPC mechanism. Guests cannot securely make request/reply calls to other guests or external services. The existing pub/sub and channel patterns are unidirectional, multi-writer, and have no built-in request correlation or tenant isolation. A malicious guest with access to a shared memory region can read, tamper with, or impersonate other tenants' messages. Secure RPC is the missing building block for service-to-service communication — particularly the discovery service, which needs it to resolve URI lookups.

## What Changes

- **BREAKING**: `FrameHeader` layout changes from 8 bytes (`len: u32, flags: u16, writer_id: u16`) to 12 bytes (`len: u32, tag: u32, flags: u8, _reserved: [u8; 3]`). The `writer_id: u16` field is replaced by `tag: u32`, which serves as `writer_id` in pub/sub contexts and `correlation_id` in RPC contexts.
- New `selium-io::rpc` module providing `RpcClient<Req, Rep>`, `RpcConnection<Req, Rep>`, `RpcRequest<Req, Rep>`, and `Accept` trait.
- New `SharedRegionBuilder` for constructing multi-memory shared regions (replaces ad-hoc region setup for RPC sessions).
- New `ResourceListener` and `ResourceSender` types for host-mediated connection establishment.
- New hostcall variants `HostQueueSend` and `HostQueueRecv` in `selium-abi` for the connection handshake.
- New `Context` type injected into guest `#[entrypoint]` functions, providing access to the discovery RPC client.
- Context bootstrapping in `selium-runtime` to inject discovery handles into each guest process.

## Capabilities

### New Capabilities

- `secure-rpc`: Bidirectional request/reply communication between guests with per-connection memory isolation, host-enforced capability-gated connection establishment, and typed serialisation.
- `resource-handshake`: Host-mediated queue for establishing shared memory connections between guests. Includes `ResourceListener` (server side) and `ResourceSender` (client side), and the `Accept` trait for typed resource acceptance.
- `shared-region-builder`: Construction and layout management of multi-memory shared regions, including alignment, sealing, and positional sub-memory discovery.
- `guest-context`: Dependency injection of system resources (starting with discovery) into guest entrypoints via a `Context` object.

### Modified Capabilities

- `selium-abi`: New `HostcallRequest` variants (`HostQueueSend`, `HostQueueRecv`) and `HostcallOutput` variant for connection queue operations.
- `selium-guest`: Updated `#[entrypoint]` macro to accept and inject `Context`. New types for resource handles and connection queues.

## Impact

- **selium-io**: New `rpc` module. `FrameHeader` format change affects all existing consumers (`channels/`, `pubsub.rs`, `tables.rs`). `SharedRegion` gains builder pattern and sub-memory layout support.
- **selium-abi**: Two new hostcall variants for connection queue send/receive.
- **selium-guest**: New `Context` type, `ResourceListener`, `ResourceSender`, updated `#[entrypoint]` macro.
- **selium-runtime**: Host-side implementation of connection queue (enqueuing incoming connections, capability checking, notifying server guests). Guest bootstrap updates to inject `Context`.
- **selium-discovery**: First consumer of the RPC system — wiring up `DISCOVERY_EXCHANGE` as an `RpcServer<DiscoveryRequest, DiscoveryResponse>`.