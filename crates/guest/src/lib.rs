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
    context::{Context, DISCOVERY_ROUTE, Serve},
    control::{CONTROL_ROUTE, ControlClient},
    error::{GuestError, Result},
    hostcall::{
        authorise_attach, commit_tenant_ca, declare_attachable_by_resolution, expected_claims,
        namespace_from_tenant, process_capability, process_capability_for_uri, process_list,
        process_namespace, process_tenant, quota_clear, quota_set, random_bytes,
        record_resolved_resource_for, resource_authority, revoke_ca, rollback_tenant_ca, self_info,
        sign_tenant_ca, sign_user_cert,
    },
    net::{Datagram, TcpListener, TcpStream, UdpSocket},
    platform::process_id,
    process::{ActivityLog, Metering, Process},
    resource::{Accept, IncomingConnection, ResourceListener, ResourceSender},
    store::{BlobStore, DurableLog, STORE_ROUTE, Slice, StoreClient},
    time::{Instant, Timer, now},
    watch::{ROUTE_TABLE_ROUTE, RouteEvent, RouteWatch},
};
pub use selium_abi::{
    AbiErrorCode, AttachDeclarationScope, Capability, CapabilityGrant, EntrypointMetadata,
    LocalityScope, ModuleSource, Namespace, RegionProt, ResourceClass, ResourceIdentity,
    ResourceSelector, ScopeContext,
};
// Re-export service message types and encoding types.
pub use selium_guest_macros::{entrypoint, pattern_interface, schema};
// Principal channel and messaging-pattern surface: a first guest should not
// need to depend on the lower transport crates directly.
pub use selium_shm::{
    Channel, ChannelBackpressure,
    rpc::{self, OwnedRpcClient, RpcClient, RpcConnection, RpcError},
    transport::ShmTransport,
};
pub use selium_wire::{
    FrameCodec, FramedRead, FramedWrite, LiveTable, LiveTableMessage, LiveTableRecord,
    LiveTableView, Publisher, Subscriber, TableWriterAuthority,
};
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
mod client_session;
mod context;
mod control;
mod error;
mod hostcall;
mod hostcall_region_provider;
pub mod log;
pub mod net;
mod platform;
mod process;
mod resource;
mod store;
pub mod time;
pub mod volume;
mod watch;

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

/// Advertises the guest ABI in a passive custom section. The runtime reads
/// these bytes before instantiation, so negotiation cannot execute guest code
/// or reach a host import. Keep the section name in sync with
/// `selium_abi::SELIUM_ABI_VERSION_SECTION` (WASM attributes require a literal).
#[cfg(target_family = "wasm")]
#[used]
#[unsafe(link_section = "selium_abi_version")]
static SELIUM_ABI_VERSION_DECLARATION: [u8; 4] = selium_abi::SELIUM_ABI_VERSION.to_le_bytes();

/// Tracks whether this SDK installed the ambient provider. The provider itself
/// is a process-global trait object with no identity query, so this bit is what
/// distinguishes idempotent re-entry from a conflicting preinstalled context.
static GUEST_PROVIDER_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Installs the hostcall-backed region provider and registers the mailbox
/// reactor so the guest can allocate and share memory regions.
///
/// This should be called once per guest process, typically from an
/// entrypoint before any I/O patterns are used. It is safe to call multiple
/// times after this SDK's own successful installation. A provider installed by
/// anybody else is a conflicting guest context and is rejected loudly.
pub fn init() -> Result<()> {
    use std::sync::atomic::Ordering;

    if !GUEST_PROVIDER_INSTALLED.load(Ordering::Acquire) {
        if selium_memory::region_provider().is_ok() {
            return Err(GuestError::Host(
                "guest region provider conflicts with a preinstalled provider".to_string(),
            ));
        }
        selium_memory::set_region_provider(Box::new(HostcallRegionProvider::new()))
            .map_err(|error| GuestError::Host(error.to_string()))?;
        GUEST_PROVIDER_INSTALLED.store(true, Ordering::Release);
    }
    crate::platform::register_mailbox();
    // Install the generation-wait callbacks before any entrypoint work. A
    // channel, RPC, or route-table read that finds its ring empty parks on
    // `register_generation_wait`; with no callback installed that returns
    // false and every caller self-wakes, so the entrypoint's *first* park
    // spins the reactor instead of stalling. `poll_reactor` also installs
    // them (idempotently) for the host-driven polls.
    crate::async_runtime::install_generation_wait_callbacks();
    Ok(())
}

#[cfg(test)]
mod tests {
    // scenario: region-provider/global-regionprovider-installation/conflicting-guest-provider-installation
    #[test]
    fn init_rejects_a_conflicting_preinstalled_provider() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
        let error = crate::init().expect_err("a foreign provider must not be retained silently");
        assert!(
            error.to_string().contains("conflicts"),
            "unexpected error: {error}"
        );
    }

    /// The first-guest surface: the spine demo imports its channel, transport,
    /// framing, and pub/sub types from `selium-guest` alone — no direct
    /// dependency on the lower transport crates.
    // scenario: selium-guest/primary-channel-patterns-use-the-unified-sdk-surface/first-guest-uses-only-the-sdk
    // scenario: selium-guest/livetable-in-selium-wire/livetable-is-importable-from-selium-wire
    // scenario: selium-guest/livetable-in-selium-wire/livetable-is-re-exported-from-selium-guest
    // scenario: selium-guest/rpc-types-in-selium-wire/rpcclient-is-importable-from-selium-wire
    // scenario: selium-guest/rpc-types-in-selium-wire/rpcclient-is-re-exported-from-selium-guest
    #[test]
    fn the_spine_demo_consumes_the_unified_sdk_surface() {
        let manifest = include_str!("../../../integration/spine-demo/Cargo.toml");
        for forbidden in ["selium-shm", "selium-wire"] {
            assert!(
                !manifest.contains(forbidden),
                "the spine demo must not depend on {forbidden} directly:\n{manifest}"
            );
        }
        let source = include_str!("../../../integration/spine-demo/src/lib.rs");
        assert!(
            source.contains("use selium_guest::{"),
            "the spine demo imports its pattern types from the SDK"
        );
        // The wire crate is the definition site; the SDK re-exports both at
        // its root. Naming the paths proves resolution without instantiating
        // them (the table and client carry bounds only on their impls).
        let wire_table =
            std::any::type_name::<selium_wire::LiveTable<u32, u32, crate::ShmTransport>>();
        let wire_client =
            std::any::type_name::<selium_wire::rpc::RpcClient<u32, u32, crate::ShmTransport>>();
        let sdk_table = std::any::type_name::<crate::LiveTable<u32, u32, crate::ShmTransport>>();
        let sdk_client = std::any::type_name::<crate::RpcClient<u32, u32>>();
        for path in [wire_table, wire_client, sdk_table, sdk_client] {
            assert!(
                path.contains("LiveTable") || path.contains("RpcClient"),
                "{path}"
            );
        }

        // The names the demo uses are reachable from this crate's root.
        let _sdk_surface = std::any::type_name::<(
            crate::Channel,
            crate::ChannelBackpressure,
            crate::ShmTransport,
            crate::Publisher<crate::LiveTableMessage<u32, u32>, crate::ShmTransport>,
            crate::TableWriterAuthority,
        )>();
    }
}
