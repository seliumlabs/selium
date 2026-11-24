use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use selium_abi::{ActivityEvent, Capability, CapabilityGrant, ResourceIdentity, ResourceSelector};
use selium_shm::{Channel, ChannelBackpressure, transport::ShmTransport};
use selium_wire::{framed::FramedWrite, pubsub::Publisher};
use tracing::info;
use wasmtiny::{WasmApplication, WasmValue};

use crate::{
    Error, Result,
    config::{
        BootstrapReport, BootstrappedGuest, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor,
    },
    error::map_wasm_error,
    runtime::{DiscoveryPublisher, Runtime},
};

pub(crate) enum GuestExecution {
    /// The cooperative single-worker reactor (interpreter path): the host
    /// drives `__selium_guest_poll` to stall on each wake.
    Cooperative {
        app: WasmApplication,
        module_index: u32,
    },
    /// The multithreaded worker pool (AOT path): dedicated OS worker threads
    /// enter `__selium_guest_worker` concurrently over the shared instance.
    Multithreaded(crate::multithreaded::MultithreadedGuest),
}

pub(crate) struct LoadedGuest {
    pub(crate) execution: GuestExecution,
    pub(crate) entrypoint_results: Vec<WasmValue>,
}

/// Validates the passive module declaration before instantiation, start
/// functions, entrypoints, or hostcall request decoding can run.
pub(crate) fn negotiate_abi_version(probe: &crate::module_probe::ModuleProbe) -> Result<()> {
    use crate::module_probe::AbiVersionDeclaration;

    if !probe.uses_hostcalls {
        return Ok(());
    }
    match probe.abi_version {
        AbiVersionDeclaration::Version(selium_abi::SELIUM_ABI_VERSION) => Ok(()),
        AbiVersionDeclaration::Version(guest) => Err(Error::AbiVersionMismatch {
            host: selium_abi::SELIUM_ABI_VERSION,
            guest,
        }),
        AbiVersionDeclaration::Missing | AbiVersionDeclaration::Invalid => {
            Err(Error::MissingAbiNegotiation {
                host: selium_abi::SELIUM_ABI_VERSION,
            })
        }
    }
}

impl Runtime {
    /// Boots all configured system guests in declaration order, in a single
    /// pass.
    ///
    /// There is no readiness gate and no dependency sequencer: boot order is
    /// the configuration's declaration order, and convergence on peers is the
    /// guests' concern (they watch or retry routes rather than rely on the
    /// host to withhold a start). An entrypoint failure still rolls back the
    /// guests started so far.
    pub fn bootstrap_system_guests(&self, mut config: RuntimeConfig) -> Result<BootstrapReport> {
        // Boot is state-rooted: root the volume layer at the configured state
        // directory, load or generate the host PKI hierarchy, and reconstruct
        // per-tenant volume usage — all before any guest runs.
        self.prepare_state_root(config.state_dir.as_deref())?;

        // The metering producer: a one-second host ticker projecting fresh
        // per-process observations into the kernel, so the bookkeeper's
        // `MeteringRead` polls observe live consumption (the accountant's
        // "Metering Projection by the Host" requirement). Started at the
        // first bootstrap of the runtime; unit tests that drive
        // `metering_tick` manually are unaffected (the projection is
        // idempotent — it re-derives observations from the accumulators).
        if !self
            .metering_ticker_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let runtime = self.background_handle();
            let stop = Arc::clone(&self.metering_ticker_stop);
            let handle = std::thread::Builder::new()
                .name("selium-metering-ticker".to_string())
                .spawn(move || {
                    // The CPU budget refresh runs on the same ticker: each live
                    // process's engine execution budget is re-applied every
                    // second, so a ceiling authored or withdrawn between
                    // windows lands within one sampling interval. The budget
                    // *window* itself re-anchors only when the wall-clock
                    // minute rolls (see `refresh_cpu_budgets`).
                    loop {
                        let mut stopped = stop.0.lock();
                        if *stopped {
                            break;
                        }
                        stop.1
                            .wait_for(&mut stopped, std::time::Duration::from_secs(1));
                        if *stopped {
                            break;
                        }
                        drop(stopped);
                        let Some(runtime) = runtime.upgrade() else {
                            break;
                        };
                        runtime.metering_tick();
                        runtime.refresh_cpu_budgets();
                    }
                })
                .map_err(|error| Error::Host(format!("metering ticker spawn failed: {error}")))?;
            *self.metering_ticker_handle.lock() = Some(handle);
        }
        let (discovery_feed_region_id, discovery_listener_shared_id) = if config.start_discovery {
            let (feed_region_id, listener_shared_id) = self.setup_discovery()?;
            (Some(feed_region_id), Some(listener_shared_id))
        } else {
            // A bootstrap call against a runtime whose discovery service is
            // already running (e.g. a follow-up spawn of additional system
            // guests) still wires the discovery handle into the new guests.
            (
                self.discovery_feed_region_id(),
                self.discovery_listener_shared_id(),
            )
        };

        if let Some(listener_shared_id) = discovery_listener_shared_id {
            let feed_region_id =
                discovery_feed_region_id.expect("discovery feed region id must be present");
            for descriptor in &mut config.system_guests {
                if descriptor.discovery_service {
                    descriptor.set_discovery_feed_and_handle(feed_region_id, listener_shared_id);
                    // Add explicit resource grants so the discovery guest can attach
                    // to the feed region and listener queue created by setup_discovery.
                    descriptor.grants.push(CapabilityGrant::new(
                        Capability::SharedMemory,
                        vec![ResourceSelector::ExplicitResource(
                            ResourceIdentity::Shared(feed_region_id),
                        )],
                    ));
                    descriptor.grants.push(CapabilityGrant::new(
                        Capability::HostQueue,
                        vec![ResourceSelector::ExplicitResource(
                            ResourceIdentity::Shared(listener_shared_id),
                        )],
                    ));
                } else if matches!(
                    descriptor.arguments.first(),
                    None | Some(SystemGuestArg::Pointer(_))
                ) {
                    // Every non-discovery guest whose leading parameter is the
                    // discovery Context (empty args) or a Context followed by a
                    // pointer argument gets the discovery handle prepended.
                    descriptor.set_discovery_handle(listener_shared_id);
                    // Other guests also need explicit grant for the discovery listener.
                    descriptor.grants.push(CapabilityGrant::new(
                        Capability::HostQueue,
                        vec![ResourceSelector::ExplicitResource(
                            ResourceIdentity::Shared(listener_shared_id),
                        )],
                    ));
                }
            }
        }

        // Guests are spawned in declaration order in a single pass. There is
        // no readiness gate and no dependency sequencer: ordering and
        // convergence are the guests' concern, expressed through route watches
        // and retry rather than host policy.
        let mut names = BTreeSet::new();
        let mut claims: BTreeMap<String, String> = BTreeMap::new();
        let mut report = BootstrapReport::default();
        for descriptor in config.system_guests {
            if !names.insert(descriptor.name.clone()) {
                // Reject the configuration without leaking the guests already
                // started before the duplicate was noticed.
                self.rollback_bootstrapped(&report);
                return Err(Error::DuplicateDescriptor(descriptor.name));
            }
            for uri in descriptor_claim_uris(&descriptor) {
                if let Some(existing) = claims.insert(uri.clone(), descriptor.name.clone()) {
                    self.rollback_bootstrapped(&report);
                    return Err(Error::Host(format!(
                        "duplicate route claim {uri} declared by {existing} and {}",
                        descriptor.name
                    )));
                }
            }
            let bootstrapped = match self.spawn_system_guest(descriptor.clone()) {
                Ok(bootstrapped) => bootstrapped,
                Err(error) => {
                    self.rollback_bootstrapped(&report);
                    return Err(error);
                }
            };
            if descriptor.discovery_service {
                *self.discovery_process_id.lock() = Some(bootstrapped.process_id);
            }
            report.guests.push(bootstrapped);
        }

        Ok(report)
    }

    /// Creates the discovery pub/sub feed ring and RPC listener.
    ///
    /// Stores the publisher and listener shared id in runtime state, and returns
    /// the feed region id and listener shared id so bootstrap can wire them into
    /// the discovery guest descriptor.
    fn setup_discovery(&self) -> Result<(u64, u64)> {
        let feed_channel = Channel::create_with_provider(
            self.region_provider.as_ref(),
            64 * 1024,
            ChannelBackpressure::Drop,
            selium_abi::ResourceKind::PubSubTopic,
        )
        .map_err(|error| {
            Error::Host(format!("failed to create discovery feed channel: {error}"))
        })?;
        let feed_region_id = feed_channel.region_id();

        let transport = ShmTransport::new(&feed_channel, &feed_channel).map_err(|error| {
            Error::Host(format!(
                "failed to create discovery feed transport: {error}"
            ))
        })?;
        let publisher: DiscoveryPublisher = Publisher::new(FramedWrite::new(transport));
        *self.discovery_publisher.lock() = Some(publisher);

        let queues = self.kernel.queues();
        let memory = self.kernel.memory();
        let listener = queues.create_host_queue(&memory);
        *self.discovery_listener_shared_id.lock() = Some(listener.shared_id);

        self.kernel.processes().record_activity(ActivityEvent {
            kind: selium_abi::ActivityKind::GuestBootstrapped,
            process_id: None,
            message: format!(
                "discovery feed region={feed_region_id} listener={}",
                listener.shared_id
            ),
        });

        Ok((feed_region_id, listener.shared_id))
    }

    /// Starts and records a single system guest.
    pub fn spawn_system_guest(
        &self,
        descriptor: SystemGuestDescriptor,
    ) -> Result<BootstrappedGuest> {
        // Handler schemes are host-provisioned route claims: the guest that
        // declares a protocol handler is the only process allowed to publish
        // `sel:///handler/<scheme>`. Platform routes are declared explicitly as
        // `RouteClaim` grants on the descriptor.
        let grants = grant_uris_with_handler_claims(&descriptor);
        self.validate_grants(&grants)?;

        let process = self.kernel.processes().start_process(
            descriptor.module_id.clone(),
            descriptor.entrypoint.clone(),
            grants.clone(),
        );
        self.persist_process_authority(process.local_id, grants, descriptor.tenant.clone(), None);
        // The process owns itself as a resource from the moment it exists: the
        // entrypoint publishes its own node before the host would otherwise get
        // another chance to claim the handle.
        self.claim_local_handle(
            process.local_id,
            selium_abi::ResourceClass::Process,
            process.local_id,
        );
        if descriptor.discovery_service
            && let Some(listener_shared_id) = self.discovery_listener_shared_id()
        {
            // The host minted the bootstrap listener before the guest existed.
            // Assign its shared ownership before the entrypoint publishes it,
            // so discovery's normal host-authority and route-claim checks have
            // a real owner to admit rather than a guest-side bypass.
            self.claim_shared_resource(
                process.local_id,
                selium_abi::ResourceClass::HostQueue,
                listener_shared_id,
            );
            self.resource_tenants.lock().insert(
                (selium_abi::ResourceClass::HostQueue, listener_shared_id),
                String::new(),
            );
        }

        // Shared-page fast-path detection: probe the guest's module bytes
        // once at spawn (shared memory declaration + atomic notify
        // opcodes). Region attach consumes the result; no user-facing
        // configuration exists (see the shared-page-fastpath spec).
        let fastpath = crate::module_probe::probe(&descriptor.module_bytes);
        if let Err(error) = negotiate_abi_version(&fastpath) {
            self.cleanup_failed_process(process.local_id)?;
            return Err(error);
        }
        self.record_process_fastpath(process.local_id, fastpath.fast_path_capable());
        // Multithreaded execution is opt-in per guest via the module: a guest
        // exporting the worker entry runs on a dedicated worker pool (AOT);
        // every other guest keeps the cooperative single-worker reactor.
        let loaded_guest = if fastpath.multithreaded() {
            match self.load_multithreaded_guest(&descriptor.module_bytes, process.local_id) {
                Ok(loaded_guest) => loaded_guest,
                Err(error) => {
                    self.cleanup_failed_process(process.local_id)?;
                    return Err(error);
                }
            }
        } else {
            match self.load_guest_module(&descriptor.module_bytes, process.local_id) {
                Ok(loaded_guest) => loaded_guest,
                Err(error) => {
                    self.cleanup_failed_process(process.local_id)?;
                    return Err(error);
                }
            }
        };
        // Bound the fresh process from its first instructions: anchor its
        // engine execution budget to the tenant's authored CPU ceiling now,
        // before the entrypoint runs, so a process spawned mid-window is not
        // unbounded until the next wall-clock minute.
        self.anchor_spawn_cpu_budget(
            process.local_id,
            descriptor.tenant.as_deref(),
            &loaded_guest.execution,
        );
        let loaded_guest = match self.execute_entrypoint(loaded_guest, &descriptor) {
            Ok(loaded_guest) => {
                if loaded_guest.entrypoint_results == [WasmValue::I32(1)] {
                    // The guest's own error reporting is its log channel
                    // (`run_entrypoint_with_result` logs the failing future's
                    // error there); surface it in the activity log so a
                    // bootstrap failure is diagnosable without a debugger.
                    let guest_logs = self.drain_guest_log_messages(process.local_id);
                    self.kernel.processes().record_activity(ActivityEvent {
                        kind: selium_abi::ActivityKind::ProcessExited,
                        process_id: Some(process.local_id),
                        message: format!(
                            "guest {} entrypoint returned error; guest logs: {guest_logs:?}",
                            descriptor.name
                        ),
                    });
                    self.cleanup_failed_process(process.local_id)?;
                    return Err(Error::EntrypointFailed(descriptor.name.clone()));
                }
                loaded_guest
            }
            Err(error) => {
                let guest_logs = self.drain_guest_log_messages(process.local_id);
                self.kernel.processes().record_activity(ActivityEvent {
                    kind: selium_abi::ActivityKind::ProcessExited,
                    process_id: Some(process.local_id),
                    message: format!(
                        "guest {} trapped: {error}; recent guest logs: {guest_logs:?}",
                        descriptor.name
                    ),
                });
                self.cleanup_failed_process(process.local_id)?;
                return Err(error);
            }
        };
        self.loaded_guests
            .lock()
            .insert(process.local_id, loaded_guest);
        // Start a multithreaded guest's worker pool now that the guest is
        // registered, so the pool monitor's reap can always find it. The
        // pool size is the configured per-guest count (default: available
        // CPU cores, never exceeding them unless explicitly configured). The
        // `loaded_guests` guard is released before the failure path's
        // `cleanup_failed_process` (which re-locks `loaded_guests`).
        let start_result = {
            let mut guests = self.loaded_guests.lock();
            match guests.get_mut(&process.local_id) {
                Some(guest) => match &mut guest.execution {
                    GuestExecution::Multithreaded(mt) => {
                        let worker_count = self.worker_count_for(&descriptor.name);
                        Some(mt.start_workers(self.clone(), process.local_id, worker_count))
                    }
                    _ => None,
                },
                None => None,
            }
        };
        if let Some(Err(error)) = start_result {
            self.cleanup_failed_process(process.local_id)?;
            return Err(error);
        }
        self.kernel.processes().record_activity(ActivityEvent {
            kind: selium_abi::ActivityKind::GuestBootstrapped,
            process_id: Some(process.local_id),
            message: format!("guest {} bootstrapped", descriptor.name),
        });
        info!(
            guest = descriptor.name.as_str(),
            process_id = process.local_id,
            "bootstrapped system guest"
        );

        // Registration publication belongs to the owner, not the host: the
        // process publishes its own node (and its named routes/handler claims)
        // when its entrypoint starts. The host merely attests authority.
        Ok(BootstrappedGuest {
            name: descriptor.name,
            process_id: process.local_id,
        })
    }

    pub(crate) fn load_guest_module(
        &self,
        module_bytes: &[u8],
        process_id: selium_abi::ProcessId,
    ) -> Result<LoadedGuest> {
        let store = self.kernel.memory().shared_store();
        let mut app = WasmApplication::with_store(store);
        let module_index = app
            .load_module_from_memory(module_bytes)
            .map_err(map_wasm_error)?;
        self.register_runtime_host_functions(&mut app, module_index, process_id)?;
        app.instantiate(module_index).map_err(map_wasm_error)?;
        app.execute_start(module_index).map_err(map_wasm_error)?;
        Ok(LoadedGuest {
            execution: GuestExecution::Cooperative { app, module_index },
            entrypoint_results: Vec::new(),
        })
    }

    /// AOT-compiles a multithreaded guest's module and instantiates the
    /// shared instance its worker pool will enter concurrently.
    pub(crate) fn load_multithreaded_guest(
        &self,
        module_bytes: &[u8],
        process_id: selium_abi::ProcessId,
    ) -> Result<LoadedGuest> {
        let mt = crate::multithreaded::MultithreadedGuest::load(self, process_id, module_bytes)?;
        Ok(LoadedGuest {
            execution: GuestExecution::Multithreaded(mt),
            entrypoint_results: Vec::new(),
        })
    }

    pub(crate) fn execute_entrypoint(
        &self,
        mut loaded_guest: LoadedGuest,
        descriptor: &SystemGuestDescriptor,
    ) -> Result<LoadedGuest> {
        let results = match &mut loaded_guest.execution {
            GuestExecution::Cooperative { app, module_index } => {
                let arguments = crate::wasm::resolve_entrypoint_arguments(
                    app,
                    *module_index,
                    &descriptor.arguments,
                )?;
                app.call_function(*module_index, descriptor.entrypoint.as_str(), &arguments)
                    .map_err(map_wasm_error)?
            }
            GuestExecution::Multithreaded(mt) => {
                mt.run_entrypoint(&descriptor.entrypoint, &descriptor.arguments)?
            }
        };
        loaded_guest.entrypoint_results = results;
        Ok(loaded_guest)
    }

    pub(crate) fn rollback_bootstrapped(&self, report: &BootstrapReport) {
        for guest in report.guests.iter().rev() {
            drop(self.stop_process(guest.process_id));
        }
    }
}

/// Returns the exact registry URIs a descriptor claims: its explicit
/// `RouteClaim` grants plus one claim per declared protocol handler scheme.
fn descriptor_claim_uris(descriptor: &SystemGuestDescriptor) -> Vec<String> {
    let mut uris: Vec<String> = descriptor
        .grants
        .iter()
        .filter(|grant| grant.capability == Capability::RouteClaim)
        .flat_map(|grant| {
            grant
                .selectors
                .iter()
                .filter_map(|selector| match selector {
                    ResourceSelector::Uri(uri) => Some(uri.clone()),
                    _ => None,
                })
        })
        .collect();
    for scheme in &descriptor.handlers {
        let uri = handler_claim_uri(scheme);
        if !uris.contains(&uri) {
            uris.push(uri);
        }
    }
    uris
}

/// Returns the descriptor's grants with a host-provisioned `RouteClaim` added
/// for each declared protocol handler scheme.
fn grant_uris_with_handler_claims(descriptor: &SystemGuestDescriptor) -> Vec<CapabilityGrant> {
    let mut grants = descriptor.grants.clone();
    for scheme in &descriptor.handlers {
        let uri = handler_claim_uri(scheme);
        let already_claimed = grants.iter().any(|grant| {
            grant.capability == Capability::RouteClaim
                && grant.selectors.iter().any(
                    |selector| matches!(selector, ResourceSelector::Uri(claimed) if claimed == &uri),
                )
        });
        if !already_claimed {
            grants.push(CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri(uri)],
            ));
        }
    }
    grants
}

/// The exact registry URI a protocol handler claim covers.
fn handler_claim_uri(scheme: &str) -> String {
    format!("{}/handler/{scheme}", selium_abi::uri::SEL_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::{Capability, CapabilityGrant, LocalityScope, ResourceClass, ResourceSelector};
    use selium_kernel::Kernel;
    use wasmtiny::WasmValue;

    /// A unique temporary state directory for a test.
    fn temp_state_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "selium-{tag}-{}-{nanos}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn state_root_boot_is_clean_against_fresh_and_existing_directories() {
        let dir = temp_state_dir("state-root");
        let boot = |dir: &std::path::Path| {
            let runtime = Runtime::default();
            runtime
                .bootstrap_system_guests(RuntimeConfig {
                    start_discovery: false,
                    system_guests: Vec::new(),
                    state_dir: Some(dir.to_path_buf()),
                })
                .expect("clean bootstrap");
            runtime
        };

        // A fresh directory: the volume layer is rooted there and a hierarchy
        // is generated and persisted.
        let fresh = boot(&dir);
        assert_eq!(fresh.kernel().volumes().root(), dir);
        let first_cert = fresh
            .keyring_intermediate_cert()
            .expect("first boot installs a keyring");

        // An existing directory: same root, same hierarchy, still clean.
        let existing = boot(&dir);
        assert_eq!(existing.kernel().volumes().root(), dir);
        assert_eq!(
            existing
                .keyring_intermediate_cert()
                .expect("restart reuses the keyring"),
            first_cert,
            "a restart must not replace the persisted hierarchy"
        );

        drop(std::fs::remove_dir_all(&dir));
    }

    // scenario: selium-runtime/state-root-boot/usage-reconstructed-at-boot
    // scenario: selium-abi/tenant-volume-usage-hostcall/reading-survives-a-restart
    #[test]
    fn resident_volume_bytes_seed_the_tenant_usage_at_boot() {
        let dir = temp_state_dir("usage-seed");
        // A pre-populated state directory: two tenants hold resident bytes.
        {
            let kernel = Kernel::with_state_dir(1, &dir);
            let volumes = kernel.volumes();
            let volume = volumes.open(Some("acme"), "orders").expect("open acme");
            volumes
                .append(volume.local_id, b"0123456789")
                .expect("append");
            let volume = volumes.open(Some("beta"), "assets").expect("open beta");
            volumes.append(volume.local_id, b"abc").expect("append");
        }

        let runtime = Runtime::default();
        runtime
            .bootstrap_system_guests(RuntimeConfig {
                start_discovery: false,
                system_guests: Vec::new(),
                state_dir: Some(dir.clone()),
            })
            .expect("bootstrap");

        let quota = runtime.kernel().quota();
        assert_eq!(
            quota.used("acme", ResourceClass::Volume),
            10,
            "acme's resident bytes are reconstructed before any guest runs"
        );
        assert_eq!(quota.used("beta", ResourceClass::Volume), 3);

        drop(std::fs::remove_dir_all(&dir));
    }

    fn module_with_entrypoint(entrypoint: &str, body: &str) -> Vec<u8> {
        wat::parse_str(format!("(module (func (export \"{entrypoint}\") {body}))"))
            .expect("compile wat")
    }

    fn module_with_runtime_bridge(entrypoint: &str) -> Vec<u8> {
        wat::parse_str(format!(
            "(module
                (import \"selium\" \"process_id\" (func $process_id (result i64)))
                (func (export \"{entrypoint}\") (result i64)
                    call $process_id))"
        ))
        .expect("compile runtime bridge wat")
    }

    // scenario: selium-runtime/wasmtiny-backed-guest-execution/runtime-starts-a-guest-module
    // scenario: selium-kernel/primitive-process-lifecycle/runtime-starts-configured-guest-process
    #[test]
    fn runtime_bootstraps_guests_from_config() {
        let runtime = Runtime::default();
        let config = RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: "cluster".to_string(),
                module_id: "cluster-module".to_string(),
                module_bytes: module_with_entrypoint("boot", "(result i32) i32.const 7"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::Locality(LocalityScope::Cluster)],
                )],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        };

        let report = runtime
            .bootstrap_system_guests(config)
            .expect("bootstrap guests");
        assert_eq!(report.guests.len(), 1);
        assert_eq!(runtime.loaded_guest_count(), 1);
        assert_eq!(
            runtime
                .entrypoint_results(report.guests[0].process_id)
                .expect("entrypoint results"),
            vec![WasmValue::I32(7)]
        );
    }

    // scenario: selium-runtime/entrypoint-failure-detection/void-entrypoint-no-return-value-proceeds-normally
    #[test]
    fn declaration_order_is_the_boot_order() {
        // Guests spawn in declaration order in a single pass; there is no
        // dependency sequencer.
        let runtime = Runtime::default();
        let guest = |name: &str| SystemGuestDescriptor {
            name: name.to_string(),
            module_id: format!("{name}-module"),
            module_bytes: module_with_entrypoint("boot", ""),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: vec![CapabilityGrant::new(
                Capability::ProcessLifecycle,
                vec![ResourceSelector::Locality(LocalityScope::Cluster)],
            )],
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        };

        let report = runtime
            .bootstrap_system_guests(RuntimeConfig {
                start_discovery: false,
                system_guests: vec![guest("zulu"), guest("alpha"), guest("mike")],
                state_dir: None,
            })
            .expect("bootstrap guests");

        let order: Vec<&str> = report
            .guests
            .iter()
            .map(|bootstrapped| bootstrapped.name.as_str())
            .collect();
        assert_eq!(order, vec!["zulu", "alpha", "mike"]);
    }

    #[test]
    fn duplicate_route_claims_across_descriptors_are_rejected() {
        let runtime = Runtime::default();
        let guest = |name: &str, claim: &str| SystemGuestDescriptor {
            name: name.to_string(),
            module_id: format!("{name}-module"),
            module_bytes: module_with_entrypoint("boot", ""),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: vec![CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri(claim.to_string())],
            )],
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        };

        let result = runtime.bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![
                guest("first", "sel:///control"),
                guest("second", "sel:///control"),
            ],
            state_dir: None,
        });
        assert!(
            result.is_err(),
            "two descriptors may not claim the same reserved name"
        );
        // The guest started before the duplicate was noticed is rolled back, so
        // a rejected configuration leaves nothing running.
        assert_eq!(
            runtime.loaded_guest_count(),
            0,
            "a rejected bootstrap must not leak the guests it already started"
        );
    }

    // scenario: selium-runtime/protocol-handler-route-claims/handler-claim-is-host-provisioned
    #[test]
    fn handler_schemes_provision_route_claims() {
        let runtime = Runtime::default();
        let guest = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "handler".to_string(),
                module_id: "handler-module".to_string(),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
                discovery_service: false,
                handlers: vec!["sel-quic".to_string()],
            })
            .expect("spawn handler guest");

        assert_eq!(
            runtime.expected_claims(guest.process_id),
            vec!["sel:///handler/sel-quic".to_string()]
        );
    }

    #[test]
    fn runtime_registers_host_import_bridge_for_guest_modules() {
        let runtime = Runtime::default();
        let bootstrapped = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "bridged".to_string(),
                module_id: "bridged-module".to_string(),
                module_bytes: module_with_runtime_bridge("boot"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::ActivityRead,
                    vec![ResourceSelector::Locality(LocalityScope::Cluster)],
                )],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            })
            .expect("spawn bridged guest");

        let results = runtime
            .entrypoint_results(bootstrapped.process_id)
            .expect("entrypoint results");
        assert_eq!(
            results,
            vec![WasmValue::I64(bootstrapped.process_id as i64)]
        );
    }

    // scenario: selium-runtime/entrypoint-failure-detection/zero-exit-code-proceeds-normally
    #[test]
    fn guest_with_i32_zero_entrypoint_result_bootstraps_normally() {
        let runtime = Runtime::default();
        let config = RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: "ok-guest".to_string(),
                module_id: "ok-module".to_string(),
                module_bytes: module_with_entrypoint("boot", "(result i32) i32.const 0"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::Locality(LocalityScope::Cluster)],
                )],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        };

        let report = runtime
            .bootstrap_system_guests(config)
            .expect("bootstrap guests");
        assert_eq!(report.guests.len(), 1);
        assert_eq!(
            runtime
                .entrypoint_results(report.guests[0].process_id)
                .expect("entrypoint results"),
            vec![WasmValue::I32(0)]
        );
    }

    // scenario: selium-runtime/entrypoint-failure-detection/non-zero-exit-code-fails-bootstrap
    #[test]
    fn guest_with_i32_one_entrypoint_result_fails_with_entrypoint_failed() {
        let runtime = Runtime::default();
        let config = RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: "fail-guest".to_string(),
                module_id: "fail-module".to_string(),
                module_bytes: module_with_entrypoint("boot", "(result i32) i32.const 1"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::Locality(LocalityScope::Cluster)],
                )],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        };

        let err = runtime
            .bootstrap_system_guests(config)
            .expect_err("should fail with EntrypointFailed");
        assert!(
            matches!(err, Error::EntrypointFailed(ref name) if name == "fail-guest"),
            "expected EntrypointFailed, got {err:?}"
        );
    }

    // scenario: selium-runtime/discovery-enabled-bootstrap/readiness-is-per-guest
    // scenario: route-watch/health-is-published-state/missing-health-is-not-a-boot-failure
    #[test]
    fn bootstrap_does_not_gate_on_readiness() {
        // A guest that never registers a serving route still boots: there is
        // no readiness gate.
        let runtime = Runtime::default();
        let config = RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: "silent".to_string(),
                module_id: "silent-module".to_string(),
                module_bytes: module_with_entrypoint("boot", "(result i32) i32.const 0"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::Locality(LocalityScope::Cluster)],
                )],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        };

        let report = runtime
            .bootstrap_system_guests(config)
            .expect("a guest without a registration still boots");
        assert_eq!(report.guests.len(), 1);
    }
}
