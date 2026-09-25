//! Selium guest SDK.

// The `nightly-wasm-atomics` feature enables the genuine
// `memory.atomic.wait32` / `memory.atomic.notify` WASM intrinsics the
// multithreaded executor uses to park workers and deliver wake-by-notify.
// Those intrinsics live behind the `stdarch_wasm_atomic_wait` feature gate on
// nightly; gate the attribute so stable builds (without the cargo feature)
// stay on the stable compiler.
#![cfg_attr(
    all(target_arch = "wasm32", feature = "nightly-wasm-atomics"),
    feature(stdarch_wasm_atomic_wait)
)]

use crate::hostcall_region_provider::HostcallRegionProvider;

pub use crate::{
    async_runtime::{
        JoinHandle, poll_reactor, poll_safely, run_entrypoint_safely, run_entrypoint_with_result,
        spawn, yield_now,
    },
    context::{Context, Serve},
    error::{GuestError, Result},
    hostcall::{
        namespace_from_tenant, process_capability, process_namespace, process_tenant, quota_clear,
        quota_set, random_bytes, record_registration, record_resolved_queue_for,
        record_resolved_region_for, resolve_protocol_handler, revoke_ca, self_info, sign_tenant_ca,
        sign_user_cert,
    },
    net::{Datagram, TcpListener, TcpStream, UdpSocket},
    platform::{mark_ready, process_id},
    process::{ActivityLog, Metering, Process},
    resource::{Accept, IncomingConnection, ResourceListener, ResourceSender},
    storage::{BlobStore, DurableLog},
    time::{Instant, Timer, now},
};
pub use selium_abi::{
    Capability, CapabilityGrant, EntrypointMetadata, LocalityScope, Namespace, RegionProt,
    ResourceClass, ResourceIdentity, ResourceSelector, ScopeContext,
};
// Re-export service message types and encoding types.
pub use selium_guest_macros::{entrypoint, pattern_interface, schema};
// Re-export transport-agnostic memory primitives.
pub use selium_memory::{RING_HEADER_SIZE, RegionMapping, SHARED_REGION_MAGIC, WASM_PAGE_SIZE};
pub use selium_service::{
    DiscoveryRequest, DiscoveryResponse, FieldEncoder, FlatMsg, HasSchema, InterfaceMetadata,
    ResourceTarget, SchemaDescriptor,
    codec::{decode_typed, encode_typed},
    log::{LogField, LogLevel, LogRecord, LogSpan},
};
pub use tracing::{debug, error, info, trace, warn};

pub mod args;
mod async_runtime;
mod context;
mod error;
mod hostcall;
mod hostcall_region_provider;
pub mod log;
pub mod net;
mod platform;
mod process;
mod resource;
mod storage;
pub mod time;

/// The multithreaded worker entry export: the runtime calls this on each
/// dedicated OS worker thread of a multithreaded guest, passing the worker's
/// id, and the worker runs the shared executor over the guest's linear memory
/// until the process exits. Emitted alongside the `__selium_guest_poll`
/// entrypoint-exit-code export; `module_probe` uses its presence to select
/// multithreaded execution.
///
/// The export is emitted **only for guests opting into multithreaded
/// execution** (`multithreaded` feature) **and built with the atomics target**
/// (`+atomics` + `--shared-memory`, the `nightly-wasm-atomics` feature): the
/// executor's parking words and worker handoff require real atomics over
/// shared linear memory, so a guest without both never carries the worker
/// entry and the runtime falls back to the cooperative single-worker reactor.
///
/// Returns the poll-owner exit code (0 running / 1 completed) so a worker
/// observing process completion reports it the same way the poll export does.
#[cfg(all(
    target_family = "wasm",
    feature = "multithreaded",
    target_feature = "atomics"
))]
#[unsafe(export_name = "__selium_guest_worker")]
pub extern "C" fn __selium_guest_worker(worker_id: i32) -> i32 {
    let worker_id = u32::try_from(worker_id).unwrap_or(0);
    crate::async_runtime::worker_enter(worker_id, true)
}

/// Installs the hostcall-backed region provider and registers the mailbox
/// reactor so the guest can allocate and share memory regions.
///
/// This should be called once per guest process, typically from an
/// entrypoint before any I/O patterns are used. It is safe to call multiple
/// times; subsequent calls are no-ops.
pub fn init() -> Result<()> {
    if selium_memory::region_provider().is_err() {
        selium_memory::set_region_provider(Box::new(HostcallRegionProvider::new()))
            .map_err(|error| GuestError::Host(error.to_string()))?;
    }
    crate::platform::register_mailbox();
    Ok(())
}
