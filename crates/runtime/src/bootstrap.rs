use std::{
    collections::{BTreeMap, BTreeSet},
    thread,
    time::{Duration, Instant},
};

use selium_abi::{
    ActivityEvent, Capability, CapabilityGrant, ResourceClass, ResourceIdentity, ResourceSelector,
};
use selium_service::{DiscoveryRequest, ResourceTarget};
use selium_shm::{Channel, ChannelBackpressure, transport::ShmTransport};
use selium_wire::{framed::FramedWrite, pubsub::Publisher};
use tracing::info;
use wasmtiny::{WasmApplication, WasmValue};

use crate::{
    Error, Result,
    config::{
        BootstrapReport, BootstrappedGuest, ReadinessCondition, RuntimeConfig, SystemGuestArg,
        SystemGuestDescriptor,
    },
    error::map_wasm_error,
    runtime::{DiscoveryPublisher, Runtime},
};

const DEFAULT_READINESS_POLL_MS: u64 = 10;
const DEFAULT_READINESS_TIMEOUT_MS: u64 = 1_000;

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

impl Runtime {
    /// Boots all configured system guests in dependency order.
    pub fn bootstrap_system_guests(&self, mut config: RuntimeConfig) -> Result<BootstrapReport> {
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
            let runtime = self.clone();
            std::thread::Builder::new()
                .name("selium-metering-ticker".to_string())
                .spawn(move || {
                    // The CPU budget refresh runs on the same ticker: each live
                    // process's engine execution budget is re-applied every
                    // second, so a ceiling authored or withdrawn between
                    // windows lands within one sampling interval. The budget
                    // *window* itself re-anchors only when the wall-clock
                    // minute rolls (see `refresh_cpu_budgets`).
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        runtime.metering_tick();
                        runtime.refresh_cpu_budgets();
                    }
                })
                .map_err(|error| Error::Host(format!("metering ticker spawn failed: {error}")))?;
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
                if descriptor.name == "discovery" {
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

        // Seed the out-of-band domain→tenant table into discovery (a no-op
        // when discovery is not enabled).
        for (domain, tenant) in &config.domain_table {
            self.publish_seed_domain(domain, tenant)?;
        }

        let mut pending = BTreeMap::new();
        for descriptor in config.system_guests {
            let name = descriptor.name.clone();
            if pending.insert(name.clone(), descriptor).is_some() {
                return Err(Error::DuplicateDescriptor(name));
            }
        }

        let mut ready = BTreeSet::new();
        let mut report = BootstrapReport::default();

        while !pending.is_empty() {
            let ready_name = pending.iter().find_map(|(name, descriptor)| {
                descriptor
                    .dependencies
                    .iter()
                    .all(|dependency| ready.contains(dependency))
                    .then_some(name.clone())
            });
            let Some(name) = ready_name else {
                if let Some(missing_dependency) = pending
                    .values()
                    .flat_map(|descriptor| descriptor.dependencies.iter())
                    .find(|dependency| {
                        !ready.contains(*dependency) && !pending.contains_key(*dependency)
                    })
                {
                    self.rollback_bootstrapped(&report);
                    return Err(Error::UnknownDependency(missing_dependency.clone()));
                }
                self.rollback_bootstrapped(&report);
                return Err(Error::DependencyCycle);
            };

            let descriptor = pending
                .remove(&name)
                .ok_or_else(|| Error::DescriptorNotFound(name.clone()))?;
            let bootstrapped = match self.spawn_system_guest(descriptor.clone()) {
                Ok(bootstrapped) => bootstrapped,
                Err(error) => {
                    self.rollback_bootstrapped(&report);
                    return Err(error);
                }
            };
            // A role-declared system guest is ready only when its declared
            // route is observable in discovery (recorded by the discovery
            // service), on top of the ordinary readiness signal.
            let role_registered = match descriptor.serving_role.as_deref() {
                Some(role) => self.wait_for_registration(bootstrapped.process_id, role),
                None => true,
            };
            if !self.wait_for_readiness(bootstrapped.process_id, &descriptor.readiness)
                || !role_registered
            {
                drop(self.stop_process(bootstrapped.process_id));
                self.rollback_bootstrapped(&report);
                return Err(Error::ReadinessUnsatisfied(descriptor.name));
            }
            ready.insert(name);
            report.guests.push(bootstrapped);
        }

        Ok(report)
    }

    /// Publishes an out-of-band domain→tenant seed event to the discovery
    /// feed. A no-op when discovery is not enabled.
    fn publish_seed_domain(&self, domain: &str, tenant: &str) -> Result<()> {
        let request = DiscoveryRequest::SeedDomain {
            domain: domain.to_string(),
            tenant: tenant.to_string(),
        };
        self.publish_discovery_event(request)
    }

    /// Waits until the discovery service has recorded a registration for
    /// `role_uri` by `process_id`, with the same timeout budget as the
    /// ordinary readiness poll. `mark_ready()` carries no payload; readiness
    /// is verified by observing the registration record in discovery.
    fn wait_for_registration(&self, process_id: selium_abi::ProcessId, role_uri: &str) -> bool {
        let deadline = Instant::now() + Duration::from_millis(DEFAULT_READINESS_TIMEOUT_MS);
        loop {
            if self.has_registration(process_id, role_uri) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(DEFAULT_READINESS_POLL_MS));
        }
    }

    /// Creates the discovery pub/sub feed ring and RPC listener.
    ///
    /// Stores the publisher and listener shared id in runtime state, and returns
    /// the feed region id and listener shared id so bootstrap can wire them into
    /// the discovery guest descriptor.
    fn setup_discovery(&self) -> Result<(u64, u64)> {
        let feed_channel = Channel::create_with_backpressure(
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
        self.validate_grants(&descriptor.grants)?;

        let process = self.kernel.processes().start_process(
            descriptor.module_id.clone(),
            descriptor.entrypoint.clone(),
            descriptor.grants.clone(),
        );
        self.persist_process_authority(
            process.local_id,
            descriptor.grants.clone(),
            descriptor.tenant.clone(),
            None,
        );

        // Shared-page fast-path detection: probe the guest's module bytes
        // once at spawn (shared memory declaration + atomic notify
        // opcodes). Region attach consumes the result; no user-facing
        // configuration exists (see the shared-page-fastpath spec).
        let fastpath = crate::module_probe::probe(&descriptor.module_bytes);
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
        self.claim_local_handle(
            process.local_id,
            selium_abi::ResourceClass::Process,
            process.local_id,
        );
        self.register_module_bytes(
            descriptor.module_id.clone(),
            descriptor.module_bytes.clone(),
        )?;
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

        // Record the discovery service's process identity so the runtime can
        // restrict `RecordResolvedQueueFor` to the trusted discovery guest.
        if descriptor.name == "discovery" {
            *self.discovery_process.lock() = Some(process.local_id);
        }

        // Register the process node (`sel://<tenant>/proc/<id>`) so every
        // process is discoverable from spawn. Publishing is a no-op when
        // discovery is not enabled.
        if let Err(error) =
            self.register_process_node(process.local_id, descriptor.tenant.as_deref())
        {
            self.cleanup_failed_process(process.local_id)?;
            return Err(error);
        }

        // Remember the protocol schemes this guest handles so serve-side
        // guests can pin it via `ResolveProtocolHandler`.
        if !descriptor.handlers.is_empty() {
            self.handler_schemes
                .lock()
                .insert(process.local_id, descriptor.handlers.clone());
        }

        Ok(BootstrappedGuest {
            name: descriptor.name,
            process_id: process.local_id,
        })
    }

    /// Records and publishes the process-node registration for a spawned
    /// process so it is addressable as `sel://<tenant>/proc/<id>` from spawn
    /// until teardown.
    fn register_process_node(
        &self,
        process_id: selium_abi::ProcessId,
        tenant: Option<&str>,
    ) -> Result<()> {
        let uri =
            crate::discovery::process_registration_uri(tenant.unwrap_or_default(), process_id);
        let target = ResourceTarget {
            uri: uri.clone(),
            host_id: String::new(), // Runtime doesn't know host_id; discovery will fill it.
            resource_id: process_id,
            interface: None,
            tenant: tenant.map(str::to_string),
            class: ResourceClass::Process,
            labels: Vec::new(),
        };
        let request = DiscoveryRequest::Register {
            uri,
            target,
            owner: Some(process_id),
            root_service: false,
        };
        self.publish_discovery_event(request)?;
        Ok(())
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

    pub(crate) fn wait_for_readiness(
        &self,
        process_id: selium_abi::ProcessId,
        condition: &ReadinessCondition,
    ) -> bool {
        match condition {
            ReadinessCondition::Immediate => true,
            ReadinessCondition::ActivityLogContains(fragment) => {
                let deadline = Instant::now() + Duration::from_millis(DEFAULT_READINESS_TIMEOUT_MS);
                let mut cursor = 0;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let events = self
                        .kernel
                        .processes()
                        .wait_for_activity_from(cursor, remaining.as_millis() as u64);
                    cursor += events.len();
                    if events.iter().any(|event| {
                        event.process_id == Some(process_id) && event.message.contains(fragment)
                    }) {
                        return true;
                    }
                    if Instant::now() >= deadline {
                        return false;
                    }
                    thread::sleep(Duration::from_millis(DEFAULT_READINESS_POLL_MS));
                }
            }
        }
    }

    pub(crate) fn rollback_bootstrapped(&self, report: &BootstrapReport) {
        for guest in report.guests.iter().rev() {
            drop(self.stop_process(guest.process_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::{Capability, CapabilityGrant, LocalityScope, ResourceSelector};
    use wasmtiny::WasmValue;

    fn module_with_entrypoint(entrypoint: &str, body: &str) -> Vec<u8> {
        wat::parse_str(format!("(module (func (export \"{entrypoint}\") {body}))"))
            .expect("compile wat")
    }

    fn module_with_runtime_bridge(entrypoint: &str) -> Vec<u8> {
        wat::parse_str(format!(
            "(module
                (import \"selium\" \"process_id\" (func $process_id (result i64)))
                (import \"selium\" \"mark_ready\" (func $mark_ready))
                (func (export \"{entrypoint}\") (result i64)
                    call $mark_ready
                    call $process_id))"
        ))
        .expect("compile runtime bridge wat")
    }

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
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            }],
            domain_table: Vec::new(),
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
                dependencies: Vec::new(),
                readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
                tenant: None,
                serving_role: None,
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
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            }],
            domain_table: Vec::new(),
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
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            }],
            domain_table: Vec::new(),
        };

        let err = runtime
            .bootstrap_system_guests(config)
            .expect_err("should fail with EntrypointFailed");
        assert!(
            matches!(err, Error::EntrypointFailed(ref name) if name == "fail-guest"),
            "expected EntrypointFailed, got {err:?}"
        );
    }

    #[test]
    fn role_declared_readiness_requires_observable_registration() {
        let runtime = Runtime::default();
        // No registration recorded yet → the declared role is not observable.
        assert!(!runtime.has_registration(42, "sel:///dns/resolve"));
        // Once discovery records the registration, readiness is satisfied.
        runtime.record_registration(42, "sel:///dns/resolve".to_string());
        assert!(runtime.has_registration(42, "sel:///dns/resolve"));
    }

    #[test]
    fn silent_role_guest_fails_readiness() {
        // A role-declared guest whose registration is never observable in
        // discovery does not reach ready.
        let runtime = Runtime::default();
        let config = RuntimeConfig {
            start_discovery: false,
            domain_table: Vec::new(),
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
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: Some("sel:///dns/resolve".to_string()),
                handlers: Vec::new(),
            }],
        };

        let err = runtime
            .bootstrap_system_guests(config)
            .expect_err("a silent role guest must not be admitted as ready");
        assert!(
            matches!(err, Error::ReadinessUnsatisfied(ref name) if name == "silent"),
            "expected ReadinessUnsatisfied, got {err:?}"
        );
    }
}
