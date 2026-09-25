use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use selium_abi::{
    AbiError, AbiErrorCode, Capability, CapabilityGrant, CompletionState, GuestLogEntry,
    HostcallOutput, HostcallRequest, Namespace, OperationId, ProcessId, ResourceClass,
    ResourceIdentity, ResourceSelector, ScopeContext, TaskId,
};
use selium_service::{DiscoveryRequest, FlatMsg, ResourceTarget, log::LogRecord};
use wasmtiny::{RegionProt as WasmProt, runtime::SharedMemory};

use crate::{ReadinessCondition, SystemGuestDescriptor, error::kernel_error, runtime::Runtime};

#[derive(Debug, Clone)]
pub(crate) enum HostOperationState {
    Ready(HostcallOutput),
    Failed(AbiError),
    HostQueueRecvWait { local_id: u64, deadline: Instant },
    SleepWait { deadline: Instant },
}

#[derive(Debug, Clone)]
pub(crate) struct HostOperation {
    pub(crate) process_id: ProcessId,
    pub(crate) task_id: Option<TaskId>,
    pub(crate) state: HostOperationState,
}

/// The delegation scope implied by an authority's `DelegateGrants` grants:
/// root-wide (a `Namespace::Root` selector), or tenant-scoped (the named
/// tenants from `Tenant`/`Namespace::Tenant` selectors).
enum DelegationScope {
    Root,
    Tenants(HashSet<String>),
}

impl DelegationScope {
    /// Returns whether this scope admits a child spawn in `tenant`.
    fn admits_tenant(&self, tenant: &str) -> bool {
        match self {
            Self::Root => true,
            Self::Tenants(tenants) => tenants.contains(tenant),
        }
    }
}

impl Runtime {
    /// Begins a hostcall for a process and returns its initial status and operation id.
    pub fn begin_hostcall(
        &self,
        process_id: ProcessId,
        request: HostcallRequest,
    ) -> (u32, OperationId) {
        self.begin_hostcall_with_task(process_id, request, None, None)
    }

    pub(crate) fn begin_hostcall_with_task(
        &self,
        process_id: ProcessId,
        request: HostcallRequest,
        task_id: Option<TaskId>,
        guest_memory: Option<SharedMemory>,
    ) -> (u32, OperationId) {
        // Extract WaitRegister fields before dispatch consumes the request.
        let wait_register = match &request {
            HostcallRequest::WaitRegister {
                region_id,
                generation,
            } => Some((*region_id, *generation)),
            _ => None,
        };

        let state = match self.dispatch_hostcall(process_id, request, guest_memory.as_ref()) {
            Ok(state) => state,
            Err(error) => HostOperationState::Failed(error),
        };
        let status = match state {
            HostOperationState::Ready(_) => selium_abi::HOSTCALL_STATUS_READY,
            HostOperationState::Failed(_) => selium_abi::HOSTCALL_STATUS_FAILED,
            HostOperationState::HostQueueRecvWait { .. } => selium_abi::HOSTCALL_STATUS_PENDING,
            HostOperationState::SleepWait { .. } => selium_abi::HOSTCALL_STATUS_PENDING,
        };

        // Register the wait if the guest parked on a host-writable ring.
        // This is a fire-and-forget registration: the hostcall returns
        // Ready immediately; the wake comes later via mailbox.
        if let HostOperationState::Ready(_) = &state
            && let Some(tid) = task_id
            && let Some((region_id, generation)) = wait_register
        {
            self.register_wait(process_id, tid, region_id, generation);
        }

        let mut operations = self.operations.lock();
        let operation_id = self.next_operation_id(&operations);
        operations.insert(
            operation_id,
            HostOperation {
                process_id,
                task_id,
                state: state.clone(),
            },
        );

        // Register SleepWait timer so the guest is woken via mailbox when
        // the deadline arrives. The wake is spawned context-free: a guest
        // executed inline on a non-Tokio thread (e.g. the kernel poller's
        // datagram-wake path) has no ambient reactor, so reuse a handle
        // captured from the first Tokio-threaded hostcall — falling back to
        // a minimal dedicated runtime when no Tokio context was ever seen.
        if let HostOperationState::SleepWait { deadline } = state
            && let Some(tid) = task_id
        {
            let duration = deadline.saturating_duration_since(Instant::now());
            let operations = Arc::clone(&self.operations);
            let runtime = self.clone();
            let timer = async move {
                tokio::time::sleep(duration).await;
                if let Some(op) = operations.lock().get_mut(&operation_id) {
                    op.state = HostOperationState::Ready(HostcallOutput::Empty);
                }
                // Deliver through the standard wake path (mailbox enqueue +
                // inline guest poll). A bare enqueue is a lost wake when the
                // guest reactor is stalled with only this timer outstanding:
                // nothing else would ever poll it, deadlocking timer-driven
                // progress (e.g. quinn loss detection, pacing, delayed ACKs).
                runtime.wake_process_task(process_id, tid);
            };
            let ambient = tokio::runtime::Handle::try_current().ok();
            if let Some(handle) = ambient.as_ref() {
                drop(self.timer_handle.set(handle.clone()));
            }
            match ambient.or_else(|| self.timer_handle.get().cloned()) {
                Some(handle) => drop(handle.spawn(timer)),
                None => drop(std::thread::spawn(move || {
                    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                    {
                        runtime.block_on(timer);
                    }
                })),
            }
        }

        (status, operation_id)
    }

    /// Polls a hostcall operation for completion.
    pub fn poll_hostcall(
        &self,
        process_id: ProcessId,
        operation_id: OperationId,
    ) -> CompletionState {
        let mut operations = self.operations.lock();
        let Some(operation) = operations.get_mut(&operation_id) else {
            return CompletionState::Failed(AbiError::new(
                AbiErrorCode::InvalidHandle,
                format!("unknown operation {operation_id}"),
            ));
        };
        if operation.process_id != process_id {
            return CompletionState::Failed(AbiError::new(
                AbiErrorCode::PermissionDenied,
                "operation belongs to another process",
            ));
        }

        match operation.state.clone() {
            HostOperationState::Ready(output) => CompletionState::Ready(output.clone()),
            HostOperationState::Failed(error) => CompletionState::Failed(error.clone()),
            HostOperationState::HostQueueRecvWait { local_id, deadline } => {
                match self.kernel.queues().try_host_queue_recv(local_id) {
                    Ok(Some((client_process_id, value, metadata))) => {
                        // The item left the queue: release its pipe slot.
                        self.release_queue_item(
                            self.kernel
                                .queues()
                                .host_queue_shared_id(local_id)
                                .unwrap_or_default(),
                        );
                        // Queue handoff: transfer region ownership on recv.
                        self.transfer_region_ownership_on_recv(
                            operation.process_id,
                            client_process_id,
                            value,
                        );
                        if client_process_id == 0 {
                            // Kernel-originated rendezvous (e.g. the network
                            // poller enqueuing an accepted connection's
                            // stream region): the kernel created and owns
                            // the region outside `shared_resource_owners`,
                            // so grant it to the receiver explicitly.
                            self.claim_shared_resource(
                                operation.process_id,
                                ResourceClass::SharedRegion,
                                value,
                            );
                        }
                        let output = HostcallOutput::ConnectionInfo {
                            client_process_id,
                            value,
                            metadata,
                        };
                        operation.state = HostOperationState::Ready(output.clone());
                        CompletionState::Ready(output)
                    }
                    Ok(None) if Instant::now() >= deadline => {
                        let error =
                            AbiError::new(AbiErrorCode::Timeout, "host queue recv timed out");
                        operation.state = HostOperationState::Failed(error.clone());
                        CompletionState::Failed(error)
                    }
                    Ok(None) => CompletionState::Pending { operation_id },
                    Err(error) => CompletionState::Failed(kernel_error(error)),
                }
            }
            HostOperationState::SleepWait { deadline } => {
                if Instant::now() >= deadline {
                    operation.state = HostOperationState::Ready(HostcallOutput::Empty);
                    CompletionState::Ready(HostcallOutput::Empty)
                } else {
                    CompletionState::Pending { operation_id }
                }
            }
        }
    }

    /// Drops a hostcall operation if it belongs to the supplied process.
    pub fn drop_hostcall(&self, process_id: ProcessId, operation_id: OperationId) -> bool {
        let mut operations = self.operations.lock();
        if operations
            .get(&operation_id)
            .is_some_and(|operation| operation.process_id == process_id)
        {
            operations.remove(&operation_id);
            true
        } else {
            false
        }
    }

    /// Authorizes a principal-provenance allocation for the supplied
    /// serving tenant and returns the principal tenant the resource is
    /// minted under.
    ///
    /// A serving tenant equal to the allocating process's own tenant (or
    /// absent) needs no extra authority. A root principal (no tenant) may
    /// mint for any tenant — connectors and other trusted edge
    /// infrastructure run as root. A tenant-scoped process may mint for
    /// another tenant only with a `DelegateGrants` grant scoped to that
    /// tenant.
    fn authorize_serving_tenant(
        &self,
        process_id: ProcessId,
        serving_tenant: Option<&str>,
    ) -> std::result::Result<String, AbiError> {
        let own_tenant = self.process_tenant(process_id);
        let Some(requested) = serving_tenant else {
            return Ok(own_tenant.unwrap_or_default());
        };
        if own_tenant.as_deref() == Some(requested) || own_tenant.is_none() {
            return Ok(requested.to_string());
        }
        if self.authorises(
            process_id,
            Capability::DelegateGrants,
            &ScopeContext {
                tenant: Some(requested.to_string()),
                ..ScopeContext::default()
            },
        ) {
            return Ok(requested.to_string());
        }
        Err(AbiError::new(
            AbiErrorCode::PermissionDenied,
            format!(
                "cross-tenant allocation for tenant {requested:?} requires a root principal or tenant-scoped delegation"
            ),
        ))
    }

    fn dispatch_hostcall(
        &self,
        process_id: ProcessId,
        request: HostcallRequest,
        guest_memory: Option<&SharedMemory>,
    ) -> std::result::Result<HostOperationState, AbiError> {
        if !self.process_authorities.lock().contains_key(&process_id) {
            return Err(AbiError::new(
                AbiErrorCode::InvalidHandle,
                format!("unknown process authority {process_id}"),
            ));
        }

        match request {
            HostcallRequest::AllocRegion {
                pages,
                prot,
                purpose,
                serving_tenant,
            } => {
                // Ignore unused `prot` field; `purpose` is informational.
                let _prot = prot;
                let _purpose = purpose;

                self.require(
                    process_id,
                    Capability::SharedMemory,
                    ResourceClass::SharedRegion,
                    None,
                )?;

                // Principal provenance: the region is minted under the serving
                // tenant, which may differ from the allocating process's own
                // tenant only for a root principal or with tenant-scoped
                // delegation.
                let principal =
                    self.authorize_serving_tenant(process_id, serving_tenant.as_deref())?;

                let size_bytes = (pages as u64) * 65536; // WASM page size
                let size_u32 = u32::try_from(size_bytes).map_err(|_error| {
                    AbiError::new(AbiErrorCode::MalformedPayload, "region size exceeds u32")
                })?;

                // Quota primitives cap extent independently of grants: reserve
                // the byte size against the serving tenant's shared-memory
                // ceiling before any region is allocated.
                self.enforce_quota(&principal, ResourceClass::SharedRegion, size_bytes)?;

                // Allocate region in the shared registry (standalone, no guest mapping yet).
                let (shared_id, _len) = self
                    .kernel
                    .memory()
                    .allocate_shared_region(size_u32)
                    .map_err(kernel_error)?;

                // Note: we do NOT auto-attach here. The allocating process must
                // call `AttachRegion` to map the region into its linear memory.
                // This avoids double-attachment when the caller also calls
                // AttachRegion with different protection/reader-slot parameters.
                let page_offset = 0;

                self.claim_shared_resource(process_id, ResourceClass::SharedRegion, shared_id);

                // Tier-1 discovery registration: one canonical typed URI.
                let uri = crate::discovery::region_registration_uri(&principal, shared_id);
                let target = ResourceTarget {
                    uri: uri.clone(),
                    host_id: String::new(), // Runtime doesn't know host_id; discovery will fill it.
                    resource_id: shared_id,
                    interface: None,
                    tenant: (!principal.is_empty()).then_some(principal.clone()),
                    class: ResourceClass::SharedRegion,
                    labels: Vec::new(),
                };
                let request = DiscoveryRequest::Register {
                    uri,
                    target,
                    owner: Some(process_id),
                    root_service: false,
                };
                if let Err(error) = self.publish_discovery_event(request) {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        format!("discovery publish failed: {error}"),
                    ));
                }

                // Remember the serving tenant so FreeRegion can revoke the URI.
                self.region_tenants
                    .lock()
                    .insert((process_id, shared_id), principal);

                Ok(HostOperationState::Ready(HostcallOutput::RegionAlloc(
                    selium_abi::RegionAllocation {
                        region_id: shared_id,
                        page_offset,
                    },
                )))
            }
            HostcallRequest::FreeRegion { region_id } => {
                self.ensure_shared_resource_owner(
                    process_id,
                    Capability::SharedMemory,
                    ResourceClass::SharedRegion,
                    region_id,
                )?;

                // Capture the region size and serving tenant before teardown so
                // the quota reservation made at AllocRegion can be released.
                let region_len = self
                    .kernel
                    .memory()
                    .shared_region_len(region_id)
                    .map_err(kernel_error)?;
                let serving_tenant = self
                    .region_tenants
                    .lock()
                    .get(&(process_id, region_id))
                    .cloned();

                // Detach the region from ALL loaded guests' wasm memory.
                let wasm_region_id = self
                    .kernel
                    .memory()
                    .wasmtiny_region_id(region_id)
                    .map_err(kernel_error)?;
                let mut guests = self.loaded_guests.lock();
                let to_detach: Vec<ProcessId> = guests.keys().copied().collect();
                for pid in to_detach {
                    if let Some(guest) = guests.get_mut(&pid) {
                        match &mut guest.execution {
                            crate::bootstrap::GuestExecution::Cooperative { app, module_index } => {
                                drop(app.detach_shared_region(*module_index, wasm_region_id));
                            }
                            // The AOT instance detaches from the shared
                            // linear memory; its mapping list tracks the
                            // attachment the same way the interpreter's does.
                            crate::bootstrap::GuestExecution::Multithreaded(mt) => {
                                drop(mt.detach_shared_region(wasm_region_id));
                            }
                        }
                    }
                }
                drop(guests);

                // Detach all kernel-level mappings for this region before destroying.
                self.kernel.memory().detach_all_shared_mappings(region_id);

                self.kernel
                    .memory()
                    .destroy_shared_region(region_id)
                    .map_err(kernel_error)?;
                // Every attachment was force-detached above, so every
                // fast-path eligibility vote for this region is stale.
                self.clear_fast_path_attachments(region_id);
                self.release_shared_resource(process_id, &ResourceClass::SharedRegion, region_id);

                // Return the region's byte size to the serving tenant's
                // shared-memory quota reservation.
                if let Some(tenant) = serving_tenant {
                    self.kernel.quota().release(
                        &tenant,
                        ResourceClass::SharedRegion,
                        u64::from(region_len),
                    );
                }

                // Tier-1 discovery revocation: publish a Revoke for the region URI.
                if let Some(tenant) = self.region_tenants.lock().remove(&(process_id, region_id)) {
                    let uri = crate::discovery::region_registration_uri(&tenant, region_id);
                    let request = DiscoveryRequest::Revoke { uri };
                    if let Err(error) = self.publish_discovery_event(request) {
                        return Err(AbiError::new(
                            AbiErrorCode::Internal,
                            format!("discovery publish failed: {error}"),
                        ));
                    }
                }

                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::AttachRegion {
                region_id,
                reader_slot,
                prot,
            } => {
                self.require(
                    process_id,
                    Capability::SharedMemory,
                    ResourceClass::SharedRegion,
                    Some(ResourceIdentity::Shared(region_id)),
                )?;
                self.ensure_attach_authorised(process_id, region_id)?;

                let page_offset = if let Some(memory) = &guest_memory {
                    // Attach directly into the calling guest's memory. This
                    // works while the guest is mid-execution (e.g. inside its
                    // entrypoint), when its `WasmApplication` is borrowed by
                    // the executor and unavailable through the loaded-guest
                    // table.
                    let mut memory = memory.lock().map_err(|_lock_err| {
                        AbiError::new(
                            AbiErrorCode::Internal,
                            "guest memory lock poisoned".to_string(),
                        )
                    })?;
                    self.kernel
                        .memory()
                        .attach_shared_region_to_memory(
                            &mut memory,
                            region_id,
                            to_wasm_prot(prot),
                            reader_slot,
                        )
                        .map_err(|e| {
                            AbiError::new(
                                AbiErrorCode::Internal,
                                format!("attach shared region failed: {e}"),
                            )
                        })?
                } else {
                    // Host-driven path (tests and tooling): map through the
                    // guest's `WasmApplication` in the loaded-guest table.
                    let wasm_region_id = self
                        .kernel
                        .memory()
                        .wasmtiny_region_id(region_id)
                        .map_err(kernel_error)?;
                    let mut guests = self.loaded_guests.lock();
                    let guest = guests.get_mut(&process_id).ok_or_else(|| {
                        AbiError::new(
                            AbiErrorCode::InvalidHandle,
                            "process not found for AttachRegion",
                        )
                    })?;
                    let page_offset = match &mut guest.execution {
                        crate::bootstrap::GuestExecution::Cooperative { app, module_index } => app
                            .attach_shared_region(
                                *module_index,
                                wasm_region_id,
                                to_wasm_prot(prot),
                                reader_slot,
                            )
                            .map_err(|e| {
                                AbiError::new(
                                    AbiErrorCode::Internal,
                                    format!("attach shared region failed: {e}"),
                                )
                            })?,
                        // The AOT instance maps the region into the guest's
                        // shared linear memory; safe while the worker pool
                        // executes (see MultithreadedGuest::attach_shared_region).
                        crate::bootstrap::GuestExecution::Multithreaded(mt) => mt
                            .attach_shared_region(wasm_region_id, to_wasm_prot(prot), reader_slot)
                            .map_err(|e| {
                                AbiError::new(
                                    AbiErrorCode::Internal,
                                    format!("attach shared region failed: {e}"),
                                )
                            })?,
                    };
                    drop(guests);
                    page_offset
                };

                // Shared-page fast-path detection, not configuration. The
                // outbound drain for this region blocks on the engine's
                // unified waiter registry, so a transition kick is only
                // redundant when two things hold:
                //   - the engine advertises its per-region wait registry
                //     (`HostWaitSupport`); and
                //   - the attaching guest's module is fast-path capable — it
                //     declares shared memory AND contains atomic notify
                //     opcodes (probed from the module bytes at spawn; see
                //     `module_probe`), i.e. it was built with the atomics
                //     feature and emits generation-word notifies on its
                //     write path.
                // The pinned wasmtiny always advertises at least
                // `RegistryOnly`, so the guest module probe is the operative
                // signal; the flag keeps the detection honest if an
                // unsupported variant is ever added.
                let engine_supports_registry = {
                    use wasmtiny::runtime::HostWaitSupport;
                    matches!(
                        self.kernel.memory().host_wait_support(),
                        HostWaitSupport::RegistryOnly | HostWaitSupport::RegistryAndOsWake
                    )
                };
                let guest_fastpath_capable =
                    engine_supports_registry && self.process_fastpath_capable(process_id);
                // Every attacher votes; the region's fast path is active
                // only when all of them are capable, so a stable-built guest
                // sharing a region with an atomics guest keeps its kicks.
                self.record_fast_path_attachment(region_id, process_id, guest_fastpath_capable);

                let local_id = self
                    .kernel
                    .memory()
                    .attach_shared_region(region_id)
                    .map_err(kernel_error)?;
                self.claim_local_handle(process_id, ResourceClass::SharedMapping, local_id);
                // Record the attachment so the attacher's parked readers can
                // register generation wakes on this region: it holds a live
                // mapping from here until its cleanup.
                self.record_region_attachment(process_id, region_id);

                let len = self
                    .kernel
                    .memory()
                    .shared_region_len(region_id)
                    .map_err(kernel_error)?;

                Ok(HostOperationState::Ready(HostcallOutput::RegionAttach(
                    selium_abi::RegionAttachment { page_offset, len },
                )))
            }
            HostcallRequest::TcpBind { address } => {
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
                self.claim_local_handle(
                    process_id,
                    ResourceClass::TcpListener,
                    descriptor.local_id,
                );
                // The descriptor doubles as a connection queue: HostQueueRecv
                // checks ownership under the HostQueue class, so claim both.
                self.claim_local_handle(process_id, ResourceClass::HostQueue, descriptor.local_id);
                self.claim_shared_resource(
                    process_id,
                    ResourceClass::TcpListener,
                    descriptor.shared_id,
                );
                Ok(HostOperationState::Ready(HostcallOutput::HostQueue(
                    descriptor,
                )))
            }
            HostcallRequest::TcpConnect { address } => {
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
                self.claim_shared_resource(
                    process_id,
                    ResourceClass::TcpStream,
                    descriptor.shared_id,
                );
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
            HostcallRequest::UdpBind { address } => {
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
                self.claim_shared_resource(
                    process_id,
                    ResourceClass::UdpSocket,
                    descriptor.shared_id,
                );
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
            HostcallRequest::StorageOpenLog { name } => {
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::DurableLog,
                    None,
                )?;
                let descriptor = self.kernel.storage().open_log(&self.kernel.memory(), name);
                self.claim_local_handle(process_id, ResourceClass::DurableLog, descriptor.local_id);
                Ok(HostOperationState::Ready(HostcallOutput::DurableLog(
                    descriptor,
                )))
            }
            HostcallRequest::StorageLogClose { local_id } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::Storage,
                    ResourceClass::DurableLog,
                    local_id,
                )?;
                self.kernel
                    .storage()
                    .close_log(local_id)
                    .map_err(kernel_error)?;
                self.release_local_handle(process_id, &ResourceClass::DurableLog, local_id);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::StorageLogAppend {
                local_id,
                timestamp_ms,
                headers,
                payload,
            } => {
                let shared_id = self.log_shared_id(process_id, local_id)?;
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::DurableLog,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                // Storage allocations count against the writer's durable-log
                // quota: the append's payload bytes are reserved before the
                // record is written.
                let payload_len = payload.len() as u64;
                self.enforce_quota(
                    &self.process_tenant(process_id).unwrap_or_default(),
                    ResourceClass::DurableLog,
                    payload_len,
                )?;
                let sequence = self
                    .kernel
                    .storage()
                    .append_log(local_id, timestamp_ms, headers, payload)
                    .map_err(kernel_error)?;
                self.note_storage_usage(process_id, payload_len);
                Ok(HostOperationState::Ready(HostcallOutput::Sequence(Some(
                    sequence,
                ))))
            }
            HostcallRequest::StorageLogReplay {
                local_id,
                from_sequence,
                limit,
            } => {
                let shared_id = self.log_shared_id(process_id, local_id)?;
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::DurableLog,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                let records = self
                    .kernel
                    .storage()
                    .replay_log(local_id, from_sequence, limit as usize)
                    .map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::StorageRecords(
                    records,
                )))
            }
            HostcallRequest::StorageLogCheckpoint {
                local_id,
                name,
                sequence,
            } => {
                let shared_id = self.log_shared_id(process_id, local_id)?;
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::DurableLog,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                self.kernel
                    .storage()
                    .checkpoint_log(local_id, name, sequence)
                    .map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::StorageLogCheckpointRead { local_id, name } => {
                let shared_id = self.log_shared_id(process_id, local_id)?;
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::DurableLog,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                let sequence = self
                    .kernel
                    .storage()
                    .checkpoint_sequence(local_id, &name)
                    .map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::Sequence(
                    sequence,
                )))
            }
            HostcallRequest::StorageOpenBlobStore { name } => {
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::BlobStore,
                    None,
                )?;
                let descriptor = self
                    .kernel
                    .storage()
                    .open_blob_store(&self.kernel.memory(), name);
                self.claim_local_handle(process_id, ResourceClass::BlobStore, descriptor.local_id);
                Ok(HostOperationState::Ready(HostcallOutput::BlobStore(
                    descriptor,
                )))
            }
            HostcallRequest::StorageBlobStoreClose { local_id } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::Storage,
                    ResourceClass::BlobStore,
                    local_id,
                )?;
                self.kernel
                    .storage()
                    .close_blob_store(local_id)
                    .map_err(kernel_error)?;
                self.release_local_handle(process_id, &ResourceClass::BlobStore, local_id);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::StorageBlobPut { local_id, bytes } => {
                let shared_id = self.blob_store_shared_id(process_id, local_id)?;
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::BlobStore,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                // Storage allocations count against the writer's blob-store
                // quota: the blob's byte length is reserved before it is stored.
                let blob_len = bytes.len() as u64;
                self.enforce_quota(
                    &self.process_tenant(process_id).unwrap_or_default(),
                    ResourceClass::BlobStore,
                    blob_len,
                )?;
                let blob_id = self
                    .kernel
                    .storage()
                    .put_blob(local_id, bytes)
                    .map_err(kernel_error)?;
                self.note_storage_usage(process_id, blob_len);
                Ok(HostOperationState::Ready(HostcallOutput::BlobId(blob_id)))
            }
            HostcallRequest::StorageBlobGet { local_id, blob_id } => {
                let shared_id = self.blob_store_shared_id(process_id, local_id)?;
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::BlobStore,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                match self
                    .kernel
                    .storage()
                    .get_blob(local_id, &blob_id)
                    .map_err(kernel_error)?
                {
                    Some(bytes) => Ok(HostOperationState::Ready(HostcallOutput::Bytes(bytes))),
                    None => Ok(HostOperationState::Ready(HostcallOutput::Empty)),
                }
            }
            HostcallRequest::StorageBlobSetManifest {
                local_id,
                name,
                blob_id,
            } => {
                let shared_id = self.blob_store_shared_id(process_id, local_id)?;
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::BlobStore,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                self.kernel
                    .storage()
                    .set_manifest(local_id, name, blob_id)
                    .map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::StorageBlobGetManifest { local_id, name } => {
                let shared_id = self.blob_store_shared_id(process_id, local_id)?;
                self.require(
                    process_id,
                    Capability::Storage,
                    ResourceClass::BlobStore,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                match self
                    .kernel
                    .storage()
                    .get_manifest(local_id, &name)
                    .map_err(kernel_error)?
                {
                    Some(blob_id) => Ok(HostOperationState::Ready(HostcallOutput::BlobId(blob_id))),
                    None => Ok(HostOperationState::Ready(HostcallOutput::Empty)),
                }
            }
            HostcallRequest::ProcessStart {
                module_id,
                entrypoint,
                arguments,
                grants,
                tenant,
            } => {
                self.require(
                    process_id,
                    Capability::ProcessLifecycle,
                    ResourceClass::Process,
                    None,
                )?;
                let tenant = self.resolve_spawn_tenant(process_id, tenant.as_deref())?;
                self.validate_child_grants(process_id, &grants)?;
                let module_bytes = self
                    .module_bytes(&module_id)
                    .map_err(|error| AbiError::new(AbiErrorCode::NotFound, error.to_string()))?;
                let arguments =
                    crate::wasm::decode_integer_arguments(&arguments).map_err(|error| {
                        AbiError::new(AbiErrorCode::MalformedPayload, error.to_string())
                    })?;
                // Enforce the tenant-scoped process quota after spawn-tenant
                // resolution and before instantiation: a spawn whose target
                // tenant is at its authored ceiling is denied with
                // `QuotaExceeded`. Tenant-less (root) processes are not
                // metered.
                let quota_tenant = tenant.clone();
                if let Some(tenant) = quota_tenant.as_deref() {
                    self.enforce_quota(tenant, ResourceClass::Process, 1)?;
                }
                let descriptor = SystemGuestDescriptor {
                    name: module_id.clone(),
                    module_id: module_id.clone(),
                    module_bytes,
                    entrypoint,
                    arguments,
                    grants,
                    dependencies: Vec::new(),
                    readiness: ReadinessCondition::Immediate,
                    tenant,
                    serving_role: None,
                    handlers: Vec::new(),
                };
                let child = match self.spawn_system_guest(descriptor) {
                    Ok(child) => child,
                    Err(error) => {
                        // The reserved slot was never claimed by a live child;
                        // return it so a failed spawn does not leak quota.
                        if let Some(tenant) = quota_tenant.as_deref() {
                            self.kernel
                                .quota()
                                .release(tenant, ResourceClass::Process, 1);
                        }
                        return Err(AbiError::new(AbiErrorCode::Internal, error.to_string()));
                    }
                };
                // Record the charged tenant so teardown releases the slot once.
                if let Some(tenant) = quota_tenant.as_deref() {
                    self.process_quota_tenants
                        .lock()
                        .insert(child.process_id, tenant.to_string());
                }
                // Record the parent relationship for the Children selector.
                self.process_authorities
                    .lock()
                    .entry(child.process_id)
                    .and_modify(|auth| auth.parent = Some(process_id));
                self.claim_local_handle(process_id, ResourceClass::Process, child.process_id);
                let process = self
                    .kernel
                    .processes()
                    .inspect_process(child.process_id)
                    .map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::Process(process)))
            }
            HostcallRequest::ProcessStop {
                process_id: target_process_id,
            } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::ProcessLifecycle,
                    ResourceClass::Process,
                    target_process_id,
                )?;
                self.require(
                    process_id,
                    Capability::ProcessLifecycle,
                    ResourceClass::Process,
                    Some(ResourceIdentity::Local(target_process_id)),
                )?;
                self.stop_process(target_process_id)
                    .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::ActivityRead { cursor } => {
                self.require(
                    process_id,
                    Capability::ActivityRead,
                    ResourceClass::ActivityLog,
                    None,
                )?;
                Ok(HostOperationState::Ready(HostcallOutput::ActivityEvents(
                    self.kernel.processes().read_activity_from(cursor),
                )))
            }
            HostcallRequest::MeteringRead {
                process_id: target_process_id,
            } => {
                self.require(
                    process_id,
                    Capability::MeteringRead,
                    ResourceClass::MeteringStream,
                    Some(ResourceIdentity::Local(target_process_id)),
                )?;
                match self
                    .kernel
                    .processes()
                    .metering_observation(target_process_id)
                {
                    Some(observation) => Ok(HostOperationState::Ready(HostcallOutput::Metering(
                        observation,
                    ))),
                    None => Ok(HostOperationState::Ready(HostcallOutput::Empty)),
                }
            }
            HostcallRequest::QuotaSet {
                tenant,
                class,
                limit,
            } => {
                // Quota authorship is bootstrap-provisioned: only the
                // accounting guest holds `QuotaWrite`, and the admission matrix
                // keeps it non-conferable.
                self.require_capability(process_id, Capability::QuotaWrite)?;
                self.kernel.quota().set(tenant, class, limit);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::QuotaClear { tenant, class } => {
                self.require_capability(process_id, Capability::QuotaWrite)?;
                self.kernel.quota().clear(&tenant, class);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::GuestLogWrite { entry } => {
                // GuestLogWrite treats the writer's own pid as owned: a
                // process can always write a log entry for itself.
                if entry.process_id == Some(process_id) {
                    self.require(
                        process_id,
                        Capability::GuestLogWrite,
                        ResourceClass::GuestLog,
                        None,
                    )?;
                } else {
                    self.authorise_guest_log_process(
                        process_id,
                        Capability::GuestLogWrite,
                        &entry,
                    )?;
                    self.require(
                        process_id,
                        Capability::GuestLogWrite,
                        ResourceClass::GuestLog,
                        entry.process_id.map(ResourceIdentity::Local),
                    )?;
                }
                self.kernel.processes().write_guest_log(entry);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::HostQueueCreate { serving_tenant } => {
                self.require(
                    process_id,
                    Capability::HostQueue,
                    ResourceClass::HostQueue,
                    None,
                )?;
                // Principal provenance, mirroring AllocRegion: the queue is
                // minted under the serving tenant, which may differ from the
                // allocating process's own tenant only for a root principal or
                // with tenant-scoped delegation.
                let principal =
                    self.authorize_serving_tenant(process_id, serving_tenant.as_deref())?;
                // Pipe quotas meter queued *items*, not queue count: a queue
                // costs nothing to create and each pending entry reserves one
                // slot against the queue owner's (serving) tenant — released
                // when the entry is received. See `charge_queue_item`.
                let queues = self.kernel.queues();
                let memory = self.kernel.memory();
                let descriptor = queues.create_host_queue(&memory);
                self.claim_local_handle(process_id, ResourceClass::HostQueue, descriptor.local_id);
                self.claim_shared_resource(
                    process_id,
                    ResourceClass::HostQueue,
                    descriptor.shared_id,
                );
                // Tier-1 registration: queues are first-class resources so a
                // guest can register an external route whose target is its
                // listener queue and still pass discovery's ownership
                // validation. Registered under the principal tenant and
                // revoked on process teardown.
                let uri =
                    crate::discovery::queue_registration_uri(&principal, descriptor.shared_id);
                let target = ResourceTarget {
                    uri: uri.clone(),
                    host_id: String::new(), // Runtime doesn't know host_id; discovery will fill it.
                    resource_id: descriptor.shared_id,
                    interface: None,
                    tenant: (!principal.is_empty()).then_some(principal.clone()),
                    class: ResourceClass::HostQueue,
                    labels: Vec::new(),
                };
                let request = DiscoveryRequest::Register {
                    uri,
                    target,
                    owner: Some(process_id),
                    root_service: false,
                };
                if let Err(error) = self.publish_discovery_event(request) {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        format!("discovery publish failed: {error}"),
                    ));
                }
                // Remember the principal tenant so teardown revokes the same URI.
                self.queue_tenants
                    .lock()
                    .insert((process_id, descriptor.shared_id), principal);
                Ok(HostOperationState::Ready(HostcallOutput::HostQueue(
                    descriptor,
                )))
            }
            HostcallRequest::HostQueueAttach { shared_id } => {
                // Check: owner? → allow; has ExplicitResource grant? → allow;
                // resolved via discovery? → allow; else deny.
                let is_owner = self
                    .shared_resource_owners
                    .lock()
                    .get(&(ResourceClass::HostQueue, shared_id))
                    .is_some_and(|owners| owners.contains(&process_id));
                let has_explicit_grant = self
                    .process_authorities
                    .lock()
                    .get(&process_id)
                    .is_some_and(|auth| {
                        auth.grants.iter().any(|grant| {
                            grant.capability == Capability::HostQueue
                                && grant.selectors.iter().any(|sel| {
                                    sel == &ResourceSelector::ExplicitResource(
                                        ResourceIdentity::Shared(shared_id),
                                    )
                                })
                        })
                    });
                let is_resolved = self
                    .process_authorities
                    .lock()
                    .get(&process_id)
                    .is_some_and(|auth| auth.resolved_queue_ids.contains(&shared_id));

                if !is_owner && !has_explicit_grant && !is_resolved {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "HostQueueAttach denied for shared_id {shared_id}: process {process_id} is not owner, lacks ExplicitResource grant, and did not resolve via discovery",
                        ),
                    ));
                }

                let descriptor = self
                    .kernel
                    .queues()
                    .attach_host_queue(&self.kernel.memory(), shared_id)
                    .map_err(kernel_error)?;
                self.claim_local_handle(process_id, ResourceClass::HostQueue, descriptor.local_id);
                Ok(HostOperationState::Ready(HostcallOutput::HostQueue(
                    descriptor,
                )))
            }
            HostcallRequest::RandomBytes { len } => {
                const MAX_RANDOM_BYTES: u32 = 4096;
                if len > MAX_RANDOM_BYTES {
                    return Err(AbiError::new(
                        AbiErrorCode::MalformedPayload,
                        format!("RandomBytes length {len} exceeds maximum {MAX_RANDOM_BYTES}"),
                    ));
                }
                let len = len as usize;
                let mut buf = vec![0u8; len];
                let mut file = std::fs::File::open("/dev/urandom").map_err(|error| {
                    AbiError::new(
                        AbiErrorCode::Internal,
                        format!("failed to open /dev/urandom: {error}"),
                    )
                })?;
                file.read_exact(&mut buf).map_err(|error| {
                    AbiError::new(
                        AbiErrorCode::Internal,
                        format!("failed to read /dev/urandom: {error}"),
                    )
                })?;
                Ok(HostOperationState::Ready(HostcallOutput::RandomBytes(buf)))
            }
            HostcallRequest::RecordResolvedQueueFor {
                client_process_id,
                shared_id,
            } => {
                // Only the discovery system guest may report resolve results.
                // A guest reporting its own resolves would be able to
                // self-authorize cross-process queue attach.
                let discovery = *self.discovery_process.lock();
                if discovery != Some(process_id) {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "RecordResolvedQueueFor denied for process {process_id}: only the discovery service may record resolved queues",
                        ),
                    ));
                }
                let mut authorities = self.process_authorities.lock();
                if let Some(auth) = authorities.get_mut(&client_process_id) {
                    auth.resolved_queue_ids.insert(shared_id);
                }
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::RecordResolvedRegionFor {
                client_process_id,
                shared_id,
            } => {
                // Only the discovery system guest may report a region resolve:
                // a guest self-approving its own region attach would defeat
                // `AttachRegion` authorisation.
                let discovery = *self.discovery_process.lock();
                if discovery != Some(process_id) {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "RecordResolvedRegionFor denied for process {process_id}: only the discovery service may record resolved regions",
                        ),
                    ));
                }
                let mut authorities = self.process_authorities.lock();
                if let Some(auth) = authorities.get_mut(&client_process_id) {
                    auth.resolved_region_ids.insert(shared_id);
                }
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::SignTenantCa { tenant } => {
                self.require_capability(process_id, Capability::MintCertificate)?;
                let mut keyring = self.keyring.lock();
                let keyring = keyring.as_mut().ok_or_else(|| {
                    AbiError::new(
                        AbiErrorCode::Internal,
                        "certificate signing keyring is not initialized",
                    )
                })?;
                let cert_der = keyring
                    .sign_tenant_ca(&tenant)
                    .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
                Ok(HostOperationState::Ready(HostcallOutput::Certificate(
                    cert_der,
                )))
            }
            HostcallRequest::SignUserCert { tenant, spki_der } => {
                self.require_capability(process_id, Capability::MintCertificate)?;
                let keyring = self.keyring.lock();
                let keyring = keyring.as_ref().ok_or_else(|| {
                    AbiError::new(
                        AbiErrorCode::Internal,
                        "certificate signing keyring is not initialized",
                    )
                })?;
                let cert_der = keyring
                    .sign_user_cert(&tenant, &spki_der)
                    .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
                Ok(HostOperationState::Ready(HostcallOutput::Certificate(
                    cert_der,
                )))
            }
            HostcallRequest::RevokeCa { tenant } => {
                self.require_capability(process_id, Capability::MintCertificate)?;
                let mut keyring = self.keyring.lock();
                let keyring = keyring.as_mut().ok_or_else(|| {
                    AbiError::new(
                        AbiErrorCode::Internal,
                        "certificate signing keyring is not initialized",
                    )
                })?;
                keyring
                    .revoke_ca(&tenant)
                    .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::RecordRegistration {
                process_id: registered_process,
                uri,
            } => {
                // Only the discovery system guest may report registrations;
                // the record gates role-declared readiness.
                let discovery = *self.discovery_process.lock();
                if discovery != Some(process_id) {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "RecordRegistration denied for process {process_id}: only the discovery service may record registrations",
                        ),
                    ));
                }
                self.record_registration(registered_process, uri);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::SelfInfo => Ok(HostOperationState::Ready(HostcallOutput::SelfInfo {
                process_id,
                tenant: self.process_tenant(process_id),
            })),
            HostcallRequest::ProcessTenant { process_id } => Ok(HostOperationState::Ready(
                HostcallOutput::Tenant(self.process_tenant(process_id)),
            )),
            HostcallRequest::ProcessCapability {
                process_id: target_process_id,
                capability,
            } => {
                // Only the discovery system guest may probe another process's
                // grants; it uses the check to gate root-registration requests.
                let discovery = *self.discovery_process.lock();
                if discovery != Some(process_id) {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "ProcessCapability denied for process {process_id}: only the discovery service may probe process capabilities",
                        ),
                    ));
                }
                let held = self.authorises(target_process_id, capability, &ScopeContext::default());
                Ok(HostOperationState::Ready(HostcallOutput::U64(u64::from(
                    held,
                ))))
            }
            HostcallRequest::ResolveProtocolHandler { scheme } => {
                // Handler registrations are Tier-1 (bootstrap-published), so
                // this lookup cannot be forged by guests. Serve-side guests
                // use it to pin the process legitimately delivering handoffs.
                let handler = self
                    .handler_schemes
                    .lock()
                    .iter()
                    .find(|(_, schemes)| schemes.contains(&scheme))
                    .map(|(pid, _)| *pid);
                match handler {
                    Some(pid) => Ok(HostOperationState::Ready(HostcallOutput::U64(pid))),
                    None => Ok(HostOperationState::Ready(HostcallOutput::Empty)),
                }
            }
            HostcallRequest::HostQueueSend {
                local_id,
                value,
                metadata,
            } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::HostQueue,
                    ResourceClass::HostQueue,
                    local_id,
                )?;
                if metadata.len() > selium_abi::METADATA_MAX_BYTES {
                    return Err(AbiError::new(
                        AbiErrorCode::MalformedPayload,
                        format!(
                            "handoff metadata length {} exceeds maximum {}",
                            metadata.len(),
                            selium_abi::METADATA_MAX_BYTES
                        ),
                    ));
                }
                let shared_id = self
                    .kernel
                    .queues()
                    .host_queue_shared_id(local_id)
                    .map_err(kernel_error)?;
                self.require(
                    process_id,
                    Capability::HostQueue,
                    ResourceClass::HostQueue,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                // A queued item reserves one pipe slot against the queue
                // owner's tenant; it is released when the item is received.
                self.charge_queue_item(shared_id)?;
                self.kernel
                    .queues()
                    .host_queue_send(local_id, process_id, value, metadata)
                    .map_err(kernel_error)?;
                self.wake_host_queue_waiters(shared_id);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::HostQueueRecv { local_id } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::HostQueue,
                    ResourceClass::HostQueue,
                    local_id,
                )?;
                let shared_id = self
                    .kernel
                    .queues()
                    .host_queue_shared_id(local_id)
                    .map_err(kernel_error)?;
                self.require(
                    process_id,
                    Capability::HostQueue,
                    ResourceClass::HostQueue,
                    Some(ResourceIdentity::Shared(shared_id)),
                )?;
                match self
                    .kernel
                    .queues()
                    .try_host_queue_recv(local_id)
                    .map_err(kernel_error)?
                {
                    Some((client_process_id, value, metadata)) => {
                        // The item left the queue and entered the receiving
                        // process's resource table: release its pipe slot.
                        self.release_queue_item(shared_id);
                        // Queue handoff: if the value matches a shared region
                        // owned by the sender, ownership TRANSFERS to the
                        // receiver (documented rendezvous pattern — the only
                        // place ownership moves implicitly, kernel-side),
                        // with the region's quota reservation following it.
                        self.transfer_region_ownership_on_recv(
                            process_id,
                            client_process_id,
                            value,
                        );
                        Ok(HostOperationState::Ready(HostcallOutput::ConnectionInfo {
                            client_process_id,
                            value,
                            metadata,
                        }))
                    }
                    None => Ok(HostOperationState::HostQueueRecvWait {
                        local_id,
                        deadline: Instant::now() + Duration::from_secs(30),
                    }),
                }
            }
            HostcallRequest::GuestLogRead {
                cursor,
                process_id: target_process_id,
            } => {
                if let Some(target_process_id) = target_process_id {
                    self.ensure_local_handle_owner(
                        process_id,
                        Capability::GuestLogRead,
                        ResourceClass::Process,
                        target_process_id,
                    )?;
                }
                self.require(
                    process_id,
                    Capability::GuestLogRead,
                    ResourceClass::GuestLog,
                    target_process_id.map(ResourceIdentity::Local),
                )?;

                // Read from the legacy guest_logs vec (existing path).
                let mut logs: Vec<GuestLogEntry> = self
                    .kernel
                    .processes()
                    .read_guest_logs_from(cursor)
                    .into_iter()
                    .filter(|entry| {
                        target_process_id.is_none() || entry.process_id == target_process_id
                    })
                    .collect();

                // Also drain from log channels if target process has one.
                if let Some(target_pid) = target_process_id
                    && let Ok(frames) = self.kernel.processes().drain_log_channel(target_pid)
                {
                    for frame in frames {
                        // Decode FlatBuffer LogRecord into GuestLogEntry.
                        if let Ok(record) = LogRecord::decode(&frame) {
                            logs.push(GuestLogEntry {
                                process_id: Some(target_pid),
                                level: format!("{:?}", record.level),
                                target: record.target,
                                message: record.message,
                            });
                        }
                    }
                }

                Ok(HostOperationState::Ready(HostcallOutput::GuestLogEntries(
                    logs,
                )))
            }
            HostcallRequest::TimeNow => {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
                    .as_nanos() as u64;
                Ok(HostOperationState::Ready(HostcallOutput::U64(nanos)))
            }
            HostcallRequest::TimeMonotonic => {
                static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
                let nanos = EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64;
                Ok(HostOperationState::Ready(HostcallOutput::U64(nanos)))
            }
            HostcallRequest::Sleep { millis } => {
                let deadline = Instant::now() + Duration::from_millis(millis);
                Ok(HostOperationState::SleepWait { deadline })
            }
            HostcallRequest::GuestLogRegister { shared_id } => {
                // Validate that shared_id belongs to the calling process.
                let owns = self
                    .shared_resource_owners
                    .lock()
                    .get(&(ResourceClass::SharedRegion, shared_id))
                    .is_some_and(|owners| owners.contains(&process_id));

                if !owns {
                    return Err(AbiError::new(
                        AbiErrorCode::DetachedResource,
                        format!(
                            "GuestLogRegister: shared_id {shared_id} not owned by process {process_id}"
                        ),
                    ));
                }

                // Register the log channel with the kernel. The kernel stores
                // the shared_id per process; actual channel reading is done
                // via the kernel's shared memory primitives.
                self.kernel
                    .processes()
                    .register_log_channel(&self.kernel.memory(), process_id, shared_id)
                    .map_err(kernel_error)?;

                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::WaitRegister {
                region_id,
                generation: _,
            } => {
                // Reject if the process neither owns nor attached this
                // region: wake registration requires a live mapping.
                if !self.owns_or_attached_region(process_id, region_id) {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "WaitRegister denied: process {process_id} has neither attached nor owns region {region_id}"
                        ),
                    ));
                }

                // Client must supply a task_id in the envelope so the host
                // can route the wake correctly. The hostcall itself returns
                // Ready immediately — the wake comes later via mailbox.
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::GenerationAdvance {
                region_id,
                generation,
            } => {
                // Mirrors WaitRegister authorisation: only a process that
                // owns or attached the region may advance (and thereby
                // wake) it.
                if !self.owns_or_attached_region(process_id, region_id) {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "GenerationAdvance denied: process {process_id} has neither attached nor owns region {region_id}"
                        ),
                    ));
                }

                self.note_generation_advance(region_id, generation);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
        }
    }

    fn wake_host_queue_waiters(&self, shared_id: u64) {
        let mut wakeups = Vec::new();
        {
            let mut operations = self.operations.lock();
            for operation in operations.values_mut() {
                let should_wake = matches!(
                    &operation.state,
                    HostOperationState::HostQueueRecvWait {
                        local_id,
                        ..
                    } if self.kernel.queues().host_queue_shared_id(*local_id).ok() == Some(shared_id)
                );
                if should_wake {
                    let local_id = match &operation.state {
                        HostOperationState::HostQueueRecvWait { local_id, .. } => *local_id,
                        _ => continue,
                    };
                    if let Ok(Some((client_process_id, value, metadata))) =
                        self.kernel.queues().try_host_queue_recv(local_id)
                    {
                        // The item left the queue: release its pipe slot.
                        self.release_queue_item(shared_id);
                        // Queue handoff: mirror the ownership transfer performed
                        // by the `poll_hostcall` completion path. Without this,
                        // a receiver woken via `HostQueueSend` gets the value
                        // but no authorisation basis to attach the handed-off
                        // region (documented rendezvous pattern).
                        self.transfer_region_ownership_on_recv(
                            operation.process_id,
                            client_process_id,
                            value,
                        );
                        operation.state =
                            HostOperationState::Ready(HostcallOutput::ConnectionInfo {
                                client_process_id,
                                value,
                                metadata,
                            });
                        if let Some(task_id) = operation.task_id {
                            wakeups.push((operation.process_id, task_id));
                        }
                    }
                }
            }
        }
        for (process_id, task_id) in wakeups {
            self.wake_process_task(process_id, task_id);
        }
    }

    #[expect(
        clippy::panic,
        reason = "running out of operation ids cripples the system"
    )]
    pub(crate) fn next_operation_id(
        &self,
        operations: &HashMap<OperationId, HostOperation>,
    ) -> OperationId {
        let mut next_operation_id = self.next_operation_id.lock();
        let first_candidate = *next_operation_id;
        loop {
            let operation_id = *next_operation_id;
            *next_operation_id = operation_id.checked_add(1).unwrap_or(1);
            if operation_id != 0 && !operations.contains_key(&operation_id) {
                return operation_id;
            }
            if *next_operation_id == first_candidate {
                panic!("operation id space exhausted");
            }
        }
    }

    /// Reserves `amount` of the tenant's quota ceiling for `class`, denying
    /// the allocation (before any resource is granted) when it would exceed
    /// the authored ceiling. The platform tenant (empty name) is unrestricted.
    fn enforce_quota(
        &self,
        tenant: &str,
        class: ResourceClass,
        amount: u64,
    ) -> std::result::Result<(), AbiError> {
        self.kernel
            .quota()
            .try_consume(tenant, class, amount)
            .map_err(|error| AbiError::new(AbiErrorCode::QuotaExceeded, error.to_string()))
    }

    /// Returns the tenant whose pipe quota meters a queue: the serving
    /// principal the queue was minted for (its owner). Items pending in a
    /// tenant's queues count against that tenant, whoever sent them.
    fn queue_owner_tenant(&self, shared_id: u64) -> Option<String> {
        self.queue_tenants
            .lock()
            .iter()
            .find(|((_, queue_shared_id), _)| *queue_shared_id == shared_id)
            .map(|(_, principal)| principal.clone())
    }

    /// Reserves one pipe slot against the queue owner's (serving) tenant for
    /// an item enqueued onto `shared_id`, denying the enqueue when the
    /// tenant's ceiling is exhausted.
    pub(crate) fn charge_queue_item(&self, shared_id: u64) -> std::result::Result<(), AbiError> {
        if let Some(tenant) = self.queue_owner_tenant(shared_id) {
            self.enforce_quota(&tenant, ResourceClass::HostQueue, 1)?;
        }
        Ok(())
    }

    /// Releases the pipe slot reserved for a delivered (dequeued) item: the
    /// item has left the queue and entered the receiving process's resource
    /// table.
    pub(crate) fn release_queue_item(&self, shared_id: u64) {
        if let Some(tenant) = self.queue_owner_tenant(shared_id) {
            self.kernel
                .quota()
                .release(&tenant, ResourceClass::HostQueue, 1);
        }
    }

    fn ensure_shared_resource_owner(
        &self,
        process_id: ProcessId,
        capability: Capability,
        resource_class: ResourceClass,
        shared_id: u64,
    ) -> std::result::Result<(), AbiError> {
        if self
            .shared_resource_owners
            .lock()
            .get(&(resource_class, shared_id))
            .is_some_and(|owners| owners.contains(&process_id))
        {
            Ok(())
        } else {
            Err(AbiError::new(
                AbiErrorCode::PermissionDenied,
                format!("permission denied for capability {capability:?}"),
            ))
        }
    }

    /// Checks that `process_id` may attach to `region_id`: either it owns the
    /// shared region or it holds an `ExplicitResource(Shared(region_id))` grant.
    fn ensure_attach_authorised(
        &self,
        process_id: ProcessId,
        region_id: u64,
    ) -> std::result::Result<(), AbiError> {
        // Ownership check.
        let owns = self
            .shared_resource_owners
            .lock()
            .get(&(ResourceClass::SharedRegion, region_id))
            .is_some_and(|owners| owners.contains(&process_id));
        if owns {
            return Ok(());
        }

        // ExplicitResource grant check.
        let has_explicit = self
            .process_authorities
            .lock()
            .get(&process_id)
            .map(|auth| {
                auth.grants.iter().any(|grant| {
                    grant.capability == Capability::SharedMemory
                        && grant.selectors.iter().any(|sel| {
                            matches!(
                                sel,
                                ResourceSelector::ExplicitResource(
                                    ResourceIdentity::Shared(id)
                                ) if *id == region_id
                            )
                        })
                })
            })
            .unwrap_or(false);
        if has_explicit {
            return Ok(());
        }

        // Discovery resolve basis: a process that resolved this region via
        // discovery gained an authorisation basis for attach (mirrors the
        // queue resolve basis used by `HostQueueAttach`).
        let was_resolved = self
            .process_authorities
            .lock()
            .get(&process_id)
            .is_some_and(|auth| auth.resolved_region_ids.contains(&region_id));
        if was_resolved {
            return Ok(());
        }

        Err(AbiError::new(
            AbiErrorCode::PermissionDenied,
            format!(
                "AttachRegion denied: process {process_id} does not own region {region_id} and has no ExplicitResource grant",
            ),
        ))
    }

    /// Queue handoff ownership sharing: if `value` matches a shared region
    /// owned by `sender_pid`, share ownership with `receiver_pid`. This is the
    /// one place ownership is granted implicitly (kernel-side, documented).
    /// Transfers ownership of a handed-off region from sender to receiver.
    ///
    /// A delivered handoff leaves the sender's resource table and enters the
    /// receiver's (Option A transfer semantics — the rendezvous pattern is
    /// the only place ownership moves implicitly, kernel-side). The region's
    /// quota reservation follows the resource: released from the sender's
    /// (recorded serving) tenant and force-consumed against the receiver's.
    ///
    /// The receiver's consumption is **force-accepted**, not denied: a peer
    /// can hand over a "poisoned" resource that pushes the receiving tenant
    /// over its ceiling (subsequent allocations are denied; metering
    /// surfaces the anomaly). Denying the receive instead would require
    /// peek/requeue machinery in the host queue and would clog the victim's
    /// queue slot permanently — a strictly worse denial than the documented
    /// ceiling overflow. See the accountant spec's open-attack-vector note.
    ///
    /// Cross-tenant handoffs also move the discovery revocation bookkeeping
    /// (`region_tenants`) to the receiver under the receiver's tenant; the
    /// tier-1 registration URI remains minted under the original tenant, so
    /// a cross-tenant revocation may miss (single-tenant handoffs — all
    /// current flows — are unaffected).
    fn transfer_region_ownership_on_recv(
        &self,
        receiver_pid: ProcessId,
        sender_pid: ProcessId,
        value: u64,
    ) {
        let region_id = value;
        let sender_owns = self
            .shared_resource_owners
            .lock()
            .get(&(ResourceClass::SharedRegion, region_id))
            .is_some_and(|owners| owners.contains(&sender_pid));
        if !sender_owns {
            return;
        }

        // Move the quota reservation: release from the recorded serving
        // tenant, force-consume against the receiver's tenant (only when the
        // tenants differ — an intra-tenant handoff keeps the reservation put).
        let mut region_tenants = self.region_tenants.lock();
        let key = (sender_pid, region_id);
        if let Some(serving_tenant) = region_tenants.get(&key).cloned() {
            let receiver_tenant = self.process_tenant(receiver_pid).unwrap_or_default();
            if serving_tenant != receiver_tenant
                && let Ok(len) = self.kernel.memory().shared_region_len(region_id)
            {
                self.kernel.quota().release(
                    &serving_tenant,
                    ResourceClass::SharedRegion,
                    u64::from(len),
                );
                self.kernel.quota().force_consume(
                    &receiver_tenant,
                    ResourceClass::SharedRegion,
                    u64::from(len),
                );
            }
            // Re-key the revocation bookkeeping to the receiver under its
            // tenant (single-tenant handoffs keep the same tenant value).
            region_tenants.remove(&key);
            region_tenants.insert(
                (receiver_pid, region_id),
                self.process_tenant(receiver_pid).unwrap_or_default(),
            );
        }
        drop(region_tenants);

        // Transfer ownership: the sender's entry leaves its resource table,
        // the receiver's gains it.
        let mut shared_resource_owners = self.shared_resource_owners.lock();
        if let Some(owners) =
            shared_resource_owners.get_mut(&(ResourceClass::SharedRegion, region_id))
        {
            owners.remove(&sender_pid);
            owners.insert(receiver_pid);
        }
    }

    fn log_shared_id(
        &self,
        process_id: ProcessId,
        local_id: u64,
    ) -> std::result::Result<u64, AbiError> {
        self.ensure_local_handle_owner(
            process_id,
            Capability::Storage,
            ResourceClass::DurableLog,
            local_id,
        )?;
        self.kernel
            .storage()
            .log_shared_id_public(local_id)
            .map_err(kernel_error)
    }

    fn blob_store_shared_id(
        &self,
        process_id: ProcessId,
        local_id: u64,
    ) -> std::result::Result<u64, AbiError> {
        self.ensure_local_handle_owner(
            process_id,
            Capability::Storage,
            ResourceClass::BlobStore,
            local_id,
        )?;
        self.kernel
            .storage()
            .blob_store_shared_id_public(local_id)
            .map_err(kernel_error)
    }

    fn validate_child_grants(
        &self,
        process_id: ProcessId,
        grants: &[CapabilityGrant],
    ) -> std::result::Result<(), AbiError> {
        // Well-formedness admission always runs, even under delegation.
        self.validate_grants(grants)
            .map_err(|error| AbiError::new(AbiErrorCode::MalformedPayload, error.to_string()))?;
        let authority = self.restore_process_authority(process_id).ok_or_else(|| {
            AbiError::new(
                AbiErrorCode::InvalidHandle,
                format!("unknown process authority {process_id}"),
            )
        })?;

        // `DelegateGrants` is bootstrap-provisioned only: it can never be
        // conferred on a child process, not even by a parent that holds it.
        // Without this, a delegator could spawn further delegators and chain
        // the exception to authority monotonicity arbitrarily.
        if grants
            .iter()
            .any(|grant| grant.capability == Capability::DelegateGrants)
        {
            return Err(AbiError::new(
                AbiErrorCode::PermissionDenied,
                "DelegateGrants cannot be delegated to child processes",
            ));
        }

        // `MintCertificate` mirrors `DelegateGrants`: it is bootstrap-
        // provisioned only. A spawn that confers it on a child is denied even
        // when the parent itself holds the capability, so mint authority can
        // never be reproduced outside the identity guest.
        if grants
            .iter()
            .any(|grant| grant.capability == Capability::MintCertificate)
        {
            return Err(AbiError::new(
                AbiErrorCode::PermissionDenied,
                "MintCertificate cannot be delegated to child processes",
            ));
        }

        // `QuotaWrite` mirrors `MintCertificate` and `DelegateGrants`: it is
        // bootstrap-provisioned only. A spawn that confers it on a child is
        // denied even when the parent itself holds the capability, so quota
        // authorship stays with the accounting guest.
        if grants
            .iter()
            .any(|grant| grant.capability == Capability::QuotaWrite)
        {
            return Err(AbiError::new(
                AbiErrorCode::PermissionDenied,
                "QuotaWrite cannot be delegated to child processes",
            ));
        }

        // Tenant-scoped grant delegation: a parent holding a
        // `DelegateGrants` grant with a tenant or root namespace selector may
        // confer child grants it does not itself hold, provided **every**
        // child grant is itself tenant-scoped within the parent's delegation
        // scope. A grant without an in-scope `Tenant` selector is unrestricted
        // within its capability and must therefore fall through to the subset
        // check: admitting it under delegation would escape the tenant fence.
        // This is the deliberate, tenant-fenced exception to authority
        // monotonicity (see the `rebuild-guest-bridge` design, D7).
        let delegating = match delegation_scope(&authority.grants) {
            Some(DelegationScope::Root) => grants.iter().all(|grant| {
                grant.selectors.iter().any(|selector| {
                    matches!(
                        selector,
                        ResourceSelector::Tenant(_)
                            | ResourceSelector::Namespace(Namespace::Tenant(_))
                    )
                })
            }),
            Some(DelegationScope::Tenants(tenants)) => grants.iter().all(|grant| {
                grant.selectors.iter().any(|selector| match selector {
                    ResourceSelector::Tenant(t) => tenants.contains(t),
                    ResourceSelector::Namespace(Namespace::Tenant(t)) => tenants.contains(t),
                    _ => false,
                })
            }),
            None => false,
        };

        if delegating {
            return Ok(());
        }

        let parent_grants = authority.grants;
        for grant in grants {
            if !parent_grants
                .iter()
                .any(|parent| parent_grant_covers_child(parent, grant))
            {
                return Err(AbiError::new(
                    AbiErrorCode::PermissionDenied,
                    format!(
                        "child grant exceeds parent authority for {:?}",
                        grant.capability
                    ),
                ));
            }
        }
        Ok(())
    }

    fn authorise_guest_log_process(
        &self,
        process_id: ProcessId,
        capability: Capability,
        entry: &GuestLogEntry,
    ) -> std::result::Result<(), AbiError> {
        if let Some(entry_process_id) = entry.process_id {
            self.ensure_local_handle_owner(
                process_id,
                capability,
                ResourceClass::Process,
                entry_process_id,
            )?;
        }
        Ok(())
    }

    /// Resolves the tenant a spawned child runs under.
    ///
    /// `None` (or a value equal to the parent's own tenant) inherits the
    /// parent's tenant with no extra authority. Spawning a child under any
    /// other tenant — for a root parent as much as a tenant-scoped one —
    /// requires a `DelegateGrants` grant whose scope (root or tenant-scoped)
    /// admits the requested tenant: cross-tenant authority is a grant, not an
    /// accident of the parent's bootstrap tenant.
    fn resolve_spawn_tenant(
        &self,
        process_id: ProcessId,
        requested: Option<&str>,
    ) -> std::result::Result<Option<String>, AbiError> {
        let own_tenant = self.process_tenant(process_id);
        let Some(requested) = requested else {
            return Ok(own_tenant);
        };
        if own_tenant.as_deref() == Some(requested) {
            return Ok(Some(requested.to_string()));
        }
        let authority = self.restore_process_authority(process_id).ok_or_else(|| {
            AbiError::new(
                AbiErrorCode::InvalidHandle,
                format!("unknown process authority {process_id}"),
            )
        })?;
        if delegation_scope(&authority.grants).is_some_and(|scope| scope.admits_tenant(requested)) {
            return Ok(Some(requested.to_string()));
        }
        Err(AbiError::new(
            AbiErrorCode::PermissionDenied,
            format!("spawn tenant {requested:?} requires an in-scope DelegateGrants grant"),
        ))
    }
}

/// Extracts the delegation scope from an authority's grants: `Root` when any
/// `DelegateGrants` grant carries a `Namespace::Root` selector, the tenant set
/// from `Tenant`/`Namespace::Tenant` selectors, and `None` when no
/// `DelegateGrants` grant carries a tenant-scoping selector.
fn delegation_scope(grants: &[CapabilityGrant]) -> Option<DelegationScope> {
    let mut tenants = HashSet::new();
    let mut root = false;
    for grant in grants
        .iter()
        .filter(|grant| grant.capability == Capability::DelegateGrants)
    {
        for selector in &grant.selectors {
            match selector {
                ResourceSelector::Tenant(tenant) => {
                    tenants.insert(tenant.clone());
                }
                ResourceSelector::Namespace(Namespace::Tenant(tenant)) => {
                    tenants.insert(tenant.clone());
                }
                ResourceSelector::Namespace(Namespace::Root) => {
                    root = true;
                }
                _ => {}
            }
        }
    }
    if root {
        Some(DelegationScope::Root)
    } else if tenants.is_empty() {
        None
    } else {
        Some(DelegationScope::Tenants(tenants))
    }
}

/// Returns whether a parent `Namespace` selector covers (is at least as broad
/// as) a child namespace: root is greater than any tenant, and a tenant
/// covers only itself.
fn namespace_covers(parent: &Namespace, child: &Namespace) -> bool {
    match parent {
        Namespace::Root => true,
        Namespace::Tenant(parent_tenant) => child == &Namespace::Tenant(parent_tenant.clone()),
    }
}

fn parent_grant_covers_child(parent: &CapabilityGrant, child: &CapabilityGrant) -> bool {
    if parent.capability != child.capability {
        return false;
    }

    parent.selectors.iter().all(|parent_selector| match parent_selector {
        ResourceSelector::Tenant(parent_tenant) => child.selectors.iter().any(|selector| {
            matches!(selector, ResourceSelector::Tenant(child_tenant) if child_tenant == parent_tenant)
        }),
        ResourceSelector::Namespace(parent_scope) => {
            child.selectors.iter().any(|selector| match selector {
                ResourceSelector::Namespace(child_scope) => {
                    namespace_covers(parent_scope, child_scope)
                }
                ResourceSelector::Tenant(child_tenant) => {
                    namespace_covers(parent_scope, &Namespace::Tenant(child_tenant.clone()))
                }
                _ => false,
            })
        }
        ResourceSelector::UriPrefix(parent_prefix) => child.selectors.iter().any(|selector| {
            matches!(selector, ResourceSelector::UriPrefix(child_prefix) if child_prefix.starts_with(parent_prefix))
        }),
        ResourceSelector::Locality(parent_locality) => child.selectors.iter().any(|selector| {
            matches!(selector, ResourceSelector::Locality(child_locality) if parent_locality.matches(child_locality))
        }),
        ResourceSelector::ResourceClass(parent_class) => child.selectors.iter().any(|selector| {
            matches!(selector, ResourceSelector::ResourceClass(child_class) if child_class == parent_class)
        }),
        ResourceSelector::ExplicitResource(parent_identity) => {
            child.selectors.iter().any(|selector| {
                matches!(selector, ResourceSelector::ExplicitResource(child_identity) if child_identity == parent_identity)
            })
        }
        ResourceSelector::Children => child.selectors.iter().any(|selector| {
            matches!(selector, ResourceSelector::Children)
        }),
    })
}

/// Converts a selium-abi `RegionProt` to wasmtiny's `RegionProt`.
///
/// The selium-abi crate defines its own `RegionProt` enum to maintain
/// independence from the wasmtiny runtime implementation. This conversion
/// function bridges the two types at the runtime boundary where hostcalls
/// need to pass protection flags to the WASM engine.
///
/// Both enums have identical variants (ReadOnly, ReadWrite) and semantics,
/// so the conversion is a simple 1:1 mapping.
fn to_wasm_prot(prot: selium_abi::RegionProt) -> WasmProt {
    match prot {
        selium_abi::RegionProt::ReadOnly => WasmProt::ReadOnly,
        selium_abi::RegionProt::ReadWrite => WasmProt::ReadWrite,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};
    use selium_abi::{GuestLogEntry, MeteringObservation, ResourceSelector};

    fn module_with_entrypoint(entrypoint: &str, body: &str) -> Vec<u8> {
        wat::parse_str(format!("(module (func (export \"{entrypoint}\") {body}))"))
            .expect("compile wat")
    }

    fn spawn_with_grants(
        runtime: &Runtime,
        grants: Vec<CapabilityGrant>,
    ) -> crate::BootstrappedGuest {
        runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "hostcall-test".to_string(),
                module_id: "hostcall-test-module".to_string(),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants,
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn hostcall test guest")
    }

    fn spawn_with_grants_and_tenant(
        runtime: &Runtime,
        name: &str,
        grants: Vec<CapabilityGrant>,
        tenant: Option<&str>,
    ) -> crate::BootstrappedGuest {
        runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: name.to_string(),
                module_id: format!("{name}-module"),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants,
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: tenant.map(str::to_string),
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn hostcall test guest")
    }

    fn ready(
        runtime: &Runtime,
        process_id: ProcessId,
        operation_id: OperationId,
    ) -> HostcallOutput {
        match runtime.poll_hostcall(process_id, operation_id) {
            CompletionState::Ready(output) => output,
            other => panic!("expected ready hostcall, got {other:?}"),
        }
    }

    #[test]
    fn operation_ids_roll_over_without_saturating() {
        let runtime = Runtime::default();
        let bootstrapped = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "rollover".to_string(),
                module_id: "rollover-module".to_string(),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                )],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn rollover guest");
        *runtime.next_operation_id.lock() = OperationId::MAX;

        let (first_status, first_id) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        let (second_status, second_id) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );

        assert_eq!(first_status, selium_abi::HOSTCALL_STATUS_READY);
        assert_eq!(second_status, selium_abi::HOSTCALL_STATUS_READY);
        assert_eq!(first_id, OperationId::MAX);
        assert_eq!(second_id, 1);
    }

    #[test]
    fn storage_hostcalls_cover_logs_and_blobs() {
        let runtime = Runtime::default();
        let bootstrapped = spawn_with_grants(
            &runtime,
            vec![
                CapabilityGrant::new(
                    Capability::Storage,
                    vec![ResourceSelector::ResourceClass(ResourceClass::DurableLog)],
                ),
                CapabilityGrant::new(
                    Capability::Storage,
                    vec![ResourceSelector::ResourceClass(ResourceClass::BlobStore)],
                ),
            ],
        );

        let (_, open_log_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageOpenLog {
                name: "audit".to_string(),
            },
        );
        let HostcallOutput::DurableLog(log) = ready(&runtime, bootstrapped.process_id, open_log_op)
        else {
            panic!("expected durable log");
        };
        let (_, append_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageLogAppend {
                local_id: log.local_id,
                timestamp_ms: 42,
                headers: Vec::new(),
                payload: b"entry".to_vec(),
            },
        );
        let HostcallOutput::Sequence(Some(sequence)) =
            ready(&runtime, bootstrapped.process_id, append_op)
        else {
            panic!("expected appended sequence");
        };
        let (_, checkpoint_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageLogCheckpoint {
                local_id: log.local_id,
                name: "boot".to_string(),
                sequence,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, checkpoint_op),
            HostcallOutput::Empty
        );
        let (_, checkpoint_read_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageLogCheckpointRead {
                local_id: log.local_id,
                name: "boot".to_string(),
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, checkpoint_read_op),
            HostcallOutput::Sequence(Some(sequence))
        );
        let (_, replay_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageLogReplay {
                local_id: log.local_id,
                from_sequence: Some(sequence),
                limit: 1,
            },
        );
        let HostcallOutput::StorageRecords(records) =
            ready(&runtime, bootstrapped.process_id, replay_op)
        else {
            panic!("expected log records");
        };
        assert_eq!(records[0].payload, b"entry".to_vec());

        let (_, open_blob_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageOpenBlobStore {
                name: "assets".to_string(),
            },
        );
        let HostcallOutput::BlobStore(store) =
            ready(&runtime, bootstrapped.process_id, open_blob_op)
        else {
            panic!("expected blob store");
        };
        let (_, put_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageBlobPut {
                local_id: store.local_id,
                bytes: b"blob".to_vec(),
            },
        );
        let HostcallOutput::BlobId(blob_id) = ready(&runtime, bootstrapped.process_id, put_op)
        else {
            panic!("expected blob id");
        };
        let (_, manifest_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageBlobSetManifest {
                local_id: store.local_id,
                name: "latest".to_string(),
                blob_id: blob_id.clone(),
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, manifest_op),
            HostcallOutput::Empty
        );
        let (_, get_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageBlobGet {
                local_id: store.local_id,
                blob_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, get_op),
            HostcallOutput::Bytes(b"blob".to_vec())
        );
        let (_, manifest_read_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageBlobGetManifest {
                local_id: store.local_id,
                name: "latest".to_string(),
            },
        );
        assert!(matches!(
            ready(&runtime, bootstrapped.process_id, manifest_read_op),
            HostcallOutput::BlobId(_)
        ));
        let (_, close_log_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageLogClose {
                local_id: log.local_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, close_log_op),
            HostcallOutput::Empty
        );
        let (_, close_blob_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::StorageBlobStoreClose {
                local_id: store.local_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, close_blob_op),
            HostcallOutput::Empty
        );
    }

    #[test]
    fn process_activity_metering_and_guest_log_hostcalls_work() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");
        let bootstrapped = spawn_with_grants(
            &runtime,
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::Locality(
                        selium_abi::LocalityScope::Cluster,
                    )],
                ),
                CapabilityGrant::new(
                    Capability::ActivityRead,
                    vec![ResourceSelector::ResourceClass(ResourceClass::ActivityLog)],
                ),
                CapabilityGrant::new(
                    Capability::MeteringRead,
                    vec![ResourceSelector::ResourceClass(
                        ResourceClass::MeteringStream,
                    )],
                ),
                CapabilityGrant::new(
                    Capability::GuestLogWrite,
                    vec![ResourceSelector::ResourceClass(ResourceClass::GuestLog)],
                ),
                CapabilityGrant::new(
                    Capability::GuestLogRead,
                    vec![ResourceSelector::ResourceClass(ResourceClass::GuestLog)],
                ),
            ],
        );

        let child_grants = vec![CapabilityGrant::new(
            Capability::ProcessLifecycle,
            vec![ResourceSelector::Locality(
                selium_abi::LocalityScope::Cluster,
            )],
        )];
        let (_, start_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: child_grants,
                tenant: None,
            },
        );
        let HostcallOutput::Process(child) = ready(&runtime, bootstrapped.process_id, start_op)
        else {
            panic!("expected child process");
        };
        runtime.project_metering(
            child.local_id,
            MeteringObservation {
                cpu_instructions: 1,
                memory_bytes: 2,
                storage_bytes: 3,
                bandwidth_bytes: 4,
            },
        );
        let (_, meter_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::MeteringRead {
                process_id: child.local_id,
            },
        );
        assert!(matches!(
            ready(&runtime, bootstrapped.process_id, meter_op),
            HostcallOutput::Metering(_)
        ));
        let entry = GuestLogEntry {
            process_id: Some(child.local_id),
            level: "INFO".to_string(),
            target: "test".to_string(),
            message: "hello".to_string(),
        };
        let (_, write_log_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::GuestLogWrite { entry },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, write_log_op),
            HostcallOutput::Empty
        );
        let (_, read_log_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::GuestLogRead {
                cursor: 0,
                process_id: Some(child.local_id),
            },
        );
        let HostcallOutput::GuestLogEntries(entries) =
            ready(&runtime, bootstrapped.process_id, read_log_op)
        else {
            panic!("expected guest log entries");
        };
        assert_eq!(entries[0].message, "hello");
        let (_, activity_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::ActivityRead { cursor: 0 },
        );
        assert!(matches!(
            ready(&runtime, bootstrapped.process_id, activity_op),
            HostcallOutput::ActivityEvents(_)
        ));
        let (_, stop_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::ProcessStop {
                process_id: child.local_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, stop_op),
            HostcallOutput::Empty
        );
    }

    #[test]
    fn guest_log_register_valid_and_foreign() {
        let runtime = Runtime::default();
        let bootstrapped = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );

        // Allocate a shared region owned by this process.
        let (_, alloc_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::LogChannel,
                serving_tenant: None,
            },
        );
        let HostcallOutput::RegionAlloc(alloc) = ready(&runtime, bootstrapped.process_id, alloc_op)
        else {
            panic!("expected RegionAlloc");
        };

        // GuestLogRegister with own shared_id should succeed.
        let (_, reg_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::GuestLogRegister {
                shared_id: alloc.region_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, reg_op),
            HostcallOutput::Empty
        );

        // Verify the kernel recorded the log channel.
        assert_eq!(
            runtime
                .kernel()
                .processes()
                .log_channel_shared_id(bootstrapped.process_id),
            Some(alloc.region_id)
        );

        // GuestLogRegister with a non-existent shared_id should fail.
        let (status, foreign_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::GuestLogRegister { shared_id: 99999 },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(bootstrapped.process_id, foreign_op),
            CompletionState::Failed(_)
        ));
    }

    #[test]
    fn tenant_scoped_grant_enforces_isolation() {
        let runtime = Runtime::default();

        // Process A: tenant "acme", grant scoped to Tenant("acme") + ResourceClass.
        let guest_a = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "tenant-a".to_string(),
                module_id: "tenant-a-module".to_string(),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![
                        ResourceSelector::Tenant("acme".to_string()),
                        ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
                    ],
                )],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: Some("acme".to_string()),
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn tenant-a guest");

        // Process B: tenant "beta", same grant scoped to Tenant("acme").
        let guest_b = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "tenant-b".to_string(),
                module_id: "tenant-b-module".to_string(),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![
                        ResourceSelector::Tenant("acme".to_string()),
                        ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
                    ],
                )],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: Some("beta".to_string()),
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn tenant-b guest");

        // Tenant "acme" process A: AllocRegion should succeed.
        let (status_a, _op_a) = runtime.begin_hostcall(
            guest_a.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(status_a, selium_abi::HOSTCALL_STATUS_READY);

        // Tenant "beta" process B: AllocRegion should be denied.
        let (status_b, op_b) = runtime.begin_hostcall(
            guest_b.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(status_b, selium_abi::HOSTCALL_STATUS_FAILED);

        // Verify error attribution names the capability and tenant.
        match runtime.poll_hostcall(guest_b.process_id, op_b) {
            CompletionState::Failed(error) => {
                assert!(
                    error.message.contains("SharedMemory"),
                    "error should name the denied capability: {error:?}"
                );
                assert!(
                    error.message.contains("\"beta\""),
                    "error should name the tenant: {error:?}"
                );
            }
            other => panic!("expected failed hostcall for tenant-b, got {other:?}"),
        }
    }

    #[test]
    fn unevaluatable_selector_rejected_at_spawn() {
        let runtime = Runtime::default();

        // A grant with UriPrefix should be rejected at spawn time — never
        // accepted and then always denied at hostcall time.
        let result = runtime.spawn_system_guest(SystemGuestDescriptor {
            name: "uri-prefix-rejected".to_string(),
            module_id: "uri-prefix-module".to_string(),
            module_bytes: module_with_entrypoint("boot", ""),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::UriPrefix("sel://acme/".to_string())],
            )],
            dependencies: Vec::new(),
            readiness: ReadinessCondition::Immediate,
            tenant: None,
            serving_role: None,
            handlers: Vec::new(),
        });

        let error = result.expect_err("should reject UriPrefix grant at spawn");
        assert!(
            error.to_string().contains("UriPrefix"),
            "error should name the rejected selector: {error}"
        );
    }

    #[test]
    fn accept_then_deny_trap_is_impossible() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");
        let bootstrapped = spawn_with_grants(
            &runtime,
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                ),
            ],
        );

        // ProcessStart with a UriPrefix grant must fail immediately at spawn,
        // not pass validation and deny every subsequent hostcall.
        let (status, op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::UriPrefix("sel://acme/".to_string())],
                )],
                tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

        match runtime.poll_hostcall(bootstrapped.process_id, op) {
            CompletionState::Failed(error) => {
                assert!(
                    error.message.contains("UriPrefix"),
                    "error should name the rejected selector: {error:?}"
                );
                assert_eq!(error.code, AbiErrorCode::MalformedPayload);
            }
            other => panic!("expected failed hostcall for accept-then-deny trap, got {other:?}"),
        }
    }

    #[test]
    fn delegator_within_tenant_spawns_child_with_extra_grants() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        // A per-tenant bridge-server holding DelegateGrants(Tenant "acme")
        // but no Network grant of its own can still confer a tenant-scoped
        // Network grant on a child within the same tenant.
        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "delegating-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Tenant("acme".to_string())],
                ),
            ],
            Some("acme"),
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::Network,
                    vec![
                        ResourceSelector::Tenant("acme".to_string()),
                        ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                    ],
                )],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_READY,
            "delegation within the tenant must succeed"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Ready(HostcallOutput::Process(_))
        ));
    }

    #[test]
    fn non_delegator_out_of_scope_child_denied() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "plain-parent",
            vec![CapabilityGrant::new(
                Capability::ProcessLifecycle,
                vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
            )],
            Some("acme"),
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::Network,
                    vec![
                        ResourceSelector::Tenant("acme".to_string()),
                        ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                    ],
                )],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "a non-delegator child grant exceeding its own must be denied"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    #[test]
    fn delegation_outside_tenant_denied() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "acme-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Tenant("acme".to_string())],
                ),
            ],
            Some("acme"),
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::Network,
                    vec![
                        ResourceSelector::Tenant("beta".to_string()),
                        ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                    ],
                )],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "delegation outside the tenant must be denied"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    #[test]
    fn delegation_denies_unscoped_child_grant() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "acme-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Tenant("acme".to_string())],
                ),
            ],
            Some("acme"),
        );

        // The mixed set contains one tenant-scoped grant and one
        // unrestricted (empty-selector) grant. The unrestricted grant must
        // not be conferable under delegation: it escapes the tenant fence,
        // so the spawn falls through to the subset check and is denied.
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![
                    CapabilityGrant::new(
                        Capability::Network,
                        vec![
                            ResourceSelector::Tenant("acme".to_string()),
                            ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                        ],
                    ),
                    CapabilityGrant::new(Capability::Storage, Vec::new()),
                ],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "an unscoped child grant must not ride the delegation path"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    /// A `Namespace::Root`-scoped delegator spawns a tenant-scoped child
    /// whose grants the root delegator does not itself hold: the root
    /// namespace is greater than any tenant, so any tenant-scoped child grant
    /// is admitted.
    #[test]
    fn root_delegator_spawns_tenant_scoped_child() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "root-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Namespace(Namespace::Root)],
                ),
            ],
            None,
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::Network,
                    vec![
                        ResourceSelector::Tenant("acme".to_string()),
                        ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                    ],
                )],
                tenant: Some("acme".to_string()),
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_READY,
            "a root delegator must admit any tenant-scoped child grant"
        );
        let child = match runtime.poll_hostcall(parent.process_id, op) {
            CompletionState::Ready(HostcallOutput::Process(child)) => child,
            other => panic!("expected child process descriptor, got {other:?}"),
        };
        assert_eq!(
            runtime.process_tenant(child.local_id).as_deref(),
            Some("acme"),
            "the root delegator spawns the child under the requested tenant"
        );
    }

    /// A root principal without a `DelegateGrants` grant cannot spawn a child
    /// under a tenant: cross-tenant spawn authority is a grant, not an
    /// accident of the parent's bootstrap tenant (the rejected
    /// "root guest implicitly delegates" shortcut).
    #[test]
    fn root_principal_without_delegate_grants_cannot_spawn_tenant_child() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "root-spawner",
            vec![CapabilityGrant::new(
                Capability::ProcessLifecycle,
                vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
            )],
            None,
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: Some("acme".to_string()),
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "a root parent without DelegateGrants must not tenant-scope a spawn"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    /// A `Namespace::Root`-scoped delegator must still refuse an unscoped
    /// (selector-less) child grant: such a grant is unrestricted within its
    /// capability and not conferable under delegation — it falls through to
    /// the subset check and is denied.
    #[test]
    fn root_delegator_denies_unscoped_child_grant() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "root-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Namespace(Namespace::Root)],
                ),
            ],
            None,
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(Capability::Storage, Vec::new())],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "an unscoped child grant must not ride the root delegation path"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    /// A tenant-scoped delegator cannot spawn a child under a foreign tenant,
    /// even with grants scoped to that foreign tenant: the requested child
    /// tenant is outside its `DelegateGrants` scope (`resolve_spawn_tenant`).
    #[test]
    fn tenant_delegator_cannot_spawn_child_in_foreign_tenant() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "acme-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Tenant("acme".to_string())],
                ),
            ],
            Some("acme"),
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![
                    CapabilityGrant::new(
                        Capability::Network,
                        vec![
                            ResourceSelector::Tenant("beta".to_string()),
                            ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                        ],
                    ),
                    CapabilityGrant::new(
                        Capability::HostQueue,
                        vec![ResourceSelector::Tenant("beta".to_string())],
                    ),
                ],
                tenant: Some("beta".to_string()),
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "a tenant-scoped delegator must not spawn a child in a foreign tenant"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    #[test]
    fn delegate_grants_cannot_be_conferred_on_children() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "acme-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Tenant("acme".to_string())],
                ),
            ],
            Some("acme"),
        );

        // Attempt to re-delegate `DelegateGrants` itself (tenant-scoped, so
        // the tenant fence alone would admit it): conferment must be denied
        // outright.
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Tenant("acme".to_string())],
                )],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "DelegateGrants must never be conferred on a child process"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    #[test]
    fn mint_certificate_is_bootstrap_provisionable() {
        let runtime = Runtime::default();

        // Bootstrap provisioning admits the capability: a system guest holding
        // `MintCertificate` starts normally and carries the grant in its
        // persisted authority.
        let bootstrapped = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::MintCertificate,
                Vec::new(),
            )],
        );

        assert!(
            runtime
                .restore_process_authority(bootstrapped.process_id)
                .is_some_and(|authority| authority
                    .grants
                    .iter()
                    .any(|grant| grant.capability == Capability::MintCertificate))
        );
    }

    #[test]
    fn mint_certificate_cannot_be_conferred_on_children() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        // A parent holding `MintCertificate` (bootstrap-provisioned) and
        // `ProcessLifecycle` cannot confer mint authority on a child: the
        // spawn is denied with a capability error, mirroring `DelegateGrants`.
        let parent = spawn_with_grants(
            &runtime,
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(Capability::MintCertificate, Vec::new()),
            ],
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::MintCertificate,
                    Vec::new(),
                )],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "MintCertificate must never be conferred on a child process"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    #[test]
    fn quota_write_is_bootstrap_provisionable() {
        let runtime = Runtime::default();

        // Bootstrap provisioning admits the capability: a system guest holding
        // `QuotaWrite` starts normally and carries the grant in its persisted
        // authority.
        let bootstrapped = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(Capability::QuotaWrite, Vec::new())],
        );

        assert!(
            runtime
                .restore_process_authority(bootstrapped.process_id)
                .is_some_and(|authority| authority
                    .grants
                    .iter()
                    .any(|grant| grant.capability == Capability::QuotaWrite))
        );
    }

    #[test]
    fn quota_write_cannot_be_conferred_on_children() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        // A parent holding `QuotaWrite` (bootstrap-provisioned) and
        // `ProcessLifecycle` cannot confer quota authorship on a child: the
        // spawn is denied with a capability error, mirroring `MintCertificate`.
        let parent = spawn_with_grants(
            &runtime,
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(Capability::QuotaWrite, Vec::new()),
            ],
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(Capability::QuotaWrite, Vec::new())],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "QuotaWrite must never be conferred on a child process"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    #[test]
    fn quota_hostcalls_deny_without_quota_write() {
        let runtime = Runtime::default();
        // A guest without `QuotaWrite` is denied the quota hostcalls: quota
        // authorship is bootstrap-provisioned to the accounting guest alone.
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );

        for request in [
            HostcallRequest::QuotaSet {
                tenant: "acme".to_string(),
                class: ResourceClass::SharedRegion,
                limit: 1024,
            },
            HostcallRequest::QuotaClear {
                tenant: "acme".to_string(),
                class: ResourceClass::SharedRegion,
            },
        ] {
            let (status, op) = runtime.begin_hostcall(guest.process_id, request);
            assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
            assert!(matches!(
                runtime.poll_hostcall(guest.process_id, op),
                CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
            ));
        }
    }

    #[test]
    fn over_ceiling_shared_memory_allocation_is_denied() {
        let runtime = Runtime::default();
        // Author a 64 KiB shared-memory ceiling for `acme` (the runtime gates
        // `QuotaSet` behind `QuotaWrite`, covered by the test above; writing
        // the table directly keeps this test focused on enforcement).
        runtime
            .kernel
            .quota()
            .set("acme", ResourceClass::SharedRegion, 65_536);

        let tenant_guest = spawn_with_grants_and_tenant(
            &runtime,
            "acme-worker",
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
            Some("acme"),
        );

        // One page fits the ceiling exactly; a second page exceeds it.
        let (first_status, _first_op) = runtime.begin_hostcall(
            tenant_guest.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(first_status, selium_abi::HOSTCALL_STATUS_READY);

        let (second_status, second_op) = runtime.begin_hostcall(
            tenant_guest.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(second_status, selium_abi::HOSTCALL_STATUS_FAILED);
        match runtime.poll_hostcall(tenant_guest.process_id, second_op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::QuotaExceeded);
                assert!(
                    error.message.contains("acme"),
                    "error names the tenant: {}",
                    error.message
                );
                assert!(
                    error.message.contains("SharedRegion"),
                    "error names the dimension: {}",
                    error.message
                );
            }
            other => panic!("expected quota denial, got {other:?}"),
        }
    }

    /// Pipe quotas meter queued *items*, not queue count: creating a queue
    /// costs nothing, each queued item reserves one slot against the queue
    /// owner's tenant, an over-ceiling send is denied with `QuotaExceeded`,
    /// and receiving releases the slot.
    #[test]
    fn pipe_quota_meters_queued_items_not_queue_count() {
        let runtime = Runtime::default();

        // The queue owner: tenant acme, holding the pipe capability.
        let owner = spawn_with_grants_and_tenant(
            &runtime,
            "queue-owner",
            vec![CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            )],
            Some("acme"),
        )
        .process_id;

        // Creating a queue consumes no slot.
        let (create_status, create_op) = runtime.begin_hostcall(
            owner,
            HostcallRequest::HostQueueCreate {
                serving_tenant: None,
            },
        );
        assert_eq!(create_status, selium_abi::HOSTCALL_STATUS_READY);
        let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
            runtime.poll_hostcall(owner, create_op)
        else {
            panic!("owner should create its queue");
        };
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::HostQueue),
            0
        );

        // Author a two-item pipe ceiling for acme.
        runtime
            .kernel()
            .quota()
            .set("acme", ResourceClass::HostQueue, 2);

        // A sender with an explicit grant for the owner's queue: it attaches
        // the queue to obtain its own local handle, then sends.
        let sender = spawn_with_grants_and_tenant(
            &runtime,
            "queue-sender",
            vec![CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ExplicitResource(
                    ResourceIdentity::Shared(queue.shared_id),
                )],
            )],
            None,
        )
        .process_id;

        let (attach_status, attach_op) = runtime.begin_hostcall(
            sender,
            HostcallRequest::HostQueueAttach {
                shared_id: queue.shared_id,
            },
        );
        assert_eq!(attach_status, selium_abi::HOSTCALL_STATUS_READY);
        let CompletionState::Ready(HostcallOutput::HostQueue(sender_queue)) =
            runtime.poll_hostcall(sender, attach_op)
        else {
            panic!("sender should attach the owner's queue");
        };

        let send = |value: u64| {
            runtime.begin_hostcall(
                sender,
                HostcallRequest::HostQueueSend {
                    local_id: sender_queue.local_id,
                    value,
                    metadata: Vec::new(),
                },
            )
        };

        // Two items fit the ceiling; the third is denied.
        let (first, _) = send(1);
        assert_eq!(first, selium_abi::HOSTCALL_STATUS_READY);
        let (second, _) = send(2);
        assert_eq!(second, selium_abi::HOSTCALL_STATUS_READY);
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::HostQueue),
            2
        );
        let (third_status, third_op) = send(3);
        assert_eq!(third_status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(sender, third_op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::QuotaExceeded
        ));

        // The owner receives one item: its slot is released and a further
        // send fits again.
        let (recv_status, recv_op) = runtime.begin_hostcall(
            owner,
            HostcallRequest::HostQueueRecv {
                local_id: queue.local_id,
            },
        );
        assert_eq!(recv_status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(owner, recv_op),
            CompletionState::Ready(HostcallOutput::ConnectionInfo { value: 1, .. })
        ));
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::HostQueue),
            1
        );
        let (fourth, _) = send(4);
        assert_eq!(fourth, selium_abi::HOSTCALL_STATUS_READY);
    }

    /// A dying process releases its live reservations: allocated regions'
    /// bytes return to the tenant's quota, and pipe slots held by items
    /// still queued in its queues are returned. Storage stays sticky.
    #[test]
    fn process_teardown_releases_region_and_pipe_quota() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants_and_tenant(
            &runtime,
            "acme-worker",
            vec![
                CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                ),
                CapabilityGrant::new(
                    Capability::HostQueue,
                    vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
                ),
            ],
            Some("acme"),
        )
        .process_id;

        // Allocate a one-page region and leave one item queued.
        let (alloc_status, alloc_op) = runtime.begin_hostcall(
            guest,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(alloc_status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(guest, alloc_op),
            CompletionState::Ready(HostcallOutput::RegionAlloc(_))
        ));

        let (queue_status, queue_op) = runtime.begin_hostcall(
            guest,
            HostcallRequest::HostQueueCreate {
                serving_tenant: None,
            },
        );
        assert_eq!(queue_status, selium_abi::HOSTCALL_STATUS_READY);
        let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
            runtime.poll_hostcall(guest, queue_op)
        else {
            panic!("guest should create its queue");
        };
        let (send_status, _) = runtime.begin_hostcall(
            guest,
            HostcallRequest::HostQueueSend {
                local_id: queue.local_id,
                value: 1,
                metadata: Vec::new(),
            },
        );
        assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::SharedRegion),
            65_536
        );
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::HostQueue),
            1
        );

        // The worker dies: its region and queued item return to the quota.
        runtime.stop_process(guest).expect("stop worker");
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::SharedRegion),
            0,
            "destroyed region's bytes must return to the tenant's quota"
        );
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::HostQueue),
            0,
            "queued items of a dead owner must release their pipe slots"
        );
    }

    /// A handed-off region transfers: the sender's entry leaves its resource
    /// table (it can no longer free the region) and the receiver's gains it,
    /// with the quota reservation following the resource across tenants.
    #[test]
    fn handoff_transfers_region_ownership_and_quota() {
        let runtime = Runtime::default();

        // The receiving side: tenant beta, holding memory and pipe
        // capabilities.
        let receiver = spawn_with_grants_and_tenant(
            &runtime,
            "beta-receiver",
            vec![
                CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                ),
                CapabilityGrant::new(
                    Capability::HostQueue,
                    vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
                ),
            ],
            Some("beta"),
        )
        .process_id;

        // The receiver creates a queue under its own tenant first: the
        // sender (a different tenant) then spawns with an explicit grant for
        // that queue so it may attach and send into it.
        let (queue_status, queue_op) = runtime.begin_hostcall(
            receiver,
            HostcallRequest::HostQueueCreate {
                serving_tenant: None,
            },
        );
        assert_eq!(queue_status, selium_abi::HOSTCALL_STATUS_READY);
        let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
            runtime.poll_hostcall(receiver, queue_op)
        else {
            panic!("receiver should create its queue");
        };

        // The sending side: tenant acme, holding memory and an explicit
        // grant for the receiver's queue.
        let sender = spawn_with_grants_and_tenant(
            &runtime,
            "acme-sender",
            vec![
                CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                ),
                CapabilityGrant::new(
                    Capability::HostQueue,
                    vec![ResourceSelector::ExplicitResource(
                        ResourceIdentity::Shared(queue.shared_id),
                    )],
                ),
            ],
            Some("acme"),
        )
        .process_id;

        // The sender allocates a one-page region (charged to acme).
        let (alloc_status, alloc_op) = runtime.begin_hostcall(
            sender,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(alloc_status, selium_abi::HOSTCALL_STATUS_READY);
        let CompletionState::Ready(HostcallOutput::RegionAlloc(region)) =
            runtime.poll_hostcall(sender, alloc_op)
        else {
            panic!("sender should allocate its region");
        };
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::SharedRegion),
            65_536
        );

        let (attach_status, attach_op) = runtime.begin_hostcall(
            sender,
            HostcallRequest::HostQueueAttach {
                shared_id: queue.shared_id,
            },
        );
        assert_eq!(attach_status, selium_abi::HOSTCALL_STATUS_READY);
        let CompletionState::Ready(HostcallOutput::HostQueue(sender_queue)) =
            runtime.poll_hostcall(sender, attach_op)
        else {
            panic!("sender should attach the receiver's queue");
        };

        let (send_status, _) = runtime.begin_hostcall(
            sender,
            HostcallRequest::HostQueueSend {
                local_id: sender_queue.local_id,
                value: region.region_id,
                metadata: Vec::new(),
            },
        );
        assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

        let (recv_status, recv_op) = runtime.begin_hostcall(
            receiver,
            HostcallRequest::HostQueueRecv {
                local_id: queue.local_id,
            },
        );
        assert_eq!(recv_status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(receiver, recv_op),
            CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) if value == region.region_id
        ));

        // The reservation followed the resource: acme's bytes moved to beta.
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::SharedRegion),
            0,
            "the sender's reservation must move with the handed-off region"
        );
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("beta", ResourceClass::SharedRegion),
            65_536,
            "the receiver's tenant inherits the region's reservation"
        );

        // Ownership transferred: the receiver frees the region (releasing
        // beta's reservation), and the sender can no longer free it.
        let (free_status, free_op) = runtime.begin_hostcall(
            receiver,
            HostcallRequest::FreeRegion {
                region_id: region.region_id,
            },
        );
        assert_eq!(free_status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(receiver, free_op),
            CompletionState::Ready(_)
        ));
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("beta", ResourceClass::SharedRegion),
            0
        );

        let (sender_free_status, _sender_free_op) = runtime.begin_hostcall(
            sender,
            HostcallRequest::FreeRegion {
                region_id: region.region_id,
            },
        );
        assert_eq!(
            sender_free_status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "the sender must no longer own the handed-off region"
        );
    }

    #[test]
    fn signing_hostcalls_deny_without_mint_certificate() {
        let runtime = Runtime::default();
        runtime.generate_keyring().expect("generate keyring");

        // A guest holding no `MintCertificate` grant is denied every signing
        // hostcall with a capability error.
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );

        for request in [
            HostcallRequest::SignTenantCa {
                tenant: "acme".to_string(),
            },
            HostcallRequest::SignUserCert {
                tenant: "acme".to_string(),
                spki_der: vec![0x30, 0x01],
            },
            HostcallRequest::RevokeCa {
                tenant: "acme".to_string(),
            },
        ] {
            let (status, op) = runtime.begin_hostcall(guest.process_id, request);
            assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
            assert!(matches!(
                runtime.poll_hostcall(guest.process_id, op),
                CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
            ));
        }
    }

    #[test]
    fn signing_hostcalls_return_certificates_only() {
        use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};

        let runtime = Runtime::default();
        runtime.generate_keyring().expect("generate keyring");

        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::MintCertificate,
                Vec::new(),
            )],
        );

        // SignTenantCa returns a DER tenant CA certificate (no key material).
        let (_, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::SignTenantCa {
                tenant: "acme".to_string(),
            },
        );
        let HostcallOutput::Certificate(tl_ca_der) = ready(&runtime, guest.process_id, op) else {
            panic!("SignTenantCa must return a certificate");
        };
        assert!(!tl_ca_der.is_empty());

        // SignUserCert signs a client SPKI, returning a DER leaf.
        let client_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("client key");
        let spki = client_key.subject_public_key_info();
        let (_, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::SignUserCert {
                tenant: "acme".to_string(),
                spki_der: spki,
            },
        );
        let HostcallOutput::Certificate(leaf_der) = ready(&runtime, guest.process_id, op) else {
            panic!("SignUserCert must return a certificate");
        };
        assert!(!leaf_der.is_empty());

        // RevokeCa returns empty, and a second revoke fails (key already gone).
        let (_, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::RevokeCa {
                tenant: "acme".to_string(),
            },
        );
        assert_eq!(ready(&runtime, guest.process_id, op), HostcallOutput::Empty);
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::RevokeCa {
                tenant: "acme".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::Internal
        ));
    }

    #[test]
    fn signing_hostcalls_fail_without_keyring() {
        let runtime = Runtime::default();
        // No keyring installed: the signing hostcall fails with an internal
        // error instead of silently returning a fabricated certificate.
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::MintCertificate,
                Vec::new(),
            )],
        );
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::SignTenantCa {
                tenant: "acme".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::Internal
        ));
    }

    #[test]
    fn record_resolved_region_grants_attach_authority() {
        let runtime = Runtime::default();

        let owner = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );
        // The consumer needs a memory-declaring module so the final
        // `AttachRegion` maps the region into its linear memory.
        let consumer = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "consumer".to_string(),
                module_id: "consumer-module".to_string(),
                module_bytes: wat::parse_str(r#"(module (memory 1) (func (export "boot")))"#)
                    .expect("compile consumer module"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                )],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn consumer");

        let (_, alloc_op) = runtime.begin_hostcall(
            owner.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        let HostcallOutput::RegionAlloc(alloc) = ready(&runtime, owner.process_id, alloc_op) else {
            panic!("expected region allocation");
        };

        // A peer that neither owns nor resolved the region is denied at attach.
        let (status, _) = runtime.begin_hostcall(
            consumer.process_id,
            HostcallRequest::AttachRegion {
                region_id: alloc.region_id,
                reader_slot: None,
                prot: selium_abi::RegionProt::ReadWrite,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

        // A non-discovery process cannot mint the resolve basis for itself.
        let (status, _) = runtime.begin_hostcall(
            consumer.process_id,
            HostcallRequest::RecordResolvedRegionFor {
                client_process_id: consumer.process_id,
                shared_id: alloc.region_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

        // The discovery system guest records the resolve basis on the
        // consumer's behalf, after which attach is authorised.
        let discovery =
            spawn_with_grants_and_tenant(&runtime, "discovery", vec![], None).process_id;
        let (status, _) = runtime.begin_hostcall(
            discovery,
            HostcallRequest::RecordResolvedRegionFor {
                client_process_id: consumer.process_id,
                shared_id: alloc.region_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);

        let (status, op) = runtime.begin_hostcall(
            consumer.process_id,
            HostcallRequest::AttachRegion {
                region_id: alloc.region_id,
                reader_slot: None,
                prot: selium_abi::RegionProt::ReadWrite,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(consumer.process_id, op),
            CompletionState::Ready(HostcallOutput::RegionAttach(_))
        ));
    }

    #[test]
    fn self_info_returns_process_id_and_tenant() {
        let runtime = Runtime::default();
        let guest =
            spawn_with_grants_and_tenant(&runtime, "tenant-guest", vec![], Some("acme")).process_id;

        let (status, op) = runtime.begin_hostcall(guest, HostcallRequest::SelfInfo);
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        match runtime.poll_hostcall(guest, op) {
            CompletionState::Ready(HostcallOutput::SelfInfo { process_id, tenant }) => {
                assert_eq!(process_id, guest);
                assert_eq!(tenant.as_deref(), Some("acme"));
            }
            other => panic!("expected SelfInfo output, got {other:?}"),
        }
    }

    #[test]
    fn resolve_protocol_handler_returns_registered_handler_pid() {
        let runtime = Runtime::default();
        let report = runtime
            .bootstrap_system_guests(RuntimeConfig {
                start_discovery: false,
                system_guests: vec![SystemGuestDescriptor {
                    name: "quic-connector".to_string(),
                    module_id: "quic-connector-module".to_string(),
                    module_bytes: module_with_entrypoint("boot", ""),
                    entrypoint: "boot".to_string(),
                    arguments: Vec::new(),
                    grants: Vec::new(),
                    dependencies: Vec::new(),
                    readiness: ReadinessCondition::Immediate,
                    tenant: None,
                    serving_role: None,
                    handlers: vec!["sel-quic".to_string()],
                }],
                domain_table: Vec::new(),
            })
            .expect("bootstrap connector");
        let connector = report.guests.first().expect("connector").process_id;
        let bystander =
            spawn_with_grants_and_tenant(&runtime, "bystander", vec![], None).process_id;

        // A guest can resolve the bootstrap-registered handler for a scheme.
        let (status, op) = runtime.begin_hostcall(
            bystander,
            HostcallRequest::ResolveProtocolHandler {
                scheme: "sel-quic".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        match runtime.poll_hostcall(bystander, op) {
            CompletionState::Ready(HostcallOutput::U64(pid)) => assert_eq!(pid, connector),
            other => panic!("expected handler pid, got {other:?}"),
        }

        // An unregistered scheme yields Empty, not a forged or stale pid.
        let (status, op) = runtime.begin_hostcall(
            bystander,
            HostcallRequest::ResolveProtocolHandler {
                scheme: "sel-http".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(bystander, op),
            CompletionState::Ready(HostcallOutput::Empty)
        ));
    }

    #[test]
    fn host_queue_send_rejects_oversized_metadata() {
        let runtime = Runtime::default();
        let sender = spawn_with_grants_and_tenant(
            &runtime,
            "meta-sender",
            vec![CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            )],
            None,
        )
        .process_id;

        let (_, op_id) = runtime.begin_hostcall(
            sender,
            HostcallRequest::HostQueueCreate {
                serving_tenant: None,
            },
        );
        let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
            runtime.poll_hostcall(sender, op_id)
        else {
            panic!("sender should create its listener queue");
        };

        let oversized = vec![0u8; selium_abi::METADATA_MAX_BYTES + 1];
        let (status, op) = runtime.begin_hostcall(
            sender,
            HostcallRequest::HostQueueSend {
                local_id: queue.local_id,
                value: 1,
                metadata: oversized,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "oversized handoff metadata must be rejected"
        );
        assert!(matches!(
            runtime.poll_hostcall(sender, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::MalformedPayload
        ));

        // The bound itself is admitted.
        let (status, op) = runtime.begin_hostcall(
            sender,
            HostcallRequest::HostQueueSend {
                local_id: queue.local_id,
                value: 2,
                metadata: vec![0u8; selium_abi::METADATA_MAX_BYTES],
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        drop(runtime.poll_hostcall(sender, op));
    }

    #[test]
    fn spawned_child_inherits_parent_tenant() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "bridge-channel-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "bridge-server-tenant",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Tenant("acme".to_string())],
                ),
            ],
            Some("acme"),
        );

        let child_grants = vec![
            CapabilityGrant::new(
                Capability::Network,
                vec![
                    ResourceSelector::Tenant("acme".to_string()),
                    ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                ],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![
                    ResourceSelector::Tenant("acme".to_string()),
                    ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
                ],
            ),
        ];
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "bridge-channel-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: child_grants,
                tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);

        let child_pid = match runtime.poll_hostcall(parent.process_id, op) {
            CompletionState::Ready(HostcallOutput::Process(process)) => process.local_id,
            other => panic!("expected child process descriptor, got {other:?}"),
        };

        assert_eq!(
            runtime.process_tenant(child_pid).as_deref(),
            Some("acme"),
            "a spawned bridge-channel must inherit the bridge-server's tenant"
        );
    }

    /// Task 3.1: a spawn for a tenant at its process ceiling is denied with
    /// `QuotaExceeded` before the child is created, while a tenant-less (root)
    /// spawn is not metered.
    #[test]
    fn process_start_enforces_tenant_process_quota_and_skips_root() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");
        runtime
            .kernel()
            .quota()
            .set("acme", ResourceClass::Process, 1);

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "root-spawner",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Namespace(Namespace::Root)],
                ),
            ],
            None,
        );

        // A tenant-less (root) spawn is not metered: it succeeds without
        // touching the ceiling.
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let root_child = match runtime.poll_hostcall(parent.process_id, op) {
            CompletionState::Ready(HostcallOutput::Process(process)) => process.local_id,
            other => panic!("expected root child descriptor, got {other:?}"),
        };

        // The first tenant-scoped spawn is within the ceiling.
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: Some("acme".to_string()),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let tenant_child = match runtime.poll_hostcall(parent.process_id, op) {
            CompletionState::Ready(HostcallOutput::Process(process)) => process.local_id,
            other => panic!("expected tenant child descriptor, got {other:?}"),
        };

        // The second tenant-scoped spawn is over the ceiling: denied before
        // any child is created.
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: Some("acme".to_string()),
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "an over-ceiling tenant spawn must be denied"
        );
        assert!(
            matches!(
                runtime.poll_hostcall(parent.process_id, op),
                CompletionState::Failed(error) if error.code == AbiErrorCode::QuotaExceeded
            ),
            "the over-ceiling spawn must fail with QuotaExceeded"
        );

        let _ = (root_child, tenant_child);
    }

    /// Task 3.2: the process-quota slot returns to the tenant when the child
    /// exits (is torn down), letting the tenant spawn again up to its ceiling.
    #[test]
    fn process_quota_slot_returns_to_tenant_on_exit() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");
        runtime
            .kernel()
            .quota()
            .set("acme", ResourceClass::Process, 1);

        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "root-spawner",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Namespace(Namespace::Root)],
                ),
            ],
            None,
        );

        // The only slot is consumed by the first child.
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: Some("acme".to_string()),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let child = match runtime.poll_hostcall(parent.process_id, op) {
            CompletionState::Ready(HostcallOutput::Process(process)) => process.local_id,
            other => panic!("expected child descriptor, got {other:?}"),
        };
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::Process),
            1,
            "the spawned child must hold the process slot"
        );

        // Stop the child (the existing teardown path): the slot returns.
        runtime.stop_process(child).expect("stop child");
        assert_eq!(
            runtime
                .kernel()
                .quota()
                .used("acme", ResourceClass::Process),
            0,
            "a torn-down child must return its process slot"
        );

        // The tenant can spawn again up to the ceiling.
        let (status, _op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: Some("acme".to_string()),
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_READY,
            "the released slot lets the tenant spawn again"
        );
    }

    #[test]
    fn empty_selector_grant_is_unrestricted_within_capability() {
        let runtime = Runtime::default();
        let bootstrapped = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "unrestricted".to_string(),
                module_id: "unrestricted-module".to_string(),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(Capability::SharedMemory, vec![])],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("empty selector grant should be accepted");

        let (status, _) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    }

    #[test]
    fn attach_isolation_prevents_guessing() {
        let runtime = Runtime::default();

        // Process A: can allocate shared memory.
        let guest_a = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );

        // Process B: has SharedMemory capability but does not own A's regions.
        let guest_b = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "guesser".to_string(),
                module_id: "guesser-module".to_string(),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                )],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn guesser");

        // A allocates a region.
        let (_, alloc_op) = runtime.begin_hostcall(
            guest_a.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        let HostcallOutput::RegionAlloc(alloc) = ready(&runtime, guest_a.process_id, alloc_op)
        else {
            panic!("expected RegionAlloc");
        };
        let region_id = alloc.region_id;

        // B tries to attach to A's region → denied (no ownership, no explicit grant).
        let (status, attach_op) = runtime.begin_hostcall(
            guest_b.process_id,
            HostcallRequest::AttachRegion {
                region_id,
                reader_slot: None,
                prot: selium_abi::RegionProt::ReadWrite,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        match runtime.poll_hostcall(guest_b.process_id, attach_op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::PermissionDenied);
                assert!(
                    error.message.contains("does not own"),
                    "error should mention ownership: {error:?}"
                );
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn attach_succeeds_with_explicit_resource_grant() {
        let runtime = Runtime::default();

        // Process A: can allocate shared memory.
        let guest_a = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );

        // A allocates a region.
        let (_, alloc_op) = runtime.begin_hostcall(
            guest_a.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        let HostcallOutput::RegionAlloc(alloc) = ready(&runtime, guest_a.process_id, alloc_op)
        else {
            panic!("expected RegionAlloc");
        };
        let region_id = alloc.region_id;

        // Process B: has ExplicitResource grant for the specific region.
        let guest_b = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "explicit-b".to_string(),
                module_id: "explicit-b-module".to_string(),
                module_bytes: wat::parse_str("(module (memory 1) (func (export \"boot\")))")
                    .expect("compile wat"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ExplicitResource(
                        ResourceIdentity::Shared(region_id),
                    )],
                )],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn explicit-b");

        // B attaches to A's region with the ExplicitResource grant → succeeds.
        let (status, attach_op) = runtime.begin_hostcall(
            guest_b.process_id,
            HostcallRequest::AttachRegion {
                region_id,
                reader_slot: None,
                prot: selium_abi::RegionProt::ReadWrite,
            },
        );
        if status != selium_abi::HOSTCALL_STATUS_READY {
            match runtime.poll_hostcall(guest_b.process_id, attach_op) {
                CompletionState::Failed(error) => {
                    panic!("attach with ExplicitResource grant should succeed: {error:?}");
                }
                other => panic!("expected ready, got {other:?}"),
            }
        }
    }

    #[test]
    fn children_selector_allows_descendant_metering_read() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "meter-child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        let parent = spawn_with_grants(
            &runtime,
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::MeteringRead,
                    vec![ResourceSelector::ResourceClass(
                        ResourceClass::MeteringStream,
                    )],
                ),
                CapabilityGrant::new(Capability::MeteringRead, vec![ResourceSelector::Children]),
            ],
        );

        let (_, start_op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "meter-child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![],
                tenant: None,
            },
        );
        let HostcallOutput::Process(child) = ready(&runtime, parent.process_id, start_op) else {
            panic!("expected child process");
        };
        runtime.project_metering(
            child.local_id,
            MeteringObservation {
                cpu_instructions: 42,
                memory_bytes: 0,
                storage_bytes: 0,
                bandwidth_bytes: 0,
            },
        );

        // Parent reads descendant metering with Children selector → succeeds.
        let (status, meter_op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::MeteringRead {
                process_id: child.local_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            ready(&runtime, parent.process_id, meter_op),
            HostcallOutput::Metering(_)
        ));

        // Parent reads an UNrelated process's metering → Children grant does
        // NOT match, but the ResourceClass grant DOES (so this succeeds).
        // To test denial, we need a grant with ONLY Children (no class-level).
    }

    #[test]
    fn children_selector_denies_unrelated_process() {
        let runtime = Runtime::default();

        // Parent has ONLY Children-scoped MeteringRead (no class-level).
        let parent = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::MeteringRead,
                vec![ResourceSelector::Children],
            )],
        );

        // An unrelated process.
        let unrelated = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );
        runtime.project_metering(
            unrelated.process_id,
            MeteringObservation {
                cpu_instructions: 7,
                memory_bytes: 0,
                storage_bytes: 0,
                bandwidth_bytes: 0,
            },
        );

        // Parent reads unrelated process metering with Only Children → denied.
        let (status, _) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::MeteringRead {
                process_id: unrelated.process_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
    }

    #[test]
    fn guest_log_write_accepts_own_pid() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::GuestLogWrite,
                vec![ResourceSelector::ResourceClass(ResourceClass::GuestLog)],
            )],
        );

        let entry = GuestLogEntry {
            process_id: Some(guest.process_id),
            level: "INFO".to_string(),
            target: "test".to_string(),
            message: "self log".to_string(),
        };
        let (status, _) =
            runtime.begin_hostcall(guest.process_id, HostcallRequest::GuestLogWrite { entry });
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    }

    #[test]
    fn child_grant_cannot_exceed_parent_authority() {
        let runtime = Runtime::default();
        runtime
            .register_module_bytes(
                "contain-child-module".to_string(),
                module_with_entrypoint("main", ""),
            )
            .expect("register child module");

        // Parent has tenant-scoped ProcessLifecycle.
        let parent = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "contain-parent".to_string(),
                module_id: "contain-parent-module".to_string(),
                module_bytes: module_with_entrypoint("boot", ""),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![
                    CapabilityGrant::new(
                        Capability::ProcessLifecycle,
                        vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                    ),
                    CapabilityGrant::new(
                        Capability::SharedMemory,
                        vec![
                            ResourceSelector::Tenant("acme".to_string()),
                            ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
                        ],
                    ),
                ],
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: Some("acme".to_string()),
                serving_role: None,
                handlers: Vec::new(),
            })
            .expect("spawn contain-parent");

        // Try to spawn a child with a Tenant("beta") grant that exceeds
        // the parent's Tenant("acme") authority.
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                module_id: "contain-child-module".to_string(),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![
                        ResourceSelector::Tenant("beta".to_string()),
                        ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
                    ],
                )],
                tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

        match runtime.poll_hostcall(parent.process_id, op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::PermissionDenied);
                assert!(
                    error.message.contains("exceeds parent authority")
                        || error.message.contains("child grant"),
                    "error should mention parent authority: {error:?}"
                );
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn tcp_connect_rejects_hostname_with_malformed_payload() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpStream)],
            )],
        );

        // "localhost:80" is a hostname, not an IP literal.
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::TcpConnect {
                address: "localhost:80".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::MalformedPayload);
                assert!(
                    error.message.contains("IP literal"),
                    "error should mention IP literal: {error:?}"
                );
            }
            other => panic!("expected failed hostcall, got {other:?}"),
        }
    }

    #[test]
    fn tcp_bind_rejects_hostname_with_malformed_payload() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpListener)],
            )],
        );

        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::TcpBind {
                address: "example.com:0".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::MalformedPayload);
            }
            other => panic!("expected failed hostcall, got {other:?}"),
        }
    }

    #[test]
    fn udp_bind_rejects_hostname_with_malformed_payload() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::UdpSocket)],
            )],
        );

        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::UdpBind {
                address: "myhost.local:8080".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::MalformedPayload);
            }
            other => panic!("expected failed hostcall, got {other:?}"),
        }
    }

    #[test]
    fn tcp_connect_accepts_ip_literal() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpStream)],
            )],
        );

        // "127.0.0.1:1" is a valid IP literal. The OS connect will fail
        // (connection refused), but it must NOT fail with MalformedPayload
        // (which would indicate our address check rejected it).
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::TcpConnect {
                address: "127.0.0.1:1".to_string(),
            },
        );
        // Accept either ready or failed — the important thing is it wasn't
        // rejected at the validation step.
        if status == selium_abi::HOSTCALL_STATUS_FAILED {
            match runtime.poll_hostcall(guest.process_id, op) {
                CompletionState::Failed(error) => {
                    assert_ne!(
                        error.code,
                        AbiErrorCode::MalformedPayload,
                        "IP literal must not be rejected as MalformedPayload"
                    );
                }
                other => panic!("expected failed, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn tcp_bind_succeeds_with_ip_literal() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpListener)],
            )],
        );

        // "127.0.0.1:0" is a valid IP literal and should bind successfully.
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::TcpBind {
                address: "127.0.0.1:0".to_string(),
            },
        );
        assert!(
            status == selium_abi::HOSTCALL_STATUS_READY
                || status == selium_abi::HOSTCALL_STATUS_PENDING,
            "expected ready or pending for valid TcpBind, got status {status}"
        );

        let output = ready(&runtime, guest.process_id, op);
        match output {
            HostcallOutput::HostQueue(descriptor) => {
                assert!(descriptor.shared_id > 0);
                assert!(descriptor.local_id > 0);
            }
            other => panic!("expected HostQueue output, got {other:?}"),
        }
    }

    #[test]
    fn record_registration_is_denied_for_non_discovery_callers() {
        // Only the discovery system guest may report registrations: the
        // record gates role-declared readiness, so a random guest forging
        // records would bypass the readiness gate.
        let runtime = Runtime::default();
        let guest = spawn_with_grants(&runtime, Vec::new());

        // Pretend a different process is the discovery guest.
        *runtime.discovery_process.lock() = Some(guest.process_id + 1);

        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::RecordRegistration {
                process_id: guest.process_id,
                uri: "sel:///dns/resolve".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::PermissionDenied);
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }
        // No registration was recorded for the forged report.
        assert!(!runtime.has_registration(guest.process_id, "sel:///dns/resolve"));
    }

    #[test]
    fn process_capability_is_denied_for_non_discovery_callers() {
        // Only the discovery system guest may probe another process's
        // grants: it uses the check to gate root-registration requests, and
        // exposing it broadly would leak grant state.
        let runtime = Runtime::default();
        let guest = spawn_with_grants(&runtime, Vec::new());

        *runtime.discovery_process.lock() = Some(guest.process_id + 1);

        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::ProcessCapability {
                process_id: guest.process_id,
                capability: Capability::SystemRegistration,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::PermissionDenied);
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }
    }
}
