use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use parking_lot::Mutex;
use selium_abi::{OperationId, ProcessId, ResourceClass, TaskId};
use selium_kernel::Kernel;
use selium_service::DiscoveryRequest;
use selium_shm::transport::ShmTransport;
use selium_wire::pubsub::Publisher;

use crate::{
    bootstrap::LoadedGuest,
    config::ProcessAuthority,
    error::{Error, Result},
    hostcall::HostOperation,
    mailbox::GuestMailbox,
    region_provider::RuntimeRegionProvider,
};

/// Publisher for the runtime→discovery pub/sub feed.
pub(crate) type DiscoveryPublisher = Publisher<DiscoveryRequest, ShmTransport>;
pub(crate) type LocalHandleOwners = HashMap<(ResourceClass, u64), BTreeSet<ProcessId>>;
/// Principal tenant per `(process_id, queue shared id)` for host queues.
pub(crate) type QueueTenants = HashMap<(ProcessId, u64), String>;
/// Serving tenant tracked per (process_id, region_id) so FreeRegion can revoke
/// the region's URI (minted under principal provenance).
pub(crate) type RegionTenants = HashMap<(ProcessId, u64), String>;
pub(crate) type SharedResourceOwners = HashMap<(ResourceClass, u64), BTreeSet<ProcessId>>;
/// Wait registry keyed by (process_id, region_id).
pub(crate) type WaitRegistry = HashMap<(ProcessId, u64), Vec<WaitEntry>>;

/// Wait registry entry: (process_id, task_id, generation) for a region.
/// When the host advances the generation on a region past a registered
/// generation, the registered task is woken via the mailbox.
#[derive(Debug, Clone)]
pub(crate) struct WaitEntry {
    pub(crate) process_id: ProcessId,
    pub(crate) task_id: TaskId,
    pub(crate) region_id: u64,
    pub(crate) generation: u64,
}

/// Runtime coordinating guest execution, hostcalls, and kernel resources.
#[derive(Clone)]
pub struct Runtime {
    pub(crate) kernel: Kernel,
    pub(crate) process_authorities: Arc<Mutex<HashMap<ProcessId, ProcessAuthority>>>,
    pub(crate) loaded_guests: Arc<Mutex<HashMap<ProcessId, LoadedGuest>>>,
    pub(crate) local_handle_owners: Arc<Mutex<LocalHandleOwners>>,
    pub(crate) shared_resource_owners: Arc<Mutex<SharedResourceOwners>>,
    pub(crate) module_registry: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub(crate) next_operation_id: Arc<Mutex<OperationId>>,
    pub(crate) operations: Arc<Mutex<HashMap<OperationId, HostOperation>>>,
    pub(crate) mailboxes: Arc<Mutex<HashMap<ProcessId, Arc<GuestMailbox>>>>,
    /// Publisher for the runtime→discovery pub/sub feed, when discovery is enabled.
    pub(crate) discovery_publisher: Arc<Mutex<Option<DiscoveryPublisher>>>,
    /// Shared id of the discovery RPC listener, when discovery is enabled.
    pub(crate) discovery_listener_shared_id: Arc<Mutex<Option<u64>>>,
    /// Process id of the booted discovery system guest, if any. Only this
    /// process may call `RecordResolvedQueueFor` on behalf of resolvers.
    pub(crate) discovery_process: Arc<Mutex<Option<ProcessId>>>,
    /// Serving tenant tracked per (process_id, region_id) so FreeRegion can
    /// revoke the region's URI.
    pub(crate) region_tenants: Arc<Mutex<RegionTenants>>,
    /// Principal tenant tracked per (process_id, queue_id) so process
    /// teardown can revoke the queue's URI under the tenant it was minted
    /// for (which may differ from the creating process's own tenant).
    pub(crate) queue_tenants: Arc<Mutex<QueueTenants>>,
    /// Tenant whose `ResourceClass::Process` quota a spawned child consumed,
    /// keyed by the child's process id. Recorded by `ProcessStart`; teardown
    /// releases the slot exactly once by removing (and reading) the entry.
    pub(crate) process_quota_tenants: Arc<Mutex<HashMap<ProcessId, String>>>,
    /// Wait registry: guest tasks parked on host-writable rings.
    pub(crate) wait_registry: Arc<Mutex<WaitRegistry>>,
    /// Region attachments: every process that mapped a shared region,
    /// recorded at `AttachRegion` and removed at process cleanup. Wake
    /// registration (`WaitRegister`/`GenerationAdvance`) authorises both
    /// owners and attachers: an attacher holds a live mapping of the
    /// region, so its parked readers must be woken by a writer's
    /// generation bump — notably a guest that attaches a region a peer
    /// allocated and handed off through a queue it does not own (the
    /// bridge channel attaching the connector's relayed stream region).
    pub(crate) region_attachments: Arc<Mutex<HashMap<u64, HashSet<ProcessId>>>>,
    /// Wait targets for active network outbound proxy threads.
    /// Each entry is `(shared_id, generation_offset)` — the absolute byte
    /// offset of the ring's generation word within the shared region — used
    /// to kick the proxy on guest→host transitions for regions without the
    /// fast path.
    pub(crate) network_wait_keys: Arc<Mutex<Vec<(u64, u64)>>>,
    /// Per-attachment shared-page fast-path eligibility: region id →
    /// attaching process id → whether that process's guest module is
    /// fast-path capable (engine registry support + module declares shared
    /// memory and contains atomic notify opcodes; see `module_probe`).
    ///
    /// A region's fast path is active only when **every** attacher is
    /// capable, so a stable-built guest sharing a region with an atomics
    /// guest keeps its transition kicks. Entries are recorded at attach and
    /// removed when the attacher releases the region or the region is
    /// destroyed.
    pub(crate) fast_path_attachments: Arc<Mutex<HashMap<u64, HashMap<ProcessId, bool>>>>,
    /// Per-process fast-path capability, probed from the guest's module
    /// bytes at spawn (see `module_probe`). Removed at process cleanup.
    pub(crate) process_fastpath: Arc<Mutex<HashMap<ProcessId, bool>>>,
    /// Guest→host transition kicks **delivered** per network region id.
    /// Suppressed regions (fast path active) are not counted. Observability
    /// for embedders and the fast-path end-to-end test, which asserts a
    /// fast-path region's count stays at zero while the guest's atomic
    /// notify carries its wakes.
    pub(crate) kick_counts: Arc<Mutex<HashMap<u64, u64>>>,
    /// Maps a host queue's local id to the process that owns its receiver,
    /// so kernel-side sends (e.g. an accepted connection enqueued by the
    /// network poller) can wake the parked receiving guest.
    pub(crate) queue_waiters: Arc<Mutex<HashMap<u64, u64>>>,
    /// Internal route URIs a process has registered with discovery, recorded
    /// by the discovery system guest via `RecordRegistration`. Used to gate
    /// the readiness of a role-declared system guest on its registration
    /// being observable in discovery. Cleared when the process terminates.
    pub(crate) process_registrations: Arc<Mutex<HashMap<ProcessId, HashSet<String>>>>,
    /// Protocol schemes a booted system guest handles (e.g. `sel-http`),
    /// keyed by process id. Revoked when the process terminates.
    pub(crate) handler_schemes: Arc<Mutex<HashMap<ProcessId, Vec<String>>>>,
    /// Process ids whose guest reactor is currently being executed by a
    /// host thread. Guarantees at most one thread enters a guest's WASM
    /// store; losers of the race return and rely on the winner's
    /// pending-wake re-check (see `poll_guest_until_stalled`).
    pub(crate) executing_guests: Arc<Mutex<HashSet<ProcessId>>>,
    /// Captured ambient Tokio runtime handle, used to spawn `Sleep`
    /// hostcall timer wakes. Guests can be executed inline on non-Tokio
    /// threads (e.g. the kernel poller's datagram-wake path), where
    /// `tokio::spawn` would panic for lack of a thread-local reactor; the
    /// first handle observed on a Tokio thread is reused for those.
    pub(crate) timer_handle: Arc<std::sync::OnceLock<tokio::runtime::Handle>>,
    /// Host-held PKI keyring backing the certificate-signing hostcalls.
    /// Initialised at startup (see [`Runtime::initialize_keyring`]); the
    /// signing hostcalls fail loudly while it is absent.
    pub(crate) keyring: Arc<Mutex<Option<crate::keyring::Keyring>>>,
    /// Host-side metering projector: accumulates per-process cumulative
    /// cpu/bandwidth counters and storage gauges, projected into the kernel on
    /// each metering tick (see [`Runtime::metering_tick`]).
    pub(crate) metering: Arc<Mutex<crate::metering::MeteringProjector>>,
    /// Guards the one-shot metering ticker started at bootstrap: the host
    /// projects fresh per-process observations on the sampling cadence so
    /// the bookkeeper's `MeteringRead` polls observe live consumption.
    pub(crate) metering_ticker_started: Arc<std::sync::atomic::AtomicBool>,
    /// Per-guest worker-count overrides for multithreaded execution, keyed by
    /// system guest name. Absent entries default to the number of available
    /// CPU cores (the pool never exceeds it unless explicitly configured).
    pub(crate) worker_counts: Arc<Mutex<HashMap<String, usize>>>,
    /// Per-process CPU budget windows (see [`CpuBudgetWindow`]): the wall-clock
    /// minute a process's engine execution budget is anchored for. Written by
    /// the spawn-time anchor and the per-second `refresh_cpu_budgets`, removed
    /// at process teardown.
    pub(crate) cpu_budget_windows: Arc<Mutex<HashMap<ProcessId, CpuBudgetWindow>>>,
}

/// Per-process CPU budget window: the wall-clock minute the process's engine
/// execution budget is currently anchored for, and its cumulative
/// instruction count at that window's start.
///
/// The runtime recomputes a process's budget as
/// `window_start_instructions + ceiling` on every refresh, so consumption in
/// one window never reduces the next window's ceiling while a ceiling authored
/// mid-window still takes effect against the current window's remaining
/// allowance. A change of `window_index` (a new wall-clock minute) re-anchors
/// `window_start_instructions` from the live count.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CpuBudgetWindow {
    /// Wall-clock minute index (`unix_seconds / 60`).
    pub(crate) window_index: u64,
    /// The process's cumulative executed-instruction count at this window's
    /// start.
    pub(crate) window_start_instructions: u64,
}

impl Runtime {
    /// Creates a runtime backed by the supplied kernel.
    pub fn new(kernel: Kernel) -> Self {
        // Install the runtime's kernel-backed region provider so that the
        // runtime can use selium-shm directly (e.g. for discovery pub/sub).
        if selium_memory::region_provider().is_err() {
            drop(selium_memory::set_region_provider(Box::new(
                RuntimeRegionProvider::new(kernel.clone()),
            )));
        }

        let operations = Arc::new(Mutex::new(HashMap::<OperationId, HostOperation>::new()));
        let mailboxes = Arc::new(Mutex::new(HashMap::<ProcessId, Arc<GuestMailbox>>::new()));

        let runtime = Self {
            kernel,
            process_authorities: Arc::new(Mutex::new(HashMap::new())),
            loaded_guests: Arc::new(Mutex::new(HashMap::new())),
            local_handle_owners: Arc::new(Mutex::new(HashMap::new())),
            shared_resource_owners: Arc::new(Mutex::new(HashMap::new())),
            module_registry: Arc::new(Mutex::new(HashMap::new())),
            next_operation_id: Arc::new(Mutex::new(1)),
            operations,
            mailboxes,
            discovery_publisher: Arc::new(Mutex::new(None)),
            discovery_listener_shared_id: Arc::new(Mutex::new(None)),
            discovery_process: Arc::new(Mutex::new(None)),
            region_tenants: Arc::new(Mutex::new(HashMap::new())),
            queue_tenants: Arc::new(Mutex::new(HashMap::new())),
            process_quota_tenants: Arc::new(Mutex::new(HashMap::new())),
            wait_registry: Arc::new(Mutex::new(HashMap::new())),
            region_attachments: Arc::new(Mutex::new(HashMap::new())),
            network_wait_keys: Arc::new(Mutex::new(Vec::new())),
            fast_path_attachments: Arc::new(Mutex::new(HashMap::new())),
            process_fastpath: Arc::new(Mutex::new(HashMap::new())),
            kick_counts: Arc::new(Mutex::new(HashMap::new())),
            queue_waiters: Arc::new(Mutex::new(HashMap::new())),
            process_registrations: Arc::new(Mutex::new(HashMap::new())),
            handler_schemes: Arc::new(Mutex::new(HashMap::new())),
            executing_guests: Arc::new(Mutex::new(HashSet::new())),
            timer_handle: Arc::new(std::sync::OnceLock::new()),
            keyring: Arc::new(Mutex::new(None)),
            metering: Arc::new(Mutex::new(crate::metering::MeteringProjector::default())),
            metering_ticker_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            worker_counts: Arc::new(Mutex::new(HashMap::new())),
            cpu_budget_windows: Arc::new(Mutex::new(HashMap::new())),
        };

        // Initialise the mio network poller if possible (best-effort).
        // Tests that don't need networking can use Runtime::default()
        // without a poller; it simply won't drive any sockets.
        if let Ok(poller) = runtime.kernel.init_poller() {
            let rt = runtime.clone();
            poller.set_generation_advance(move |region_id, new_gen| {
                rt.note_generation_advance(region_id, new_gen);
            });
            poller.start_background();
        }

        runtime
    }

    /// Returns a clone of the runtime kernel handle.
    pub fn kernel(&self) -> Kernel {
        self.kernel.clone()
    }

    /// Returns the shared region id of the discovery pub/sub feed ring, if discovery was started.
    pub fn discovery_feed_region_id(&self) -> Option<u64> {
        self.discovery_publisher
            .lock()
            .as_ref()
            .map(|publisher| publisher.writer().inner().write_region_id())
    }

    /// Returns the shared id of the discovery RPC listener, if discovery was started.
    pub fn discovery_listener_shared_id(&self) -> Option<u64> {
        *self.discovery_listener_shared_id.lock()
    }

    /// Initialises the host PKI keyring from bootstrap material, failing
    /// loudly when the intermediate is missing or invalid. Identity depends
    /// on this at startup: a host that cannot sign tenant CAs must not
    /// bootstrap the mint authority.
    pub fn initialize_keyring(&self, bootstrap: crate::keyring::KeyringBootstrap) -> Result<()> {
        let keyring = crate::keyring::Keyring::initialize(
            Box::new(crate::keyring::InMemoryCaStore::default()),
            bootstrap,
        )
        .map_err(|error| Error::Host(error.to_string()))?;
        *self.keyring.lock() = Some(keyring);
        Ok(())
    }

    /// Generates a fresh root + intermediate hierarchy and installs it as the
    /// host keyring. The offline root is retained for operator bootstrap and
    /// never referenced by a hostcall.
    pub fn generate_keyring(&self) -> Result<()> {
        let keyring =
            crate::keyring::Keyring::generate().map_err(|error| Error::Host(error.to_string()))?;
        *self.keyring.lock() = Some(keyring);
        Ok(())
    }

    /// Records a route registration for `process_id`, as reported by the
    /// discovery system guest. Used to gate role-declared readiness on
    /// discoverable self-registration.
    pub(crate) fn record_registration(&self, process_id: ProcessId, uri: String) {
        self.process_registrations
            .lock()
            .entry(process_id)
            .or_default()
            .insert(uri);
    }

    /// Returns whether `process_id` has a recorded registration for `uri`.
    pub fn has_registration(&self, process_id: ProcessId, uri: &str) -> bool {
        self.process_registrations
            .lock()
            .get(&process_id)
            .is_some_and(|uris| uris.contains(uri))
    }

    /// Publishes a typed discovery operation to the discovery feed.
    ///
    /// Returns `Ok(())` if discovery is enabled and the publish succeeds. If
    /// discovery is not enabled, this is a no-op.
    pub(crate) fn publish_discovery_event(&self, request: DiscoveryRequest) -> Result<()> {
        let mut publisher = self.discovery_publisher.lock();
        if let Some(ref mut publisher) = *publisher {
            publisher
                .publish(&request)
                .map_err(|error| crate::Error::Host(format!("discovery publish failed: {error}")))
        } else {
            Ok(())
        }
    }

    /// Returns the registered mailbox of `process_id`, if the guest has
    /// registered one (the guest calls `mailbox_register` during init).
    pub(crate) fn mailbox(&self, process_id: ProcessId) -> Option<Arc<GuestMailbox>> {
        self.mailboxes.lock().get(&process_id).cloned()
    }

    /// Configures the worker-pool size for a multithreaded system guest by
    /// name. Unconfigured guests default to the number of available CPU
    /// cores (never exceeding it unless this override is set).
    ///
    /// The count is clamped to at least one worker: a zero-worker pool would
    /// have no thread to run the guest, and the monitor would reap the
    /// process the instant it spawned.
    pub fn set_worker_count(&self, guest_name: &str, count: usize) {
        self.worker_counts
            .lock()
            .insert(guest_name.to_string(), count.max(1));
    }

    /// Resolves the worker-pool size for a multithreaded guest: the explicit
    /// per-guest override, or the available-core default.
    pub fn worker_count_for(&self, guest_name: &str) -> usize {
        self.worker_counts
            .lock()
            .get(guest_name)
            .copied()
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get)
                    .unwrap_or(1)
            })
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(Kernel::default())
    }
}
