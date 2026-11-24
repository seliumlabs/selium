use std::{
    collections::HashMap,
    io::Read,
    time::{Duration, Instant},
};

use selium_abi::{
    AbiError, AbiErrorCode, AttachDeclarationScope, Capability, CapabilityGrant, CompletionState,
    GuestLogEntry, HostcallOutput, HostcallRequest, ModuleSource, Namespace, OperationId,
    ProcessId, ResourceClass, ResourceIdentity, ResourceSelector, ScopeContext, SharedResourceId,
    TaskId, VolumeDescriptor,
};
use selium_service::{DiscoveryRequest, FlatMsg, log::LogRecord};
use wasmtiny::{RegionProt as WasmProt, runtime::SharedMemory};

use crate::{SystemGuestDescriptor, error::kernel_error, runtime::Runtime};

/// Maximum operation records one process may retain before back-pressuring new
/// hostcalls. A guest that never polls or drops completions cannot grow host
/// memory without bound; other processes retain their own independent budget.
pub(crate) const MAX_OPERATIONS_PER_PROCESS: usize = 1024;

mod network;
mod signing;
mod time;

#[derive(Debug, Clone)]
pub(crate) enum HostOperationState {
    Dispatching,
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

/// Whether one `DelegateGrants` grant admits delegation in `tenant`.
///
/// Every tenant-scoping selector in the grant intersects: `Namespace::Root` is
/// the wildcard and a `Tenant`/`Namespace::Tenant` narrows it. A grant carrying
/// both `Root` and `Tenant("acme")` admits only `acme`.
fn grant_delegates_tenant(grant: &CapabilityGrant, tenant: &str) -> bool {
    if grant.capability != Capability::DelegateGrants {
        return false;
    }
    let mut scoped = false;
    for selector in &grant.selectors {
        match selector {
            ResourceSelector::Tenant(name)
            | ResourceSelector::Namespace(Namespace::Tenant(name)) => {
                scoped = true;
                if name != tenant {
                    return false;
                }
            }
            ResourceSelector::Namespace(Namespace::Root) => scoped = true,
            _ => {}
        }
    }
    scoped
}

/// Whether any single grant (never a synthesis of several grants) admits
/// delegation in `tenant`.
fn grants_delegate_tenant(grants: &[CapabilityGrant], tenant: &str) -> bool {
    grants
        .iter()
        .any(|grant| grant_delegates_tenant(grant, tenant))
}

/// Whether a child grant is wholly within the parent's delegation authority.
fn child_grant_within_delegation(
    parent_grants: &[CapabilityGrant],
    child: &CapabilityGrant,
) -> bool {
    let mut child_tenants = child
        .selectors
        .iter()
        .filter_map(|selector| match selector {
            ResourceSelector::Tenant(name)
            | ResourceSelector::Namespace(Namespace::Tenant(name)) => Some(name.as_str()),
            _ => None,
        });
    let Some(first) = child_tenants.next() else {
        return false;
    };
    if child_tenants.any(|tenant| tenant != first) {
        return false;
    }
    parent_grants
        .iter()
        .any(|parent| grant_delegates_tenant(parent, first))
}

/// A quota reservation that releases its bytes on drop unless committed.
pub(crate) struct QuotaReservation {
    kernel: selium_kernel::Kernel,
    tenant: String,
    class: ResourceClass,
    amount: u64,
    committed: bool,
}

impl QuotaReservation {
    pub(crate) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for QuotaReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.kernel
                .quota()
                .release(&self.tenant, self.class.clone(), self.amount);
        }
    }
}

/// A region allocation reservation: quota, kernel region, and ownership
/// records, rewritten unless the allocation commits.
struct RegionReservation<'a> {
    runtime: &'a Runtime,
    process_id: ProcessId,
    quota: QuotaReservation,
    shared_id: Option<u64>,
    committed: bool,
}

impl RegionReservation<'_> {
    fn commit(mut self) {
        self.quota.commit();
        self.committed = true;
    }
}

impl Drop for RegionReservation<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(shared_id) = self.shared_id.take() {
            self.runtime
                .resource_tenants
                .lock()
                .remove(&(ResourceClass::SharedRegion, shared_id));
            let _reclaimed = self.runtime.release_shared_resource(
                self.process_id,
                &ResourceClass::SharedRegion,
                shared_id,
            );
            drop(
                self.runtime
                    .kernel
                    .memory()
                    .destroy_shared_region(shared_id),
            );
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
        // Reserve the bounded per-process slot before dispatch. Multithreaded
        // guests can enter concurrently, so checking only after dispatch would
        // both exceed the bound and perform side effects for a request whose
        // completion could not be recorded.
        let operation_id = {
            let mut operations = self.operations.lock();
            if operations
                .values()
                .filter(|operation| operation.process_id == process_id)
                .count()
                >= MAX_OPERATIONS_PER_PROCESS
            {
                return (selium_abi::HOSTCALL_STATUS_FAILED, 0);
            }
            let operation_id = self.next_operation_id(&operations);
            operations.insert(
                operation_id,
                HostOperation {
                    process_id,
                    task_id,
                    state: HostOperationState::Dispatching,
                },
            );
            operation_id
        };

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
            HostOperationState::Dispatching => selium_abi::HOSTCALL_STATUS_PENDING,
            HostOperationState::Ready(_) => selium_abi::HOSTCALL_STATUS_READY,
            HostOperationState::Failed(_) => selium_abi::HOSTCALL_STATUS_FAILED,
            HostOperationState::HostQueueRecvWait { .. } => selium_abi::HOSTCALL_STATUS_PENDING,
            HostOperationState::SleepWait { .. } => selium_abi::HOSTCALL_STATUS_PENDING,
        };
        {
            let mut operations = self.operations.lock();
            let Some(operation) = operations.get_mut(&operation_id) else {
                // Concurrent process teardown reclaimed the reservation. The
                // caller is exiting too, so no completion can be delivered.
                return (selium_abi::HOSTCALL_STATUS_DROPPED, 0);
            };
            operation.state = state.clone();
        }

        // Register the wait if the guest parked on a host-writable ring.
        // This is a fire-and-forget registration: the hostcall returns
        // Ready immediately; the wake comes later via mailbox.
        if let HostOperationState::Ready(_) = &state
            && let Some(tid) = task_id
            && let Some((region_id, generation)) = wait_register
        {
            self.register_wait(process_id, tid, region_id, generation);
        }

        // Hand the guest's sleep deadline to the runtime's timer driver, which
        // owns the deadline queue and delivers the completion wake through the
        // ordinary mailbox path. The driver runs on its own thread, so the
        // wake lands whether the caller is on a Tokio thread, a kernel poller
        // thread, or anywhere else — and cannot be dropped along with an
        // executor that happened to be borrowed here.
        if let HostOperationState::SleepWait { deadline } = state
            && let Some(tid) = task_id
        {
            crate::timer::TimerDriver::for_runtime(self).schedule(crate::timer::SleepRequest {
                deadline,
                operation_id,
                process_id,
                task_id: tid,
            });
        }

        (status, operation_id)
    }

    /// Registers a rejected hostcall as a failed operation.
    ///
    /// Create-time rejections (oversized envelope, invalid memory range,
    /// missing/incompatible ABI preamble, undecodable request) still return a
    /// failed status, but they also carry an operation id so the guest can
    /// poll the structured [`AbiError`] and drop it. Without this the guest
    /// sees only a status code and cannot report the host's diagnosis — for
    /// example which ABI versions disagreed.
    pub(crate) fn reject_hostcall(
        &self,
        process_id: ProcessId,
        error: AbiError,
    ) -> (u32, OperationId) {
        let mut operations = self.operations.lock();
        if operations
            .values()
            .filter(|operation| operation.process_id == process_id)
            .count()
            >= MAX_OPERATIONS_PER_PROCESS
        {
            return (selium_abi::HOSTCALL_STATUS_FAILED, 0);
        }
        let operation_id = self.next_operation_id(&operations);
        operations.insert(
            operation_id,
            HostOperation {
                process_id,
                task_id: None,
                state: HostOperationState::Failed(error),
            },
        );
        (selium_abi::HOSTCALL_STATUS_FAILED, operation_id)
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
            HostOperationState::Dispatching => CompletionState::Pending { operation_id },
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
        let Some(operation) = operations.get(&operation_id) else {
            return false;
        };
        if operation.process_id != process_id {
            return false;
        }
        let was_sleep = matches!(operation.state, HostOperationState::SleepWait { .. });
        operations.remove(&operation_id);
        drop(operations);
        if was_sleep && let Some(driver) = self.timer_driver.get() {
            driver.cancel_operation(operation_id);
        }
        true
    }

    /// Authorises a principal-provenance allocation for the supplied
    /// serving tenant and returns the principal tenant the resource is
    /// minted under.
    ///
    /// A serving tenant equal to the allocating process's own tenant (or
    /// absent) needs no extra authority. A root principal (no tenant) may
    /// mint for any tenant — connectors and other trusted edge
    /// infrastructure run as root. A tenant-scoped process may mint for
    /// another tenant only with a `DelegateGrants` grant scoped to that
    /// tenant.
    fn authorise_serving_tenant(
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
                    self.authorise_serving_tenant(process_id, serving_tenant.as_deref())?;

                let size_bytes = (pages as u64) * 65536; // WASM page size
                let size_u32 = u32::try_from(size_bytes).map_err(|_error| {
                    AbiError::new(AbiErrorCode::MalformedPayload, "region size exceeds u32")
                })?;

                // Quota primitives cap extent independently of grants: reserve
                // the byte size against the serving tenant's shared-memory
                // ceiling before any region is allocated.
                let quota =
                    self.reserve_quota(&principal, ResourceClass::SharedRegion, size_bytes)?;
                let mut reservation = RegionReservation {
                    runtime: self,
                    process_id,
                    quota,
                    shared_id: None,
                    committed: false,
                };

                // Allocate region in the shared registry (standalone, no guest mapping yet).
                let (shared_id, _len) = self
                    .kernel
                    .memory()
                    .allocate_shared_region(size_u32)
                    .map_err(kernel_error)?;
                reservation.shared_id = Some(shared_id);

                // Note: we do NOT auto-attach here. The allocating process must
                // call `AttachRegion` to map the region into its linear memory.
                // This avoids double-attachment when the caller also calls
                // AttachRegion with different protection/reader-slot parameters.
                let page_offset = 0;

                self.claim_shared_resource(process_id, ResourceClass::SharedRegion, shared_id);

                // Mint-time authority: record the principal tenant
                // synchronously, so admission and canonical publication never
                // depend on a projected registration.
                self.resource_tenants
                    .lock()
                    .insert((ResourceClass::SharedRegion, shared_id), principal.clone());

                // Record which ledger the reservation was charged against, so the
                // release path credits the serving tenant rather than
                // re-deriving one from the freeing process.
                if self
                    .faults
                    .region_allocation_failure
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        "injected region allocation failure",
                    ));
                }

                self.kernel.quota().note_reservation(
                    ResourceClass::SharedRegion,
                    shared_id,
                    &principal,
                    size_bytes,
                );
                reservation.commit();

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

                // Capture the region size before teardown so the quota
                // reservation made at AllocRegion can be released.
                let region_len = self
                    .kernel
                    .memory()
                    .shared_region_len(region_id)
                    .map_err(kernel_error)?;

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

                // Detach the *calling* process's recorded guest-memory
                // handles: a guest-side attach bypasses the module's own
                // bookkeeping, so the app-level detach above cannot decrement
                // its engine count. Other attachers keep the region alive (a
                // non-zero mapping count makes the destroy below fail, which
                // is the specified "other instances are attached" behaviour).
                self.detach_guest_region_mappings(Some(process_id), Some(region_id));

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
                // The resource is gone: its attachable-by-resolution
                // declaration no longer confers anything.
                self.attachable_by_resolution
                    .lock()
                    .remove(&(ResourceClass::SharedRegion, region_id));

                // Return the region's byte size to the ledger the reservation was charged
                // against. A cross-tenant mint is credited to the serving
                // tenant whose ceiling absorbed it; the quota ledger recorded
                // that at allocation, so no tenant side map is needed here.
                if !self
                    .kernel
                    .quota()
                    .release_reservation(ResourceClass::SharedRegion, region_id)
                {
                    // No reservation was recorded (the platform tenant holds no
                    // tracked counter): fall back to the caller's own tenant.
                    let tenant = self.process_tenant(process_id).unwrap_or_default();
                    self.kernel.quota().release(
                        &tenant,
                        ResourceClass::SharedRegion,
                        u64::from(region_len),
                    );
                }

                // Tier-1 discovery revocation by resource identity: the
                // registry recorded the region's tenant at registration and
                // revokes under it, so a cross-tenant mint is revoked under the
                // tenant it was minted for.
                let request = DiscoveryRequest::ResourceFreed {
                    class: ResourceClass::SharedRegion.uri_segment().to_string(),
                    id: region_id,
                };
                if let Err(error) = self.publish_discovery_event(request) {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        format!("discovery publish failed: {error}"),
                    ));
                }
                self.resource_tenants
                    .lock()
                    .remove(&(ResourceClass::SharedRegion, region_id));

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

                // Remember the guest-memory handle for this attachment: the
                // engine's attachment count is only released by an explicit
                // detach with the same memory, which teardown performs without
                // the guest instance.
                if let Some(memory) = &guest_memory {
                    self.record_guest_region_mapping(process_id, region_id, memory);
                }

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
            HostcallRequest::TcpBind { address } => self.hostcall_tcp_bind(process_id, address),
            HostcallRequest::TcpConnect { address } => {
                self.hostcall_tcp_connect(process_id, address)
            }
            HostcallRequest::UdpBind { address } => self.hostcall_udp_bind(process_id, address),
            HostcallRequest::VolumeOpen {
                name,
                serving_tenant,
            } => {
                // The volume name is the scope URI, so a prefix-scoped
                // `Storage` grant admits only the volumes under its prefix.
                self.require_with_uri(
                    process_id,
                    Capability::Storage,
                    ResourceClass::Volume,
                    None,
                    name.clone(),
                )?;
                // Principal provenance: the volume is minted under the serving
                // tenant, which may differ from the opening process's own
                // tenant only for a root principal or with tenant-scoped
                // delegation.
                let principal =
                    self.authorise_serving_tenant(process_id, serving_tenant.as_deref())?;
                let descriptor = self
                    .kernel
                    .volumes()
                    .open((!principal.is_empty()).then_some(principal.as_str()), &name)
                    .map_err(kernel_error)?;
                self.claim_local_handle(process_id, ResourceClass::Volume, descriptor.local_id);
                self.resource_tenants.lock().insert(
                    (ResourceClass::Volume, descriptor.local_id),
                    principal.clone(),
                );
                self.volume_tenants
                    .lock()
                    .insert((process_id, descriptor.local_id), principal);
                Ok(HostOperationState::Ready(HostcallOutput::Volume(
                    VolumeDescriptor {
                        local_id: descriptor.local_id,
                        len: descriptor.len,
                    },
                )))
            }
            HostcallRequest::VolumeAppend { local_id, bytes } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::Storage,
                    ResourceClass::Volume,
                    local_id,
                )?;
                // Durable bytes belong to the tenant the volume was minted
                // under: reserve the append's bytes against that tenant before
                // the host writes anything. A denial writes no bytes.
                let tenant = self.volume_serving_tenant(process_id, local_id);
                let appended = bytes.len() as u64;
                let mut quota = self.reserve_quota(&tenant, ResourceClass::Volume, appended)?;
                if self
                    .faults
                    .volume_append_failure
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        "injected volume append failure",
                    ));
                }
                let offset = self
                    .kernel
                    .volumes()
                    .append(local_id, &bytes)
                    .map_err(kernel_error)?;
                quota.commit();
                Ok(HostOperationState::Ready(HostcallOutput::U64(offset)))
            }
            HostcallRequest::VolumeReadAt {
                local_id,
                offset,
                len,
            } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::Storage,
                    ResourceClass::Volume,
                    local_id,
                )?;
                let bytes = self
                    .kernel
                    .volumes()
                    .read_at(local_id, offset, len as usize)
                    .map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::Bytes(bytes)))
            }
            HostcallRequest::VolumeLen { local_id } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::Storage,
                    ResourceClass::Volume,
                    local_id,
                )?;
                let len = self.kernel.volumes().len(local_id).map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::U64(len)))
            }
            HostcallRequest::VolumeList { serving_tenant } => {
                self.require(process_id, Capability::Storage, ResourceClass::Volume, None)?;
                let principal =
                    self.authorise_serving_tenant(process_id, serving_tenant.as_deref())?;
                let names = self
                    .kernel
                    .volumes()
                    .list((!principal.is_empty()).then_some(principal.as_str()))
                    .map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::VolumeNames(
                    names,
                )))
            }
            HostcallRequest::VolumeRemove {
                name,
                serving_tenant,
            } => {
                self.require(process_id, Capability::Storage, ResourceClass::Volume, None)?;
                let principal =
                    self.authorise_serving_tenant(process_id, serving_tenant.as_deref())?;
                let freed = self
                    .kernel
                    .volumes()
                    .remove((!principal.is_empty()).then_some(principal.as_str()), &name)
                    .map_err(kernel_error)?;
                // Removal releases the tenant's resident bytes: the volume no
                // longer exists, so its bytes leave the tenant's usage.
                self.kernel
                    .quota()
                    .release(&principal, ResourceClass::Volume, freed);
                Ok(HostOperationState::Ready(HostcallOutput::U64(freed)))
            }
            HostcallRequest::VolumeSync { local_id } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::Storage,
                    ResourceClass::Volume,
                    local_id,
                )?;
                if self
                    .faults
                    .volume_sync_failure
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        "injected volume sync failure",
                    ));
                }
                self.kernel.volumes().sync(local_id).map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::VolumeClose { local_id } => {
                self.ensure_local_handle_owner(
                    process_id,
                    Capability::Storage,
                    ResourceClass::Volume,
                    local_id,
                )?;
                self.kernel
                    .volumes()
                    .close(local_id)
                    .map_err(kernel_error)?;
                self.release_local_handle(process_id, &ResourceClass::Volume, local_id);
                self.volume_tenants.lock().remove(&(process_id, local_id));
                self.resource_tenants
                    .lock()
                    .remove(&(ResourceClass::Volume, local_id));
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::VolumeUsage { tenant } => {
                // The accounting guest is the sole reader: the reading is gated
                // by the same bootstrap-provisioned authority that authors
                // ceilings.
                self.require_capability(process_id, Capability::QuotaWrite)?;
                let usage = self
                    .kernel
                    .volumes()
                    .tenant_usage((!tenant.is_empty()).then_some(tenant.as_str()))
                    .map_err(kernel_error)?;
                Ok(HostOperationState::Ready(HostcallOutput::U64(usage)))
            }
            HostcallRequest::ProcessStart {
                source,
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
                let module_bytes = self.resolve_module_source(process_id, source)?;
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
                // No host-side module catalogue exists: the process label is
                // its entrypoint, informational only.
                let label = entrypoint.clone();
                let descriptor = SystemGuestDescriptor {
                    name: label.clone(),
                    module_id: label,
                    module_bytes,
                    entrypoint,
                    arguments,
                    grants,
                    tenant,
                    discovery_service: false,
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
                    self.authorise_serving_tenant(process_id, serving_tenant.as_deref())?;
                // Pipe quotas meter queued *items*, not queue count: a queue
                // costs nothing to create and each pending entry reserves one
                // slot against the queue owner's (serving) tenant — released
                // when the entry is received. See `reserve_queue_item`.
                let queues = self.kernel.queues();
                let memory = self.kernel.memory();
                let descriptor = queues.create_host_queue(&memory);
                self.claim_local_handle(process_id, ResourceClass::HostQueue, descriptor.local_id);
                self.claim_shared_resource(
                    process_id,
                    ResourceClass::HostQueue,
                    descriptor.shared_id,
                );
                self.resource_tenants.lock().insert(
                    (ResourceClass::HostQueue, descriptor.shared_id),
                    principal.clone(),
                );
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
                    .is_some_and(|auth| {
                        auth.resolved_resources
                            .contains(&(ResourceClass::HostQueue, shared_id))
                    });

                if !is_owner && !has_explicit_grant && !is_resolved {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "HostQueueAttach denied for shared_id {shared_id}: process {process_id} is not owner, lacks ExplicitResource grant, and was not authorised for the resource",
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
            HostcallRequest::AuthoriseAttach {
                resource_class,
                resource_id,
                client_process_id,
            } => {
                // Push handoff: the owner (or an explicit-capability holder)
                // names exactly one resource and one client process; the host
                // records the basis. No guest identity is privileged.
                self.require_attach_handoff_standing(process_id, &resource_class, resource_id)?;
                self.grant_attach_basis(client_process_id, &resource_class, resource_id)?;
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::DeclareAttachableByResolution {
                resource_class,
                resource_id,
                scope,
            } => {
                // Test seam: the failure-injection suite arms this to prove a
                // served resource whose attachability cannot be declared
                // publishes no route. Off unless explicitly armed.
                if self
                    .faults
                    .declare_attachable_failure
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        "attachability declaration refused (injected)".to_string(),
                    ));
                }
                // The owner declares the resource attachable by resolution;
                // the declaration's scope bounds who a later record admits.
                self.require_attach_handoff_standing(process_id, &resource_class, resource_id)?;
                self.attachable_by_resolution
                    .lock()
                    .insert((resource_class, resource_id), scope);
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::RecordResolvedResourceFor {
                resource_class,
                resource_id,
                client_process_id,
            } => {
                // Pull handoff: a resolve-recording is accepted only while the
                // resource carries a live attachable-by-resolution declaration
                // whose scope admits the client. An undeclared resource, or a
                // client outside the scope, is refused.
                let scope = self
                    .attachable_by_resolution
                    .lock()
                    .get(&(resource_class.clone(), resource_id))
                    .cloned()
                    .ok_or_else(|| {
                        AbiError::new(
                            AbiErrorCode::PermissionDenied,
                            format!(
                                "RecordResolvedResourceFor denied: {resource_class:?} {resource_id} has no attachable-by-resolution declaration",
                            ),
                        )
                    })?;
                let client_tenant = self.process_tenant(client_process_id);
                let admitted = match &scope {
                    AttachDeclarationScope::Any => true,
                    AttachDeclarationScope::Tenant(tenant) => {
                        client_tenant.as_deref() == Some(tenant.as_str())
                    }
                };
                if !admitted {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        format!(
                            "RecordResolvedResourceFor denied: process {client_process_id} is outside the declaration scope for {resource_class:?} {resource_id}",
                        ),
                    ));
                }
                self.grant_attach_basis(client_process_id, &resource_class, resource_id)?;
                Ok(HostOperationState::Ready(HostcallOutput::Empty))
            }
            HostcallRequest::DiscoveryListener => {
                self.require(
                    process_id,
                    Capability::HostQueue,
                    ResourceClass::HostQueue,
                    None,
                )?;
                let resource_id = self.discovery_listener_shared_id().ok_or_else(|| {
                    AbiError::new(
                        AbiErrorCode::NotFound,
                        "discovery listener is not currently available",
                    )
                })?;
                // This is the bootstrap form of resolution: the old discovery
                // service cannot answer a lookup for its own replacement. Keep
                // the same owner-declaration and tenant-scope enforcement as
                // RecordResolvedResourceFor before recording the attach basis.
                let scope = self
                    .attachable_by_resolution
                    .lock()
                    .get(&(ResourceClass::HostQueue, resource_id))
                    .cloned()
                    .ok_or_else(|| {
                        AbiError::new(
                            AbiErrorCode::NotFound,
                            "discovery listener has not declared resolution attachability",
                        )
                    })?;
                let tenant = self.process_tenant(process_id);
                let admitted = match &scope {
                    AttachDeclarationScope::Any => true,
                    AttachDeclarationScope::Tenant(expected) => {
                        tenant.as_deref() == Some(expected.as_str())
                    }
                };
                if !admitted {
                    return Err(AbiError::new(
                        AbiErrorCode::PermissionDenied,
                        "discovery listener resolution is outside the caller's tenant scope",
                    ));
                }
                self.grant_attach_basis(process_id, &ResourceClass::HostQueue, resource_id)?;
                Ok(HostOperationState::Ready(HostcallOutput::SharedId(
                    resource_id,
                )))
            }
            HostcallRequest::SignTenantCa { tenant } => {
                self.hostcall_sign_tenant_ca(process_id, tenant)
            }
            HostcallRequest::CommitTenantCa {
                tenant,
                ca_cert_der,
            } => self.hostcall_commit_tenant_ca(process_id, tenant, ca_cert_der),
            HostcallRequest::RollbackTenantCa {
                tenant,
                ca_cert_der,
            } => self.hostcall_rollback_tenant_ca(process_id, tenant, ca_cert_der),
            HostcallRequest::SignUserCert { tenant, spki_der } => {
                self.hostcall_sign_user_cert(process_id, tenant, spki_der)
            }
            HostcallRequest::RevokeCa { tenant } => self.hostcall_revoke_ca(process_id, tenant),
            HostcallRequest::SelfInfo => Ok(HostOperationState::Ready(HostcallOutput::SelfInfo {
                process_id,
                tenant: self.process_tenant(process_id),
            })),
            HostcallRequest::ExpectedClaims { process_id: target } => {
                // A guest may always see its own expectation; another
                // process's expectation is registry state and requires the
                // bootstrap-provisioned probe authority.
                if target != process_id {
                    self.require_capability(process_id, Capability::ProcessProbe)?;
                }
                Ok(HostOperationState::Ready(HostcallOutput::Claims(
                    self.expected_claims(target),
                )))
            }
            HostcallRequest::ProcessList => {
                // Process membership is registry state: only the
                // bootstrap-provisioned probe authority may enumerate it.
                self.require_capability(process_id, Capability::ProcessProbe)?;
                let mut processes: Vec<ProcessId> =
                    self.process_authorities.lock().keys().copied().collect();
                processes.sort_unstable();
                Ok(HostOperationState::Ready(HostcallOutput::Processes(
                    processes,
                )))
            }
            HostcallRequest::ProcessTenant { process_id } => Ok(HostOperationState::Ready(
                HostcallOutput::Tenant(self.process_tenant(process_id)),
            )),
            HostcallRequest::ProcessOwnsResource {
                process_id: owner,
                resource_class,
                resource_id,
            } => {
                // Test seam: the failure-injection suite arms this to prove
                // discovery denies a registration rather than falling back to
                // its projection. Off unless explicitly armed.
                if self
                    .faults
                    .authority_unavailable
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        "host authority unavailable (injected)".to_string(),
                    ));
                }
                // Ownership is read from the host's authoritative owner sets:
                // the shared-resource set (regions, queues, volumes) and the
                // process-local-handle set (the `Process` node a spawned
                // process owns as itself). The host records into both
                // synchronously as it mints a resource, so this answer cannot
                // lag a resource the caller has only just created.
                Ok(HostOperationState::Ready(
                    HostcallOutput::ResourceAuthority(self.resource_authority(
                        owner,
                        resource_class,
                        resource_id,
                    )),
                ))
            }
            HostcallRequest::ProcessCapability {
                process_id: target_process_id,
                capability,
                uri,
            } => {
                // Gated by the bootstrap-provisioned `ProcessProbe` authority
                // rather than by any guest identity: a registry implementation
                // uses the probe to gate a request against the caller's grants.
                self.require_capability(process_id, Capability::ProcessProbe)?;
                // Answer in the target's own operating scope: a tenant-scoped
                // grant is held *within that tenant*, so evaluating against an
                // empty context would report "not held" for a grant the target
                // genuinely holds. The probe still discloses nothing beyond
                // the capability asked for — only the target's own tenant and
                // the URI it asks about are used, and only as evaluation scope.
                let scope = ScopeContext {
                    tenant: self.process_tenant(target_process_id),
                    uri,
                    ..ScopeContext::default()
                };
                let held = self.authorises(target_process_id, capability, &scope);
                Ok(HostOperationState::Ready(HostcallOutput::U64(u64::from(
                    held,
                ))))
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
                let mut slot = self.reserve_queue_item(shared_id)?;
                if self
                    .faults
                    .queue_send_failure
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(AbiError::new(
                        AbiErrorCode::Internal,
                        "injected queue send failure",
                    ));
                }
                self.kernel
                    .queues()
                    .host_queue_send(local_id, process_id, value, metadata)
                    .map_err(kernel_error)?;
                if let Some(mut reservation) = slot.take() {
                    reservation.commit();
                }
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
            HostcallRequest::TimeNow => self.hostcall_time_now(process_id),
            HostcallRequest::TimeMonotonic => self.hostcall_time_monotonic(process_id),
            HostcallRequest::Sleep { millis } => self.hostcall_sleep(process_id, millis),
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

    /// Reserves quota and returns a guard that releases it unless committed.
    fn reserve_quota(
        &self,
        tenant: &str,
        class: ResourceClass,
        amount: u64,
    ) -> std::result::Result<QuotaReservation, AbiError> {
        self.enforce_quota(tenant, class.clone(), amount)?;
        Ok(QuotaReservation {
            kernel: self.kernel.clone(),
            tenant: tenant.to_string(),
            class,
            amount,
            committed: false,
        })
    }

    /// Returns the tenant whose pipe quota meters a queue: the tenant of the
    /// process that owns the queue. Items pending in a tenant's queues count
    /// against that tenant, whoever sent them.
    fn queue_owner_tenant(&self, shared_id: u64) -> Option<String> {
        let owner = self
            .shared_resource_owners
            .lock()
            .get(&(ResourceClass::HostQueue, shared_id))
            .and_then(|owners| owners.iter().next().copied())?;
        self.process_tenant(owner)
    }

    /// Reserves one pipe slot against the queue owner's (serving) tenant for
    /// an item enqueued onto `shared_id`, denying the enqueue when the
    /// tenant's ceiling is exhausted.
    pub(crate) fn reserve_queue_item(
        &self,
        shared_id: u64,
    ) -> std::result::Result<Option<QuotaReservation>, AbiError> {
        match self.queue_owner_tenant(shared_id) {
            Some(tenant) => self
                .reserve_quota(&tenant, ResourceClass::HostQueue, 1)
                .map(Some),
            None => Ok(None),
        }
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

    /// Requires that `process_id` may hand a shared resource off to a peer: it
    /// owns the resource, or it holds an `ExplicitResource` capability for it.
    /// No guest role or name confers this standing.
    fn require_attach_handoff_standing(
        &self,
        process_id: ProcessId,
        resource_class: &ResourceClass,
        resource_id: SharedResourceId,
    ) -> std::result::Result<(), AbiError> {
        let capability = match resource_class {
            ResourceClass::SharedRegion => Capability::SharedMemory,
            ResourceClass::HostQueue => Capability::HostQueue,
            other => {
                return Err(AbiError::new(
                    AbiErrorCode::MalformedPayload,
                    format!("resource class {other:?} is not a shared handoff resource"),
                ));
            }
        };
        let owns = self
            .shared_resource_owners
            .lock()
            .get(&(resource_class.clone(), resource_id))
            .is_some_and(|owners| owners.contains(&process_id));
        if owns {
            return Ok(());
        }
        let has_explicit = self
            .process_authorities
            .lock()
            .get(&process_id)
            .is_some_and(|auth| {
                auth.grants.iter().any(|grant| {
                    grant.capability == capability
                        && grant.selectors.iter().any(|sel| {
                            matches!(
                                sel,
                                ResourceSelector::ExplicitResource(ResourceIdentity::Shared(id))
                                    if *id == resource_id
                            )
                        })
                })
            });
        if has_explicit {
            return Ok(());
        }
        Err(AbiError::new(
            AbiErrorCode::PermissionDenied,
            format!(
                "handoff denied: process {process_id} neither owns {resource_class:?} {resource_id} nor holds an explicit capability for it",
            ),
        ))
    }

    /// Records `(resource_class, resource_id)` as an attach basis for
    /// `client_process_id`, failing when the client process is unknown.
    fn grant_attach_basis(
        &self,
        client_process_id: ProcessId,
        resource_class: &ResourceClass,
        resource_id: SharedResourceId,
    ) -> std::result::Result<(), AbiError> {
        let mut authorities = self.process_authorities.lock();
        let auth = authorities.get_mut(&client_process_id).ok_or_else(|| {
            AbiError::new(
                AbiErrorCode::InvalidHandle,
                format!("unknown client process {client_process_id}"),
            )
        })?;
        auth.resolved_resources
            .insert((resource_class.clone(), resource_id));
        Ok(())
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

        // Owner-scoped handoff basis: a process the region's owner authorised
        // (push) or that resolved the region under a live owner declaration
        // (pull) gained an attach basis for it.
        let was_authorised = self
            .process_authorities
            .lock()
            .get(&process_id)
            .is_some_and(|auth| {
                auth.resolved_resources
                    .contains(&(ResourceClass::SharedRegion, region_id))
            });
        if was_authorised {
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
    /// Cross-tenant handoffs move the quota reservation to the receiver under
    /// the receiver's tenant; the tier-1 registration is revoked by resource
    /// identity, so the registry revokes it under the tenant it recorded.
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

        // Move the quota reservation: release from the sender's tenant,
        // force-consume against the receiver's tenant (only when the tenants
        // differ — an intra-tenant handoff keeps the reservation put).
        let sender_tenant = self.process_tenant(sender_pid).unwrap_or_default();
        let receiver_tenant = self.process_tenant(receiver_pid).unwrap_or_default();
        if sender_tenant != receiver_tenant
            && let Ok(len) = self.kernel.memory().shared_region_len(region_id)
        {
            let len = u64::from(len);
            if !self.kernel.quota().transfer_reservation(
                ResourceClass::SharedRegion,
                region_id,
                &receiver_tenant,
                len,
            ) {
                // No reservation was recorded (the platform tenant holds no
                // tracked counter): move the counters directly.
                self.kernel
                    .quota()
                    .release(&sender_tenant, ResourceClass::SharedRegion, len);
                self.kernel.quota().force_consume(
                    &receiver_tenant,
                    ResourceClass::SharedRegion,
                    len,
                );
            }
        }

        // Transfer ownership: the sender's entry leaves its resource table,
        // the receiver's gains it.
        let mut shared_resource_owners = self.shared_resource_owners.lock();
        if let Some(owners) =
            shared_resource_owners.get_mut(&(ResourceClass::SharedRegion, region_id))
        {
            owners.remove(&sender_pid);
            owners.insert(receiver_pid);
        }
        drop(shared_resource_owners);

        // Remember the handoff: a sender that dies without a clean close
        // cannot withdraw its writer, so teardown marks the session's peer
        // departed for the receiver (see `cleanup_process_resources`).
        let mut sessions = self.handed_off_sessions.lock();
        let regions = sessions.entry(sender_pid).or_default();
        if !regions.contains(&region_id) {
            regions.push(region_id);
        }
    }

    /// Returns the serving tenant a local volume handle was minted under, so
    /// an append is reserved against that tenant rather than the writer's.
    /// Falls back to the calling process's own tenant for an unrecorded handle.
    fn volume_serving_tenant(&self, process_id: ProcessId, local_id: u64) -> String {
        self.volume_tenants
            .lock()
            .get(&(process_id, local_id))
            .cloned()
            .or_else(|| self.process_tenant(process_id))
            .unwrap_or_default()
    }

    /// Resolves a process's module source to bytes.
    ///
    /// Inline bytes are used as-is. A durable slice is read through the
    /// runtime's own volume layer under the caller's storage authority — the
    /// same rule as opening the volume — so a spawner cannot read a volume its
    /// storage authority does not cover, and the module bytes never round-trip
    /// through a guest. The volume name is passed as the scope URI so a
    /// prefix-scoped volume grant can be evaluated.
    fn resolve_module_source(
        &self,
        process_id: ProcessId,
        source: ModuleSource,
    ) -> std::result::Result<Vec<u8>, AbiError> {
        match source {
            ModuleSource::Inline(bytes) => Ok(bytes),
            ModuleSource::Slice {
                volume,
                offset,
                len,
            } => {
                self.require_with_uri(
                    process_id,
                    Capability::Storage,
                    ResourceClass::Volume,
                    None,
                    volume.clone(),
                )?;
                let tenant = self.process_tenant(process_id);
                let volumes = self.kernel.volumes();
                let descriptor = volumes.open(tenant.as_deref(), &volume).map_err(|error| {
                    AbiError::new(AbiErrorCode::PermissionDenied, error.to_string())
                })?;
                let bytes = volumes
                    .read_at(descriptor.local_id, offset, len as usize)
                    .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
                drop(volumes.close(descriptor.local_id));
                Ok(bytes)
            }
        }
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

        // Some capabilities are bootstrap-provisioned only: the host mints
        // them for the system guest that owns the resource (the bridge, the
        // identity guest, the accountant, the registry), and a spawn can never
        // confer them on a child. One vocabulary drives the check so a new
        // bootstrap-provisioned capability cannot be forgotten here.
        if let Some(grant) = grants
            .iter()
            .find(|grant| grant.capability.is_bootstrap_provisioned())
        {
            return Err(AbiError::new(
                AbiErrorCode::PermissionDenied,
                format!(
                    "{} cannot be conferred on child processes",
                    grant.capability.name()
                ),
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
        let delegating = grants
            .iter()
            .all(|grant| child_grant_within_delegation(&authority.grants, grant));

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
        if grants_delegate_tenant(&authority.grants, requested) {
            return Ok(Some(requested.to_string()));
        }
        Err(AbiError::new(
            AbiErrorCode::PermissionDenied,
            format!("spawn tenant {requested:?} requires an in-scope DelegateGrants grant"),
        ))
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
            match selector {
                ResourceSelector::UriPrefix(child_prefix) | ResourceSelector::Uri(child_prefix) => {
                    // Evaluate the parent with the same canonical, segment-aware
                    // matcher used at enforcement time, so delegation coverage
                    // cannot admit a child grant the parent would not match.
                    ResourceSelector::UriPrefix(parent_prefix.clone()).matches(&ScopeContext {
                        uri: Some(child_prefix.clone()),
                        ..ScopeContext::default()
                    })
                }
                _ => false,
            }
        }),
        ResourceSelector::Uri(parent_uri) => child.selectors.iter().any(|selector| {
            match selector {
                ResourceSelector::Uri(child_uri) => ResourceSelector::Uri(parent_uri.clone())
                    .matches(&ScopeContext {
                        uri: Some(child_uri.clone()),
                        ..ScopeContext::default()
                    }),
                _ => false,
            }
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
    use crate::{Runtime, SystemGuestDescriptor};
    use selium_abi::{GuestLogEntry, MeteringObservation, ResourceSelector};

    fn module_with_entrypoint(entrypoint: &str, body: &str) -> Vec<u8> {
        wat::parse_str(format!("(module (func (export \"{entrypoint}\") {body}))"))
            .expect("compile wat")
    }

    /// Allocates a shared region for `process_id`, returning its region id.
    fn allocate_region(runtime: &Runtime, process_id: ProcessId) -> SharedResourceId {
        let (status, op) = runtime.begin_hostcall(
            process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        match runtime.poll_hostcall(process_id, op) {
            CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) => alloc.region_id,
            other => panic!("expected RegionAlloc, got {other:?}"),
        }
    }

    /// A create-time rejection still names an operation, so the guest can poll
    /// the structured reason (and, for a version mismatch, both versions)
    /// instead of seeing only a failed status code.
    #[test]
    fn a_rejected_create_carries_its_structured_error() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(&runtime, Vec::new());
        let guest_version = selium_abi::SELIUM_ABI_VERSION + 1;
        let error = selium_abi::HostcallPreambleError::VersionMismatch {
            host: selium_abi::SELIUM_ABI_VERSION,
            guest: guest_version,
        }
        .to_abi_error();

        let (status, operation) = runtime.reject_hostcall(guest.process_id, error);
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        match runtime.poll_hostcall(guest.process_id, operation) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::MalformedPayload);
                assert!(
                    error
                        .message
                        .contains(&selium_abi::SELIUM_ABI_VERSION.to_string()),
                    "the host version is named: {}",
                    error.message
                );
                assert!(
                    error.message.contains(&guest_version.to_string()),
                    "the guest version is named: {}",
                    error.message
                );
            }
            other => panic!("expected a structured failure, got {other:?}"),
        }
        assert!(
            runtime.drop_hostcall(guest.process_id, operation),
            "the failed operation is reclaimed on drop"
        );
    }

    #[test]
    fn non_cooperative_rejections_cannot_exceed_the_process_operation_budget() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(&runtime, Vec::new());
        let error = AbiError::new(AbiErrorCode::MalformedPayload, "hostile request");

        for _ in 0..MAX_OPERATIONS_PER_PROCESS {
            let (status, operation_id) = runtime.reject_hostcall(guest.process_id, error.clone());
            assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
            assert_ne!(operation_id, 0);
        }
        assert_eq!(runtime.operations.lock().len(), MAX_OPERATIONS_PER_PROCESS);

        // The guest never polls or drops any completion. Further malformed
        // requests fail immediately without allocating another host record.
        for _ in 0..32 {
            assert_eq!(
                runtime.reject_hostcall(guest.process_id, error.clone()),
                (selium_abi::HOSTCALL_STATUS_FAILED, 0)
            );
        }
        assert_eq!(runtime.operations.lock().len(), MAX_OPERATIONS_PER_PROCESS);
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
                tenant: None,
                discovery_service: false,
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
                tenant: tenant.map(str::to_string),
                discovery_service: false,
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
                tenant: None,
                discovery_service: false,
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
    // scenario: selium-abi/volume-hostcall-variants/bytes-round-trip
    // scenario: selium-abi/volume-hostcall-variants/append-never-rewrites-earlier-bytes

    #[test]
    fn volume_hostcalls_cover_the_byte_container_lifecycle() {
        let runtime = Runtime::default();
        let bootstrapped = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            )],
        );

        let (_, open_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeOpen {
                name: "orders".to_string(),
                serving_tenant: None,
            },
        );
        let HostcallOutput::Volume(volume) = ready(&runtime, bootstrapped.process_id, open_op)
        else {
            panic!("expected volume descriptor");
        };
        assert_eq!(volume.len, 0);

        let (_, append_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeAppend {
                local_id: volume.local_id,
                bytes: b"hello".to_vec(),
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, append_op),
            HostcallOutput::U64(0)
        );
        let (_, second_append_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeAppend {
                local_id: volume.local_id,
                bytes: b" world".to_vec(),
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, second_append_op),
            HostcallOutput::U64(5)
        );

        let (_, read_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeReadAt {
                local_id: volume.local_id,
                offset: 6,
                len: 64,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, read_op),
            HostcallOutput::Bytes(b"world".to_vec())
        );

        let (_, len_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeLen {
                local_id: volume.local_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, len_op),
            HostcallOutput::U64(11)
        );

        let (_, list_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeList {
                serving_tenant: None,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, list_op),
            HostcallOutput::VolumeNames(vec!["orders".to_string()])
        );

        let (_, sync_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeSync {
                local_id: volume.local_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, sync_op),
            HostcallOutput::Empty
        );

        let (_, close_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeClose {
                local_id: volume.local_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, close_op),
            HostcallOutput::Empty
        );

        let (_, remove_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeRemove {
                name: "orders".to_string(),
                serving_tenant: None,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, remove_op),
            HostcallOutput::U64(11)
        );
    }

    // scenario: capability-enforcement/volume-hostcall-authorisation/capability-required
    #[test]
    fn volume_open_requires_the_storage_capability() {
        let runtime = Runtime::default();
        let bootstrapped = spawn_with_grants(&runtime, Vec::new());
        let (status, op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeOpen {
                name: "orders".to_string(),
                serving_tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        let CompletionState::Failed(error) = runtime.poll_hostcall(bootstrapped.process_id, op)
        else {
            panic!("expected permission denial");
        };
        assert_eq!(error.code, AbiErrorCode::PermissionDenied);
    }

    // scenario: capability-enforcement/volume-hostcall-authorisation/cross-tenant-open-denied
    #[test]
    fn volume_open_for_a_foreign_tenant_requires_delegation() {
        let runtime = Runtime::default();
        let bootstrapped = spawn_with_grants_and_tenant(
            &runtime,
            "acme-worker",
            vec![CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            )],
            Some("acme"),
        );
        let (status, op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeOpen {
                name: "orders".to_string(),
                serving_tenant: Some("beta".to_string()),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        let CompletionState::Failed(error) = runtime.poll_hostcall(bootstrapped.process_id, op)
        else {
            panic!("expected cross-tenant denial");
        };
        assert_eq!(error.code, AbiErrorCode::PermissionDenied);
    }

    // scenario: selium-runtime/principal-provenance-allocation/volume-minted-under-the-serving-tenant
    // scenario: capability-enforcement/volume-hostcall-authorisation/root-serves-a-tenant
    #[test]
    fn root_principal_opens_a_volume_under_a_named_tenant() {
        let runtime = Runtime::default();
        let bootstrapped = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            )],
        );
        let (_, open_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeOpen {
                name: "orders".to_string(),
                serving_tenant: Some("acme".to_string()),
            },
        );
        let HostcallOutput::Volume(volume) = ready(&runtime, bootstrapped.process_id, open_op)
        else {
            panic!("expected volume descriptor");
        };
        let (_, append_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeAppend {
                local_id: volume.local_id,
                bytes: b"acme-bytes".to_vec(),
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, append_op),
            HostcallOutput::U64(0)
        );
        // The bytes are recorded under `acme`, not the root namespace.
        assert_eq!(
            runtime
                .kernel
                .volumes()
                .tenant_usage(Some("acme"))
                .expect("acme usage"),
            10
        );
        assert_eq!(
            runtime
                .kernel
                .volumes()
                .tenant_usage(None)
                .expect("root usage"),
            0
        );
    }

    /// `UriPrefix` scopes a `Storage` grant over volume *names*: a prefix-scoped
    /// grant admits the volumes under it and refuses the rest, both when opening a
    /// volume and when spawning from a durable slice on one.
    // scenario: capability-enforcement/grant-time-evaluatable-honesty-for-uriprefix/volume-name-prefix-admitted
    #[test]
    fn a_volume_name_prefix_scopes_the_storage_grant_at_use_time() {
        let runtime = Runtime::default();
        let bootstrapped = spawn_with_grants_and_tenant(
            &runtime,
            "prefix-store",
            vec![CapabilityGrant::new(
                Capability::Storage,
                vec![
                    ResourceSelector::ResourceClass(ResourceClass::Volume),
                    ResourceSelector::UriPrefix("selium.control-plane".to_string()),
                ],
            )],
            Some("acme"),
        );
        let process_id = bootstrapped.process_id;

        // A volume under the prefix is admitted.
        let (status, open_op) = runtime.begin_hostcall(
            process_id,
            HostcallRequest::VolumeOpen {
                name: "selium.control-plane.modules".to_string(),
                serving_tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        ready(&runtime, process_id, open_op);

        // A volume outside it is refused.
        let (status, _op) = runtime.begin_hostcall(
            process_id,
            HostcallRequest::VolumeOpen {
                name: "acme.private".to_string(),
                serving_tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "a volume outside the granted prefix is denied"
        );

        // The same prefix bounds a module slice read at spawn.
        let spawner = spawn_with_grants_and_tenant(
            &runtime,
            "prefix-spawner",
            vec![
                CapabilityGrant::new(
                    Capability::Storage,
                    vec![
                        ResourceSelector::ResourceClass(ResourceClass::Volume),
                        ResourceSelector::UriPrefix("selium.control-plane".to_string()),
                    ],
                ),
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
            ],
            Some("acme"),
        );
        let (status, _op) = runtime.begin_hostcall(
            spawner.process_id,
            HostcallRequest::ProcessStart {
                source: selium_abi::ModuleSource::Slice {
                    volume: "acme.private".to_string(),
                    offset: 0,
                    len: 16,
                },
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "a slice on a volume outside the granted prefix is denied"
        );
    }

    // scenario: selium-runtime/volume-hostcall-enforcement/append-beyond-the-ceiling-denied
    // scenario: selium-accountant/quota-enforcement-at-allocation/append-over-volume-quota-denied
    #[test]
    fn append_beyond_the_volume_ceiling_writes_no_bytes() {
        let runtime = Runtime::default();
        runtime.kernel.quota().set("acme", ResourceClass::Volume, 4);

        let bootstrapped = spawn_with_grants_and_tenant(
            &runtime,
            "acme-store",
            vec![CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            )],
            Some("acme"),
        );
        let (_, open_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeOpen {
                name: "orders".to_string(),
                serving_tenant: None,
            },
        );
        let HostcallOutput::Volume(volume) = ready(&runtime, bootstrapped.process_id, open_op)
        else {
            panic!("expected volume descriptor");
        };

        let (status, op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeAppend {
                local_id: volume.local_id,
                bytes: b"oversized".to_vec(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        let CompletionState::Failed(error) = runtime.poll_hostcall(bootstrapped.process_id, op)
        else {
            panic!("expected quota denial");
        };
        assert_eq!(error.code, AbiErrorCode::QuotaExceeded);
        assert!(
            error.message.contains("acme"),
            "names the tenant: {}",
            error.message
        );
        assert!(
            error.message.contains("Volume"),
            "names the dimension: {}",
            error.message
        );

        // No bytes were written: the volume stays empty and no usage is booked.
        let (_, len_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeLen {
                local_id: volume.local_id,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, len_op),
            HostcallOutput::U64(0)
        );
        assert_eq!(
            runtime.kernel.quota().used("acme", ResourceClass::Volume),
            0
        );
    }
    // scenario: selium-abi/volume-resource-class/volume-is-the-storage-quota-dimension
    // scenario: selium-abi/tenant-volume-usage-hostcall/removal-releases-usage

    // scenario: selium-runtime/volume-hostcall-enforcement/removal-releases-usage
    #[test]
    fn removing_a_volume_releases_its_usage() {
        let runtime = Runtime::default();
        let bootstrapped = spawn_with_grants_and_tenant(
            &runtime,
            "acme-store",
            vec![CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            )],
            Some("acme"),
        );
        let (_, open_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeOpen {
                name: "orders".to_string(),
                serving_tenant: None,
            },
        );
        let HostcallOutput::Volume(volume) = ready(&runtime, bootstrapped.process_id, open_op)
        else {
            panic!("expected volume descriptor");
        };
        let (_, append_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeAppend {
                local_id: volume.local_id,
                bytes: b"bytes".to_vec(),
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, append_op),
            HostcallOutput::U64(0)
        );
        assert_eq!(
            runtime.kernel.quota().used("acme", ResourceClass::Volume),
            5
        );

        let (_, remove_op) = runtime.begin_hostcall(
            bootstrapped.process_id,
            HostcallRequest::VolumeRemove {
                name: "orders".to_string(),
                serving_tenant: None,
            },
        );
        assert_eq!(
            ready(&runtime, bootstrapped.process_id, remove_op),
            HostcallOutput::U64(5)
        );
        assert_eq!(
            runtime.kernel.quota().used("acme", ResourceClass::Volume),
            0
        );
    }

    #[test]
    fn process_activity_metering_and_guest_log_hostcalls_work() {
        let runtime = Runtime::default();
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: selium-runtime/guestlogregister-hostcall-validation/guestlogregister-accepted-for-own-region
    // scenario: selium-runtime/guestlogregister-hostcall-validation/guestlogregister-rejected-for-foreign-region
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

    // scenario: selium-runtime/grant-admission-and-evaluation/errors-attribute-correctly
    // scenario: capability-enforcement/evaluatable-scope-contexts/tenant-isolation-enforces
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
                tenant: Some("acme".to_string()),
                discovery_service: false,
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
                tenant: Some("beta".to_string()),
                discovery_service: false,
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

    // scenario: capability-enforcement/evaluatable-scope-contexts/selector-admission-at-grant-time
    // scenario: capability-enforcement/grant-time-evaluatable-honesty-for-uriprefix/uriprefix-on-non-network-class-rejected
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
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        });

        let error = result.expect_err("should reject UriPrefix grant at spawn");
        assert!(
            error.to_string().contains("UriPrefix"),
            "error should name the rejected selector: {error}"
        );
    }

    // scenario: selium-runtime/grant-admission-and-evaluation/accept-then-deny-is-impossible
    #[test]
    fn accept_then_deny_trap_is_impossible() {
        let runtime = Runtime::default();
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: capability-enforcement/scoped-grant-delegation/delegator-spawns-within-tenant
    #[test]
    fn delegator_within_tenant_spawns_child_with_extra_grants() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: capability-enforcement/scoped-grant-delegation/non-delegator-out-of-scope-child-denied
    #[test]
    fn non_delegator_out_of_scope_child_denied() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: capability-enforcement/scoped-grant-delegation/delegation-outside-tenant-denied
    #[test]
    fn delegation_outside_tenant_denied() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: capability-enforcement/scoped-grant-delegation/unscoped-grant-not-conferable-under-delegation
    #[test]
    fn delegation_denies_unscoped_child_grant() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
    // scenario: capability-enforcement/scoped-grant-delegation/root-delegator-spawns-for-any-tenant
    #[test]
    fn root_delegator_spawns_tenant_scoped_child() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: capability-enforcement/delegategrants-is-not-conferable/re-delegation-denied
    #[test]
    fn delegate_grants_cannot_be_conferred_on_children() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: route-claims/host-issued-route-claims/claim-is-not-delegable
    // scenario: capability-enforcement/routeclaim-is-not-conferable/child-cannot-receive-a-route-claim
    #[test]
    fn route_claim_cannot_be_conferred_on_children() {
        let runtime = Runtime::default();
        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "claim-holder",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::RouteClaim,
                    vec![ResourceSelector::Uri("sel:///control".to_string())],
                ),
            ],
            None,
        );

        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::RouteClaim,
                    vec![ResourceSelector::Uri("sel:///control".to_string())],
                )],
                tenant: None,
            },
        );
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "RouteClaim is non-delegable"
        );
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    // scenario: route-claims/host-issued-route-claims/exact-match-only
    // scenario: capability-enforcement/routeclaim-is-not-conferable/bootstrap-claim-admitted
    #[test]
    fn route_claim_requires_one_exact_root_uri() {
        let runtime = Runtime::default();
        let descriptor = |capability: CapabilityGrant| SystemGuestDescriptor {
            name: "claim-guest".to_string(),
            module_id: "claim-guest-module".to_string(),
            module_bytes: module_with_entrypoint("boot", ""),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: vec![capability],
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        };

        // Selector-less: vacuously claims every name.
        let selectorless = runtime.spawn_system_guest(descriptor(CapabilityGrant::new(
            Capability::RouteClaim,
            Vec::new(),
        )));
        assert!(selectorless.is_err(), "a selector-less claim is invalid");

        // Tenant-scoped: claims are root well-known names only.
        let tenant_scoped = runtime.spawn_system_guest(descriptor(CapabilityGrant::new(
            Capability::RouteClaim,
            vec![ResourceSelector::Uri("sel://acme/control".to_string())],
        )));
        assert!(
            tenant_scoped.is_err(),
            "a claim outside the root namespace is invalid"
        );

        // One exact root URI: admitted.
        let admitted = runtime.spawn_system_guest(descriptor(CapabilityGrant::new(
            Capability::RouteClaim,
            vec![ResourceSelector::Uri("sel:///control".to_string())],
        )));
        admitted.expect("bootstrap claim admitted");
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

    // scenario: capability-enforcement/mintcertificate-is-not-conferable/providing-mintcertificate-at-spawn-is-denied
    #[test]
    fn mint_certificate_cannot_be_conferred_on_children() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: capability-enforcement/quotawrite-is-not-conferable/conferring-quotawrite-at-spawn-is-denied
    #[test]
    fn quota_write_cannot_be_conferred_on_children() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: selium-abi/quotawrite-capability-variant/quota-hostcall-requires-the-capability
    // scenario: selium-accountant/sole-quota-authorship/non-accountant-quota-write-denied
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

    // scenario: selium-accountant/quota-enforcement-at-allocation/allocation-over-quota-denied
    // scenario: selium-accountant/quota-enforcement-at-allocation/quota-distinct-from-grant
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
    // scenario: selium-accountant/pipe-quotas-meter-queued-items/over-ceiling-send-denied
    // scenario: selium-accountant/pipe-quotas-meter-queued-items/receiving-frees-the-slot
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
    // scenario: guest-shared-memory/mapping-release-on-process-teardown/guest-side-attachment-is-released-at-teardown
    // scenario: selium-accountant/quota-enforcement-at-allocation/a-dying-process-releases-its-reservations
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
    // scenario: capability-enforcement/ownership-checked-attach/queue-handoff-shares-ownership
    // scenario: selium-accountant/handed-off-resources-transfer-ownership/cross-tenant-handoff-moves-the-reservation
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

    // scenario: capability-enforcement/operation-target-certificate-authorisation/tenant-scoped-mint-cannot-cross-tenants
    // scenario: capability-enforcement/operation-target-certificate-authorisation/authorised-target-succeeds
    #[test]
    fn tenant_scoped_mint_cannot_cross_tenants() {
        use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};

        let runtime = Runtime::default();
        runtime.generate_keyring().expect("generate keyring");
        let root = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::MintCertificate,
                Vec::new(),
            )],
        );
        let (_, op) = runtime.begin_hostcall(
            root.process_id,
            HostcallRequest::SignTenantCa {
                tenant: "beta".to_string(),
            },
        );
        assert!(matches!(
            ready(&runtime, root.process_id, op),
            HostcallOutput::Certificate(_)
        ));

        let acme = spawn_with_grants_and_tenant(
            &runtime,
            "acme-mint",
            vec![CapabilityGrant::new(
                Capability::MintCertificate,
                vec![ResourceSelector::Tenant("acme".to_string())],
            )],
            Some("acme"),
        );
        let client_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("client key");
        let spki = client_key.subject_public_key_info();
        for request in [
            HostcallRequest::SignTenantCa {
                tenant: "beta".to_string(),
            },
            HostcallRequest::SignUserCert {
                tenant: "beta".to_string(),
                spki_der: spki.clone(),
            },
            HostcallRequest::RevokeCa {
                tenant: "beta".to_string(),
            },
        ] {
            let (status, op) = runtime.begin_hostcall(acme.process_id, request);
            assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
            assert!(matches!(
                runtime.poll_hostcall(acme.process_id, op),
                CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
            ));
        }
        let (_, op) = runtime.begin_hostcall(
            root.process_id,
            HostcallRequest::SignUserCert {
                tenant: "beta".to_string(),
                spki_der: spki,
            },
        );
        assert!(matches!(
            ready(&runtime, root.process_id, op),
            HostcallOutput::Certificate(_)
        ));
        let (_, op) = runtime.begin_hostcall(
            acme.process_id,
            HostcallRequest::SignTenantCa {
                tenant: "acme".to_string(),
            },
        );
        assert!(matches!(
            ready(&runtime, acme.process_id, op),
            HostcallOutput::Certificate(_)
        ));
    }

    // scenario: capability-enforcement/intersection-preserving-delegation/root-and-tenant-selectors-remain-conjunctive
    #[test]
    fn root_plus_tenant_selectors_stay_conjunctive() {
        let runtime = Runtime::default();
        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "conjunctive-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![
                        ResourceSelector::Namespace(Namespace::Root),
                        ResourceSelector::Tenant("acme".to_string()),
                    ],
                ),
            ],
            None,
        );
        let spawn = |tenant: &str| {
            runtime.begin_hostcall(
                parent.process_id,
                HostcallRequest::ProcessStart {
                    source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
                    entrypoint: "main".to_string(),
                    arguments: Vec::new(),
                    grants: Vec::new(),
                    tenant: Some(tenant.to_string()),
                },
            )
        };
        let (status, op) = spawn("acme");
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        drop(runtime.poll_hostcall(parent.process_id, op));
        let (status, op) = spawn("beta");
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    // scenario: capability-enforcement/intersection-preserving-delegation/separate-grants-do-not-merge-authority
    #[test]
    fn separate_delegation_grants_do_not_merge_scope() {
        let runtime = Runtime::default();
        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "split-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![
                        ResourceSelector::Namespace(Namespace::Root),
                        ResourceSelector::Tenant("acme".to_string()),
                    ],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![ResourceSelector::Tenant("beta".to_string())],
                ),
            ],
            None,
        );
        let spawn = |tenant: &str| {
            runtime.begin_hostcall(
                parent.process_id,
                HostcallRequest::ProcessStart {
                    source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
                    entrypoint: "main".to_string(),
                    arguments: Vec::new(),
                    grants: Vec::new(),
                    tenant: Some(tenant.to_string()),
                },
            )
        };
        for tenant in ["acme", "beta"] {
            let (status, op) = spawn(tenant);
            assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
            drop(runtime.poll_hostcall(parent.process_id, op));
        }
        let (status, op) = spawn("gamma");
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    #[test]
    fn child_grant_must_fit_one_parent_delegation_grant() {
        let runtime = Runtime::default();
        let parent = spawn_with_grants_and_tenant(
            &runtime,
            "fenced-bridge",
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::DelegateGrants,
                    vec![
                        ResourceSelector::Namespace(Namespace::Root),
                        ResourceSelector::Tenant("acme".to_string()),
                    ],
                ),
            ],
            None,
        );
        let spawn = |tenant: &str| {
            runtime.begin_hostcall(
                parent.process_id,
                HostcallRequest::ProcessStart {
                    source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
                    entrypoint: "main".to_string(),
                    arguments: Vec::new(),
                    grants: vec![CapabilityGrant::new(
                        Capability::Network,
                        vec![
                            ResourceSelector::Tenant(tenant.to_string()),
                            ResourceSelector::ResourceClass(ResourceClass::TcpStream),
                        ],
                    )],
                    tenant: None,
                },
            )
        };
        let (status, op) = spawn("beta");
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(parent.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
        let (status, op) = spawn("acme");
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        drop(runtime.poll_hostcall(parent.process_id, op));
    }

    #[test]
    fn repeated_denied_hostcalls_are_reclaimed_after_poll_and_drop() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );
        let baseline = runtime.operations.lock().len();
        let cases = [
            (
                HostcallRequest::VolumeOpen {
                    name: "orders".to_string(),
                    serving_tenant: None,
                },
                AbiErrorCode::PermissionDenied,
            ),
            (
                HostcallRequest::RandomBytes { len: u32::MAX },
                AbiErrorCode::MalformedPayload,
            ),
        ];
        for _ in 0..32 {
            for (request, expected) in &cases {
                let (status, operation) = runtime.begin_hostcall(guest.process_id, request.clone());
                assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
                assert!(matches!(
                    runtime.poll_hostcall(guest.process_id, operation),
                    CompletionState::Failed(error) if error.code == *expected
                ));
                assert!(runtime.drop_hostcall(guest.process_id, operation));
            }
        }
        assert_eq!(runtime.operations.lock().len(), baseline);
    }

    // scenario: selium-runtime/failed-resource-operations-roll-back-reservations/region-allocation-fails-after-reservation
    #[test]
    fn injected_region_failure_rolls_back_quota_and_ownership() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants_and_tenant(
            &runtime,
            "rollback-region",
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
            Some("acme"),
        );
        let quota = runtime.kernel().quota();
        quota.set("acme", ResourceClass::SharedRegion, 1_000_000);
        let baseline_used = quota.used("acme", ResourceClass::SharedRegion);
        let baseline_regions = runtime.kernel().memory().live_shared_region_count();

        runtime.inject_region_allocation_failure(true);
        let (status, operation) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, operation),
            CompletionState::Failed(error) if error.code == AbiErrorCode::Internal
        ));
        assert_eq!(
            quota.used("acme", ResourceClass::SharedRegion),
            baseline_used
        );
        assert_eq!(
            runtime.kernel().memory().live_shared_region_count(),
            baseline_regions
        );
        assert!(
            !runtime
                .shared_resource_owners
                .lock()
                .keys()
                .any(|(class, _)| class == &ResourceClass::SharedRegion)
        );
        assert!(
            !runtime
                .resource_tenants
                .lock()
                .keys()
                .any(|(class, _)| class == &ResourceClass::SharedRegion)
        );

        runtime.inject_region_allocation_failure(false);
        let (status, operation) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::AllocRegion {
                pages: 1,
                prot: selium_abi::RegionProt::ReadWrite,
                purpose: selium_abi::ResourceKind::SharedMemory,
                serving_tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, operation),
            CompletionState::Ready(HostcallOutput::RegionAlloc(_))
        ));
        assert_eq!(
            quota.used("acme", ResourceClass::SharedRegion),
            baseline_used + 65_536
        );
    }

    // scenario: selium-runtime/failed-resource-operations-roll-back-reservations/append-or-queue-send-fails
    #[test]
    fn injected_append_failure_rolls_back_volume_quota_without_writing() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants_and_tenant(
            &runtime,
            "rollback-append",
            vec![CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            )],
            Some("acme"),
        );
        let quota = runtime.kernel().quota();
        quota.set("acme", ResourceClass::Volume, 1_000_000);
        let (status, open) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::VolumeOpen {
                name: "orders".to_string(),
                serving_tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let HostcallOutput::Volume(volume) = ready(&runtime, guest.process_id, open) else {
            panic!("expected a volume descriptor");
        };
        let baseline_used = quota.used("acme", ResourceClass::Volume);
        let baseline_len = runtime
            .kernel()
            .volumes()
            .len(volume.local_id)
            .expect("len");

        runtime.inject_volume_append_failure(true);
        let (status, operation) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::VolumeAppend {
                local_id: volume.local_id,
                bytes: b"data".to_vec(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, operation),
            CompletionState::Failed(error) if error.code == AbiErrorCode::Internal
        ));
        assert_eq!(quota.used("acme", ResourceClass::Volume), baseline_used);
        assert_eq!(
            runtime
                .kernel()
                .volumes()
                .len(volume.local_id)
                .expect("len"),
            baseline_len
        );

        runtime.inject_volume_append_failure(false);
        let (status, operation) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::VolumeAppend {
                local_id: volume.local_id,
                bytes: b"data".to_vec(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            ready(&runtime, guest.process_id, operation),
            HostcallOutput::U64(_)
        ));
        assert_eq!(quota.used("acme", ResourceClass::Volume), baseline_used + 4);
    }

    #[test]
    fn injected_queue_send_failure_rolls_back_the_pipe_slot() {
        let runtime = Runtime::default();
        let owner = spawn_with_grants_and_tenant(
            &runtime,
            "rollback-queue",
            vec![CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            )],
            Some("acme"),
        );
        let quota = runtime.kernel().quota();
        quota.set("acme", ResourceClass::HostQueue, 10);
        let (status, create) = runtime.begin_hostcall(
            owner.process_id,
            HostcallRequest::HostQueueCreate {
                serving_tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let HostcallOutput::HostQueue(queue) = ready(&runtime, owner.process_id, create) else {
            panic!("expected a queue descriptor");
        };
        let baseline_used = quota.used("acme", ResourceClass::HostQueue);
        runtime.inject_queue_send_failure(true);
        let (status, operation) = runtime.begin_hostcall(
            owner.process_id,
            HostcallRequest::HostQueueSend {
                local_id: queue.local_id,
                value: 1,
                metadata: Vec::new(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(owner.process_id, operation),
            CompletionState::Failed(error) if error.code == AbiErrorCode::Internal
        ));
        assert_eq!(quota.used("acme", ResourceClass::HostQueue), baseline_used);
        assert_eq!(
            runtime
                .kernel()
                .queues()
                .host_queue_pending(queue.local_id)
                .expect("pending"),
            0
        );

        runtime.inject_queue_send_failure(false);
        let (status, _operation) = runtime.begin_hostcall(
            owner.process_id,
            HostcallRequest::HostQueueSend {
                local_id: queue.local_id,
                value: 1,
                metadata: Vec::new(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert_eq!(
            quota.used("acme", ResourceClass::HostQueue),
            baseline_used + 1
        );
    }

    // scenario: selium-runtime/dedicated-timer-driver/a-cancelled-sleep-is-not-woken
    #[test]
    fn dropping_a_pending_sleep_releases_its_timer() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );
        let baseline = runtime.operations.lock().len();
        let (status, operation) = runtime.begin_hostcall_with_task(
            guest.process_id,
            HostcallRequest::Sleep { millis: 3_600_000 },
            Some(1),
            None,
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_PENDING);
        assert_eq!(runtime.pending_timer_count(), 1);
        assert!(runtime.drop_hostcall(guest.process_id, operation));
        assert_eq!(runtime.pending_timer_count(), 0);
        assert_eq!(runtime.operations.lock().len(), baseline);
    }

    #[test]
    fn stopping_a_process_releases_its_pending_sleep() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );
        let (status, _operation) = runtime.begin_hostcall_with_task(
            guest.process_id,
            HostcallRequest::Sleep { millis: 3_600_000 },
            Some(1),
            None,
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_PENDING);
        assert_eq!(runtime.pending_timer_count(), 1);
        runtime
            .stop_process(guest.process_id)
            .expect("process teardown");
        assert_eq!(runtime.pending_timer_count(), 0);
    }

    #[test]
    fn failed_guest_attachment_leaves_no_mapping_and_teardown_reclaims_it() {
        let runtime = Runtime::default();
        let owner = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );
        let consumer = runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: "attachment-consumer".to_string(),
                module_id: "attachment-consumer-module".to_string(),
                module_bytes: wat::parse_str(r#"(module (memory 1) (func (export "boot")))"#)
                    .expect("compile consumer module"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                )],
                tenant: None,
                discovery_service: false,
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
        let baseline_mappings = runtime.kernel().memory().live_shared_mapping_count();
        let (status, _) = runtime.begin_hostcall(
            consumer.process_id,
            HostcallRequest::AttachRegion {
                region_id: alloc.region_id,
                reader_slot: None,
                prot: selium_abi::RegionProt::ReadWrite,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert_eq!(
            runtime.kernel().memory().live_shared_mapping_count(),
            baseline_mappings
        );
        assert!(
            !runtime
                .guest_region_memories
                .lock()
                .contains_key(&(consumer.process_id, alloc.region_id))
        );

        let (status, _) = runtime.begin_hostcall(
            owner.process_id,
            HostcallRequest::DeclareAttachableByResolution {
                resource_class: ResourceClass::SharedRegion,
                resource_id: alloc.region_id,
                scope: selium_abi::AttachDeclarationScope::Any,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let (status, _) = runtime.begin_hostcall(
            consumer.process_id,
            HostcallRequest::RecordResolvedResourceFor {
                resource_class: ResourceClass::SharedRegion,
                resource_id: alloc.region_id,
                client_process_id: consumer.process_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let (status, attach_op) = runtime.begin_hostcall(
            consumer.process_id,
            HostcallRequest::AttachRegion {
                region_id: alloc.region_id,
                reader_slot: None,
                prot: selium_abi::RegionProt::ReadWrite,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(consumer.process_id, attach_op),
            CompletionState::Ready(HostcallOutput::RegionAttach(_))
        ));
        runtime
            .stop_process(consumer.process_id)
            .expect("consumer teardown");
        assert!(!runtime.resident_guest_ids().contains(&consumer.process_id));
    }

    // scenario: selium-abi/mintcertificate-capability-variant/signing-hostcall-requires-the-capability
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
    // scenario: selium-identity/host-held-signing-oracle/signing-returns-certificates-never-keys

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
    // scenario: selium-abi/owner-scoped-attach-authorisation/recorded-id-authorises-the-attach
    // scenario: selium-abi/owner-declared-resolvable-attach/declared-resource-admits-a-resolver
    // scenario: selium-abi/owner-declared-resolvable-attach/undeclared-resource-refuses-a-record

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
                tenant: None,
                discovery_service: false,
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

        // An undeclared resource refuses a resolve recording even from the
        // would-be client itself.
        let (status, _) = runtime.begin_hostcall(
            consumer.process_id,
            HostcallRequest::RecordResolvedResourceFor {
                resource_class: ResourceClass::SharedRegion,
                resource_id: alloc.region_id,
                client_process_id: consumer.process_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

        // The owner declares the region attachable by resolution; the
        // consumer's resolve then records an attach basis, after which the
        // attach is authorised.
        let (status, _) = runtime.begin_hostcall(
            owner.process_id,
            HostcallRequest::DeclareAttachableByResolution {
                resource_class: ResourceClass::SharedRegion,
                resource_id: alloc.region_id,
                scope: selium_abi::AttachDeclarationScope::Any,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let (status, _) = runtime.begin_hostcall(
            consumer.process_id,
            HostcallRequest::RecordResolvedResourceFor {
                resource_class: ResourceClass::SharedRegion,
                resource_id: alloc.region_id,
                client_process_id: consumer.process_id,
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
    // scenario: resource-handshake/guest-self-identity/self-info

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
    // scenario: resource-handshake/handoff-metadata/oversized-metadata-rejected

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
    // scenario: selium-runtime/process-spawn-quota/quota-distinct-from-lifecycle-grant
    // scenario: selium-accountant/quota-enforcement-at-allocation/spawn-over-process-quota-denied
    #[test]
    fn process_start_enforces_tenant_process_quota_and_skips_root() {
        let runtime = Runtime::default();
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
    // scenario: selium-runtime/process-spawn-quota/exited-process-returns-its-slot
    #[test]
    fn process_quota_slot_returns_to_tenant_on_exit() {
        let runtime = Runtime::default();
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
                tenant: None,
                discovery_service: false,
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

    // scenario: capability-enforcement/unguessable-resource-identities/guessing-is-not-viable
    // scenario: capability-enforcement/ownership-checked-attach/no-implicit-sharing
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
                tenant: None,
                discovery_service: false,
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
                tenant: None,
                discovery_service: false,
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

    // scenario: capability-enforcement/descendant-telemetry-reads/supervisor-reads-child-telemetry
    #[test]
    fn children_selector_allows_descendant_metering_read() {
        let runtime = Runtime::default();

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
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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
                tenant: Some("acme".to_string()),
                discovery_service: false,
                handlers: Vec::new(),
            })
            .expect("spawn contain-parent");

        // Try to spawn a child with a Tenant("beta") grant that exceeds
        // the parent's Tenant("acme") authority.
        let (status, op) = runtime.begin_hostcall(
            parent.process_id,
            HostcallRequest::ProcessStart {
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
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

    // scenario: selium-runtime/network-hostcall-dispatch/guest-binds-tcp-listener-via-runtime
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
    // scenario: selium-abi/capability-probe-gated-by-authority/authorised-probe
    // scenario: selium-abi/capability-probe-gated-by-authority/unauthorised-probe-denied
    // scenario: selium-abi/capability-probe-scope/probe-requires-the-probe-authority

    #[test]
    fn process_capability_is_denied_without_probe_authority() {
        // Probing another process's grants requires the bootstrap-provisioned
        // `ProcessProbe` authority rather than any guest identity: exposing it
        // broadly would leak grant state.
        let runtime = Runtime::default();
        let guest = spawn_with_grants(&runtime, Vec::new());

        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::ProcessCapability {
                process_id: guest.process_id,
                capability: Capability::ActivityRead,
                uri: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::PermissionDenied);
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }

        // A holder of the probe authority gets a truthful answer.
        let prober = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(Capability::ProcessProbe, Vec::new())],
        );
        let (status, op) = runtime.begin_hostcall(
            prober.process_id,
            HostcallRequest::ProcessCapability {
                process_id: guest.process_id,
                capability: Capability::ActivityRead,
                uri: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(prober.process_id, op),
            CompletionState::Ready(HostcallOutput::U64(0))
        ));
    }
    // scenario: selium-abi/live-process-list/process-list-requires-the-probe-authority
    // scenario: selium-abi/live-process-list/process-list-includes-a-silent-process

    /// Process membership is registry state and is gated on the
    /// bootstrap-provisioned probe authority, like capability probing.
    #[test]
    fn process_list_requires_the_probe_authority() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(&runtime, Vec::new());

        let (status, op) = runtime.begin_hostcall(guest.process_id, HostcallRequest::ProcessList);
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        match runtime.poll_hostcall(guest.process_id, op) {
            CompletionState::Failed(error) => {
                assert_eq!(error.code, AbiErrorCode::PermissionDenied);
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }

        let prober = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(Capability::ProcessProbe, Vec::new())],
        );
        let (status, op) = runtime.begin_hostcall(prober.process_id, HostcallRequest::ProcessList);
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        let CompletionState::Ready(HostcallOutput::Processes(processes)) =
            runtime.poll_hostcall(prober.process_id, op)
        else {
            panic!("expected Processes output");
        };
        assert!(processes.contains(&guest.process_id));
        assert!(processes.contains(&prober.process_id));
        assert!(processes.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    /// A probe answers truthfully for a tenant-scoped grant: the target holds it
    /// within its own tenant, and the probe must not report "not held" because it
    /// evaluated against an empty scope.
    #[test]
    fn process_capability_reports_a_tenant_scoped_grant_as_held() {
        let runtime = Runtime::default();
        let tenant_guest = spawn_with_grants_and_tenant(
            &runtime,
            "tenant-scoped",
            vec![CapabilityGrant::new(
                Capability::ActivityRead,
                vec![ResourceSelector::Tenant("acme".to_string())],
            )],
            Some("acme"),
        );
        let prober = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(Capability::ProcessProbe, Vec::new())],
        );

        let (status, op_id) = runtime.begin_hostcall(
            prober.process_id,
            HostcallRequest::ProcessCapability {
                process_id: tenant_guest.process_id,
                capability: Capability::ActivityRead,
                uri: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(
            matches!(
                runtime.poll_hostcall(prober.process_id, op_id),
                CompletionState::Ready(HostcallOutput::U64(1))
            ),
            "a tenant-scoped grant is held within that tenant"
        );

        // A capability the tenant does not hold is still reported as absent, so the
        // probe discloses nothing beyond the capability asked for.
        let (status, op_id) = runtime.begin_hostcall(
            prober.process_id,
            HostcallRequest::ProcessCapability {
                process_id: tenant_guest.process_id,
                capability: Capability::MintCertificate,
                uri: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(prober.process_id, op_id),
            CompletionState::Ready(HostcallOutput::U64(0))
        ));
    }
    // scenario: selium-abi/process-start-module-source/spawn-from-inline-bytes
    // scenario: selium-abi/process-start-module-source/no-host-side-catalogue

    /// A process starts from inline bytes with no host-side module catalogue:
    /// the caller supplies the module source for every spawn.
    #[test]
    fn process_start_uses_inline_module_bytes_without_a_registry() {
        let runtime = Runtime::default();
        let spawner = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::ProcessLifecycle,
                vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
            )],
        );
        let (_, op) = runtime.begin_hostcall(
            spawner.process_id,
            HostcallRequest::ProcessStart {
                source: selium_abi::ModuleSource::Inline(module_with_entrypoint("main", "")),
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
            },
        );
        assert!(matches!(
            ready(&runtime, spawner.process_id, op),
            HostcallOutput::Process(_)
        ));
    }

    // scenario: selium-abi/process-start-module-source/spawn-from-a-durable-slice
    /// A durable slice is read through the runtime's own volume layer: a
    /// caller with storage authority starts from a slice it wrote.
    #[test]
    fn process_start_reads_a_module_from_a_durable_slice() {
        let runtime = Runtime::default();
        let spawner = spawn_with_grants(
            &runtime,
            vec![
                CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                ),
                CapabilityGrant::new(
                    Capability::Storage,
                    vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
                ),
            ],
        );
        let module = module_with_entrypoint("main", "");
        let (_, open_op) = runtime.begin_hostcall(
            spawner.process_id,
            HostcallRequest::VolumeOpen {
                name: "module-vol".to_string(),
                serving_tenant: None,
            },
        );
        let HostcallOutput::Volume(volume) = ready(&runtime, spawner.process_id, open_op) else {
            panic!("expected volume descriptor");
        };
        let (_, append_op) = runtime.begin_hostcall(
            spawner.process_id,
            HostcallRequest::VolumeAppend {
                local_id: volume.local_id,
                bytes: module.clone(),
            },
        );
        drop(ready(&runtime, spawner.process_id, append_op));

        let (_, op) = runtime.begin_hostcall(
            spawner.process_id,
            HostcallRequest::ProcessStart {
                source: selium_abi::ModuleSource::Slice {
                    volume: "module-vol".to_string(),
                    offset: 0,
                    len: module.len() as u64,
                },
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
            },
        );
        assert!(matches!(
            ready(&runtime, spawner.process_id, op),
            HostcallOutput::Process(_)
        ));
    }

    // scenario: selium-abi/process-start-module-source/slice-outside-the-caller-s-authority
    /// A slice on a volume the caller's storage authority does not cover is
    /// denied with a permission error, and no process is created.
    #[test]
    fn process_start_denies_a_slice_outside_the_callers_authority() {
        let runtime = Runtime::default();
        let spawner = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::ProcessLifecycle,
                vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
            )],
        );
        let (status, op) = runtime.begin_hostcall(
            spawner.process_id,
            HostcallRequest::ProcessStart {
                source: selium_abi::ModuleSource::Slice {
                    volume: "someone-elses-vol".to_string(),
                    offset: 0,
                    len: 64,
                },
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(spawner.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    // scenario: selium-abi/waitregister-hostcall/wake-routed-via-envelope-task
    /// Task 1.1: a guest registering interest in a region through
    /// `WaitRegister` is recorded in the wait registry under
    /// `(process_id, region_id)`, so a later generation advance — from this
    /// process or any other — can route a wake back to its task.
    #[test]
    fn wait_register_records_the_waiting_process_keyed_by_region() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );
        let region_id = allocate_region(&runtime, guest.process_id);

        let (status, _op) = runtime.begin_hostcall_with_task(
            guest.process_id,
            HostcallRequest::WaitRegister {
                region_id,
                generation: 7,
            },
            Some(1),
            None,
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);

        let registry = runtime.wait_registry.lock();
        let entries = registry
            .get(&(guest.process_id, region_id))
            .expect("wait registered under (process, region)");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.first().map(|entry| entry.generation), Some(7));
        assert_eq!(entries.first().map(|entry| entry.task_id), Some(1));
    }

    /// Task 1.3: a generation advance that targets a stopped process is a
    /// no-op — it neither panics nor leaves a stale registration behind, and
    /// a repeat advance stays a no-op (delivery is idempotent).
    #[test]
    fn generation_advance_for_a_stopped_process_is_a_no_op() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
        );
        let region_id = allocate_region(&runtime, guest.process_id);
        let (status, _op) = runtime.begin_hostcall_with_task(
            guest.process_id,
            HostcallRequest::WaitRegister {
                region_id,
                generation: 1,
            },
            Some(1),
            None,
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);

        runtime
            .stop_process(guest.process_id)
            .expect("stop process with an outstanding wait registration");

        // Deliver twice: the mailbox for a stopped process is gone, so each
        // advance is dropped without error and without leaving the registry
        // populated.
        runtime.note_generation_advance(region_id, 2);
        runtime.note_generation_advance(region_id, 3);
        assert!(
            runtime.wait_registry.lock().is_empty(),
            "a delivered wake must not leave the registration behind"
        );
    }

    // scenario: selium-abi/waitregister-hostcall/unattached-region-rejected
    // scenario: channel-wake-wait/cross-process-wait-registration/registration-for-unattached-region
    #[test]
    fn wait_register_for_an_unattached_region_is_denied() {
        let runtime = Runtime::default();
        let shared_memory = || {
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )
        };
        let owner = spawn_with_grants(&runtime, vec![shared_memory()]);
        let stranger = spawn_with_grants(&runtime, vec![shared_memory()]);
        let region_id = allocate_region(&runtime, owner.process_id);

        // Holding the class grant is not enough: a wake registration requires a
        // live mapping, otherwise a guest could probe for the existence of
        // regions it never attached.
        let (status, op) = runtime.begin_hostcall_with_task(
            stranger.process_id,
            HostcallRequest::WaitRegister {
                region_id,
                generation: 1,
            },
            Some(1),
            None,
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(stranger.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
        assert!(
            runtime.wait_registry.lock().is_empty(),
            "a denied registration must leave no entry behind"
        );
    }

    // scenario: selium-abi/expected-claims-and-process-list/another-process-s-expected-claims-require-the-probe-authority
    #[test]
    fn expected_claims_of_another_process_require_the_probe_authority() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(&runtime, Vec::new());
        let other = spawn_with_grants(&runtime, Vec::new());

        // A process always sees its own expectation.
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::ExpectedClaims {
                process_id: guest.process_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, op),
            CompletionState::Ready(HostcallOutput::Claims(_))
        ));

        // Another process's expectation is registry state.
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::ExpectedClaims {
                process_id: other.process_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));

        let prober = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(Capability::ProcessProbe, Vec::new())],
        );
        let (status, op) = runtime.begin_hostcall(
            prober.process_id,
            HostcallRequest::ExpectedClaims {
                process_id: other.process_id,
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
        assert!(matches!(
            runtime.poll_hostcall(prober.process_id, op),
            CompletionState::Ready(HostcallOutput::Claims(_))
        ));
    }

    // scenario: selium-abi/capability-probe-scope/route-claim-is-probed-in-the-uri-s-scope
    #[test]
    fn capability_probe_evaluates_a_route_claim_in_the_uri_scope() {
        let runtime = Runtime::default();
        let claimant = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///control".to_string())],
            )],
        );
        let prober = spawn_with_grants(
            &runtime,
            vec![CapabilityGrant::new(Capability::ProcessProbe, Vec::new())],
        );

        let probe = |uri: &str| {
            let (status, op) = runtime.begin_hostcall(
                prober.process_id,
                HostcallRequest::ProcessCapability {
                    process_id: claimant.process_id,
                    capability: Capability::RouteClaim,
                    uri: Some(uri.to_string()),
                },
            );
            assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
            match runtime.poll_hostcall(prober.process_id, op) {
                CompletionState::Ready(HostcallOutput::U64(held)) => held,
                other => panic!("expected U64 probe answer, got {other:?}"),
            }
        };

        assert_eq!(probe("sel:///control"), 1, "claim holds in its URI scope");
        assert_eq!(probe("sel:///other"), 0, "claim does not extend past it");
    }

    // scenario: selium-abi/tenant-volume-usage-hostcall/unauthorised-caller-denied
    #[test]
    fn volume_usage_requires_the_quota_write_authority() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants(&runtime, Vec::new());
        let (status, op) = runtime.begin_hostcall(
            guest.process_id,
            HostcallRequest::VolumeUsage {
                tenant: "acme".to_string(),
            },
        );
        assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, op),
            CompletionState::Failed(error) if error.code == AbiErrorCode::PermissionDenied
        ));
    }

    // scenario: selium-abi/cpu-resource-class/cpu-class-allocates-no-resource
    // scenario: process-metering/cpu-ceiling-delivery-under-a-resource-class/cpu-class-allocates-nothing
    #[test]
    fn allocating_a_shared_region_leaves_the_cpu_class_untouched() {
        let runtime = Runtime::default();
        let guest = spawn_with_grants_and_tenant(
            &runtime,
            "cpu-class-guest",
            vec![CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            )],
            Some("acme"),
        );
        let _region = allocate_region(&runtime, guest.process_id);

        assert_eq!(
            runtime.kernel.quota().used("acme", ResourceClass::Cpu),
            0,
            "allocating bytes must not consume the CPU class"
        );
        assert!(
            runtime
                .kernel
                .quota()
                .used("acme", ResourceClass::SharedRegion)
                > 0,
            "the allocation is charged to its own class"
        );
    }
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `capability-enforcement` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
