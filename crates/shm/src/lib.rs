//! Selium shared-memory ring channels and transport.
//!
//! This crate builds on [`selium_memory`] and [`selium_wire`] to provide
//! process-local and cross-process shared-memory channels, plus a
//! [`MessageTransport`] implementation over those channels.
//!
//! # Ring protocol and the single-writer-domain rule
//!
//! The [`layout`] module defines the ring protocol once and is consumed by
//! both guest-side code (hardware atomics via `PointerBackend`) and host-side
//! code (mutex-mediated atomics via `KernelBackend`). The layout is
//! backend-agnostic: all reservation, frame I/O, and slot operations go
//! through the [`MappingBackend`] trait.
//!
//! **Atomicity contract**: each ring is **single-writer-domain**. All writers
//! on a given ring MUST operate within the same atomicity domain — either
//! guest-side hardware atomics OR host-side mutex-mediated atomics, never
//! mixed. Mixing domains is out-of-contract and may corrupt data because the
//! guest's `compare_exchange` and the host's mutex serialisation do not
//! observe each other's ordering guarantees.
//!
//! Readers may cross domains safely (a guest writer + host reader, or vice
//! versa, is fine) because reads use acquire semantics that are satisfied by
//! any release fence in the same shared memory. The constraint applies only
//! to concurrent *writers* on the same ring.
//!
//! This rule is documented in `AGENTS.md` and asserted in debug builds via
//! `layout::reserve_tail` when a domain tag is available.

use selium_memory::Region;
use selium_wire::error::Error;

pub use channels::{Channel, ChannelBackpressure, WeakReader};
pub use layout::{RingReader, RingWriter, round_capacity as layout_round_capacity};
pub use region::{ChannelRegion, DATA_OFFSET, MIN_REGION_BYTES};
pub use ring_buf::{RingBuf, round_capacity};
pub use rpc::{
    OwnedRpcClient, OwnedServerStreamClient, RpcClient, RpcConnection, RpcError, RpcRequest,
    ServerStreamClient, ServerStreamConnection, ServerStreamRequest, accept, accept_server_stream,
    connect, connect_server_stream,
};
pub use transport::{ShmRendezvous, ShmTransport};

pub mod byte_channel;
pub mod channels;
pub mod layout;
pub mod region;
pub mod ring_buf;
pub mod rpc;
pub mod transport;

/// Frees a shared memory region via the global provider.
///
/// In guest mode this issues a `FreeRegion` hostcall, which is
/// ownership-checked by the runtime; in native test mode it removes the
/// region from the heap registry.
pub fn free_region(region_id: u64) -> Result<(), Error> {
    selium_memory::region_provider()?
        .free(region_id)
        .map_err(Error::from)
}

/// Allocates a shared memory region via the global provider.
pub(crate) fn allocate_region(
    pages: u32,
    prot: selium_abi::RegionProt,
    purpose: selium_abi::ResourceKind,
) -> Result<Region, Error> {
    selium_memory::region_provider()?
        .allocate(pages, prot, purpose)
        .map_err(Error::from)
}

/// Attaches to an existing shared memory region via the global provider.
pub(crate) fn attach_region(
    region_id: u64,
    reader_slot: Option<u32>,
    prot: selium_abi::RegionProt,
) -> Result<Region, Error> {
    selium_memory::region_provider()?
        .attach(region_id, reader_slot, prot)
        .map_err(Error::from)
}

/// Ensures a heap provider is installed when running under `cfg(test)`.
#[cfg(test)]
pub(crate) fn ensure_heap_provider() {
    if selium_memory::region_provider().is_err() {
        install_heap_provider();
    }
}

/// Convenience helper to install the heap provider for tests.
#[cfg(test)]
pub(crate) fn install_heap_provider() {
    drop(selium_memory::set_region_provider(Box::new(
        selium_memory::HeapRegionProvider::new(),
    )));
}
