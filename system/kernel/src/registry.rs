use futures_util::future::BoxFuture;
use sharded_slab::Slab;
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    marker::PhantomData,
    sync::{Arc, Mutex},
    task::Waker,
};
use thiserror::Error;
use tracing::{
    Instrument, Span, debug,
    field::{self, Empty},
};

use crate::{
    KernelError,
    drivers::Capability,
    futures::FutureSharedState,
    guest_data::GuestResult,
    mailbox::GuestMailbox,
    session::{Session, SessionError},
};
use selium_abi::GuestResourceId;
use wasmtime::{StoreLimits, StoreLimitsBuilder};

/// Stable registry identifier for stored resources.
pub type ResourceId = usize;
type GuestFuture = Arc<FutureSharedState<GuestResult<Vec<u8>>>>;

/// High-level classification of a resource stored in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Process,
    Instance,
    Channel,
    Reader,
    Writer,
    Session,
    Network,
    Future,
    Other,
}

/// Metadata describing a registered resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceMetadata {
    pub id: ResourceId,
    pub owner: Option<ResourceId>,
    pub kind: ResourceType,
}

/// Typed handle to a resource stored in the [`Registry`].
#[derive(Clone)]
pub struct ResourceHandle<T>(ResourceId, PhantomData<T>);

struct Resource {
    data: Arc<Mutex<Option<Box<dyn Any + Send>>>>,
    kind: ResourceType,
    /// Tracing span for resource attribution.
    span: Span,
}

struct InstanceState {
    process_id: Option<ResourceId>,
    mailbox: Option<&'static GuestMailbox>,
    extensions: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
    limits: StoreLimits,
}

#[derive(Default)]
struct HandleTable {
    entries: Vec<Option<ResourceId>>,
    free: Vec<usize>,
}

struct HandleIndex {
    shared: HandleTable,
    shared_reverse: HashMap<ResourceId, usize>,
    instances: HashMap<ResourceId, HandleTable>,
    futures: HashMap<ResourceId, HandleTable>,
}

#[derive(Default)]
struct RelationIndex {
    owner_of: HashMap<ResourceId, ResourceId>,
    owned_by: HashMap<ResourceId, Vec<ResourceId>>,
    parent_of: HashMap<ResourceId, ResourceId>,
    children_of: HashMap<ResourceId, Vec<ResourceId>>,
    instance_to_process: HashMap<ResourceId, ResourceId>,
    process_to_instance: HashMap<ResourceId, ResourceId>,
    process_log_channel: HashMap<ResourceId, ResourceId>,
    log_channel_process: HashMap<ResourceId, ResourceId>,
}

/// Registry of guest resources.
pub struct Registry {
    resources: Slab<Resource>,
    relations: Mutex<RelationIndex>,
    handles: Mutex<HandleIndex>,
}

/// Registry view tied to a specific guest instance.
pub struct InstanceRegistry {
    /// Pointer to global registry
    registry: Arc<Registry>,
    /// Instance state resource identifier.
    instance_id: ResourceId,
}

/// Cloneable view for registering instance-scoped resources from async contexts.
#[derive(Clone)]
pub struct InstanceRegistrar {
    registry: Arc<Registry>,
    instance_id: ResourceId,
}

/// Errors surfaced when interacting with the registry.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// Registry has reached capacity and cannot accept more entries.
    #[error("registry capacity exhausted")]
    CapacityExhausted,
    /// Registry state is unavailable because an internal lock is poisoned.
    #[error("registry lock poisoned")]
    LockPoisoned,
    /// Attempted to initialise or access an invalid reservation.
    #[error("invalid resource reservation")]
    InvalidReservation,
    /// Instance state is missing from the registry.
    #[error("instance state missing")]
    MissingInstance,
}

/// Stable identity associated with a running process instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessIdentity(ResourceId);

impl InstanceState {
    fn new() -> Self {
        Self {
            process_id: None,
            mailbox: None,
            extensions: HashMap::new(),
            limits: StoreLimits::default(),
        }
    }
}

impl HandleTable {
    fn allocate(&mut self, resource_id: ResourceId) -> usize {
        if let Some(slot) = self.free.pop()
            && let Some(entry) = self.entries.get_mut(slot)
        {
            *entry = Some(resource_id);
            return slot;
        }

        self.entries.push(Some(resource_id));
        self.entries.len() - 1
    }

    fn resolve(&self, handle: usize) -> Option<ResourceId> {
        self.entries.get(handle).and_then(|entry| *entry)
    }

    fn remove(&mut self, handle: usize) -> Option<ResourceId> {
        let entry = self.entries.get_mut(handle)?;
        let resource_id = entry.take();
        if resource_id.is_some() {
            self.free.push(handle);
        }
        resource_id
    }
}

impl HandleIndex {
    fn new() -> Self {
        Self {
            shared: HandleTable::default(),
            shared_reverse: HashMap::new(),
            instances: HashMap::new(),
            futures: HashMap::new(),
        }
    }

    fn share_handle(&mut self, id: ResourceId) -> Result<GuestResourceId, RegistryError> {
        if let Some(existing) = self.shared_reverse.get(&id).copied() {
            return GuestResourceId::try_from(existing)
                .map_err(|_| RegistryError::CapacityExhausted);
        }

        let handle = self.shared.allocate(id);
        match GuestResourceId::try_from(handle) {
            Ok(guest) => {
                self.shared_reverse.insert(id, handle);
                Ok(guest)
            }
            Err(_) => {
                self.shared.remove(handle);
                Err(RegistryError::CapacityExhausted)
            }
        }
    }

    fn resolve_shared(&self, handle: GuestResourceId) -> Option<ResourceId> {
        let idx = usize::try_from(handle).ok()?;
        self.shared.resolve(idx)
    }

    fn shared_handle(&self, id: ResourceId) -> Option<GuestResourceId> {
        let handle = self.shared_reverse.get(&id).copied()?;
        GuestResourceId::try_from(handle).ok()
    }

    fn remove_shared(&mut self, id: ResourceId) {
        if let Some(handle) = self.shared_reverse.remove(&id) {
            self.shared.remove(handle);
        }
    }

    fn insert_instance(&mut self, instance_id: ResourceId, resource_id: ResourceId) -> usize {
        self.instances
            .entry(instance_id)
            .or_default()
            .allocate(resource_id)
    }

    fn resolve_instance(&self, instance_id: ResourceId, handle: usize) -> Option<ResourceId> {
        self.instances
            .get(&instance_id)
            .and_then(|table| table.resolve(handle))
    }

    fn remove_instance(&mut self, instance_id: ResourceId, handle: usize) -> Option<ResourceId> {
        self.instances
            .get_mut(&instance_id)
            .and_then(|table| table.remove(handle))
    }

    fn insert_future(&mut self, instance_id: ResourceId, resource_id: ResourceId) -> usize {
        self.futures
            .entry(instance_id)
            .or_default()
            .allocate(resource_id)
    }

    fn resolve_future(&self, instance_id: ResourceId, handle: usize) -> Option<ResourceId> {
        self.futures
            .get(&instance_id)
            .and_then(|table| table.resolve(handle))
    }

    fn remove_future(&mut self, instance_id: ResourceId, handle: usize) -> Option<ResourceId> {
        self.futures
            .get_mut(&instance_id)
            .and_then(|table| table.remove(handle))
    }

    fn remove_instance_tables(&mut self, instance_id: ResourceId) {
        self.instances.remove(&instance_id);
        self.futures.remove(&instance_id);
    }
}

impl RelationIndex {
    fn set_owner(&mut self, id: ResourceId, owner: ResourceId) {
        if let Some(previous) = self.owner_of.insert(id, owner)
            && previous != owner
        {
            Self::remove_from_list(self.owned_by.get_mut(&previous), id);
        }
        Self::push_unique(self.owned_by.entry(owner).or_default(), id);
    }

    fn owner(&self, id: ResourceId) -> Option<ResourceId> {
        self.owner_of.get(&id).copied()
    }

    fn owned_by(&self, owner: ResourceId) -> Vec<ResourceId> {
        self.owned_by.get(&owner).cloned().unwrap_or_default()
    }

    fn set_parent(&mut self, id: ResourceId, parent: ResourceId) {
        if let Some(previous) = self.parent_of.insert(id, parent)
            && previous != parent
        {
            Self::remove_from_list(self.children_of.get_mut(&previous), id);
        }
        Self::push_unique(self.children_of.entry(parent).or_default(), id);
    }

    fn parent(&self, id: ResourceId) -> Option<ResourceId> {
        self.parent_of.get(&id).copied()
    }

    fn children(&self, parent: ResourceId) -> Vec<ResourceId> {
        self.children_of.get(&parent).cloned().unwrap_or_default()
    }

    fn set_instance_process(&mut self, instance_id: ResourceId, process_id: ResourceId) {
        if let Some(previous) = self.instance_to_process.insert(instance_id, process_id)
            && previous != process_id
        {
            self.process_to_instance.remove(&previous);
        }
        self.process_to_instance.insert(process_id, instance_id);
    }

    fn instance_process(&self, instance_id: ResourceId) -> Option<ResourceId> {
        self.instance_to_process.get(&instance_id).copied()
    }

    fn process_instance(&self, process_id: ResourceId) -> Option<ResourceId> {
        self.process_to_instance.get(&process_id).copied()
    }

    fn set_log_channel(&mut self, process_id: ResourceId, channel_id: ResourceId) {
        if let Some(previous) = self.process_log_channel.insert(process_id, channel_id)
            && previous != channel_id
        {
            self.log_channel_process.remove(&previous);
        }

        if let Some(previous) = self.log_channel_process.insert(channel_id, process_id)
            && previous != process_id
        {
            self.process_log_channel.remove(&previous);
        }
    }

    fn log_channel(&self, process_id: ResourceId) -> Option<ResourceId> {
        self.process_log_channel.get(&process_id).copied()
    }

    fn remove_resource(&mut self, id: ResourceId) {
        if let Some(owner) = self.owner_of.remove(&id) {
            Self::remove_from_list(self.owned_by.get_mut(&owner), id);
        }

        if let Some(parent) = self.parent_of.remove(&id) {
            Self::remove_from_list(self.children_of.get_mut(&parent), id);
        }

        if let Some(process) = self.instance_to_process.remove(&id) {
            self.process_to_instance.remove(&process);
        }

        if let Some(instance) = self.process_to_instance.remove(&id) {
            self.instance_to_process.remove(&instance);
        }

        if let Some(channel) = self.process_log_channel.remove(&id) {
            self.log_channel_process.remove(&channel);
        }

        if let Some(process) = self.log_channel_process.remove(&id) {
            self.process_log_channel.remove(&process);
        }
    }

    fn push_unique(list: &mut Vec<ResourceId>, id: ResourceId) {
        if !list.contains(&id) {
            list.push(id);
        }
    }

    fn remove_from_list(list: Option<&mut Vec<ResourceId>>, id: ResourceId) {
        if let Some(list) = list {
            list.retain(|entry| *entry != id);
        }
    }
}

impl ProcessIdentity {
    /// Create a new identity from a resource id.
    pub fn new(id: ResourceId) -> Self {
        Self(id)
    }

    /// Return the raw numeric representation of this identity.
    pub fn raw(&self) -> ResourceId {
        self.0
    }
}

impl<T> ResourceHandle<T> {
    /// Create a typed handle from a raw resource identifier.
    pub fn new(id: ResourceId) -> ResourceHandle<T> {
        Self(id, PhantomData)
    }

    /// Return the raw numeric representation of this handle.
    pub fn into_id(self) -> ResourceId {
        self.0
    }
}

impl Registry {
    fn resource_span(kind: ResourceType, owner: Option<ResourceId>) -> Span {
        tracing::debug_span!(
            "kernel.resource",
            resource_id = Empty,
            kind = ?kind,
            owner = ?owner,
            resource_type = Empty,
            guest_slot = Empty,
            host_ptr = Empty,
            parent_id = Empty,
            shared_id = Empty,
        )
    }

    /// Create a new registry.
    pub fn new() -> Arc<Self> {
        let registry = Arc::new(Self {
            resources: Slab::new(),
            relations: Mutex::new(RelationIndex::default()),
            handles: Mutex::new(HandleIndex::new()),
        });

        // Reserve the first ID (id=0) for system use
        registry.resources.insert(Resource {
            data: Arc::new(Mutex::new(Some(Box::new(())))),
            kind: ResourceType::Other,
            span: Self::resource_span(ResourceType::Other, None),
        });

        registry
    }

    /// Create an [`InstanceRegistry`] view tied to this registry.
    pub fn instance(self: &Arc<Self>) -> Result<InstanceRegistry, RegistryError> {
        let instance = self.add(InstanceState::new(), None, ResourceType::Instance)?;
        Ok(InstanceRegistry {
            registry: self.clone(),
            instance_id: instance.into_id(),
        })
    }

    /// Add a resource to the registry and return its typed handle.
    pub fn add<T: Send + 'static>(
        &self,
        resource: T,
        owner: Option<ResourceId>,
        kind: ResourceType,
    ) -> Result<ResourceHandle<T>, RegistryError> {
        let r = Resource {
            data: Arc::new(Mutex::new(Some(Box::new(resource)))),
            kind,
            span: Self::resource_span(kind, owner),
        };
        let raw = self
            .resources
            .insert(r)
            .ok_or(RegistryError::CapacityExhausted)?;
        if let Some(owner) = owner {
            let mut relations = self
                .relations
                .lock()
                .map_err(|_| RegistryError::LockPoisoned)?;
            relations.set_owner(raw, owner);
        }
        self.record_resource_added::<T>(raw);
        Ok(ResourceHandle(raw, PhantomData))
    }

    /// Reserve a slot for a resource and return its identifier.
    pub fn reserve(
        &self,
        owner: Option<ResourceId>,
        kind: ResourceType,
    ) -> Result<ResourceId, RegistryError> {
        let r = Resource {
            data: Arc::new(Mutex::new(None)),
            kind,
            span: Self::resource_span(kind, owner),
        };

        let id = self
            .resources
            .insert(r)
            .ok_or(RegistryError::CapacityExhausted)?;
        if let Some(owner) = owner {
            let mut relations = self
                .relations
                .lock()
                .map_err(|_| RegistryError::LockPoisoned)?;
            relations.set_owner(id, owner);
        }
        self.record_resource_reserved(id);
        Ok(id)
    }

    /// Initialise a reserved slot with the given resource.
    pub fn initialise<T: Send + 'static>(
        &self,
        id: ResourceId,
        resource: T,
    ) -> Result<ResourceHandle<T>, RegistryError> {
        let entry = self
            .resources
            .get(id)
            .ok_or(RegistryError::InvalidReservation)?;
        let mut guard = entry.data.lock().map_err(|_| RegistryError::LockPoisoned)?;

        if guard.is_some() {
            return Err(RegistryError::InvalidReservation);
        }

        *guard = Some(Box::new(resource));
        self.record_resource_initialised::<T>(id);
        Ok(ResourceHandle(id, PhantomData))
    }

    /// Remove a resource from the registry, returning ownership.
    pub fn remove<T: 'static>(&self, id: ResourceHandle<T>) -> Option<T> {
        self.record_resource_removed(id.0);
        let kind = self.resources.get(id.0).map(|resource| resource.kind);
        if let Ok(mut handles) = self.handles.lock() {
            handles.remove_shared(id.0);
            if matches!(kind, Some(ResourceType::Instance)) {
                handles.remove_instance_tables(id.0);
            }
        }
        if let Ok(mut relations) = self.relations.lock() {
            relations.remove_resource(id.0);
        }
        self.resources.take(id.0).and_then(|resource| {
            let data = Arc::try_unwrap(resource.data).ok()?;
            let boxed_opt = data.into_inner().ok()?;
            let boxed = boxed_opt?;
            boxed.downcast::<T>().map(|b| *b).ok()
        })
    }

    /// Discard a resource entry without attempting to downcast its payload.
    pub fn discard(&self, id: ResourceId) -> bool {
        self.record_resource_removed(id);
        let kind = self.resources.get(id).map(|resource| resource.kind);
        if let Ok(mut handles) = self.handles.lock() {
            handles.remove_shared(id);
            if matches!(kind, Some(ResourceType::Instance)) {
                handles.remove_instance_tables(id);
            }
        }
        if let Ok(mut relations) = self.relations.lock() {
            relations.remove_resource(id);
        }
        self.resources.take(id).is_some()
    }

    /// Borrow a resource mutably by erased handle and run a closure with it.
    pub fn with<T: 'static, R>(
        &self,
        id: ResourceHandle<T>,
        func: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        let (data, span) = {
            let entry = self.resources.get(id.0)?;
            (entry.data.clone(), entry.span.clone())
        };
        let mut guard = data.lock().ok()?;
        let t = guard.as_mut().and_then(|boxed| boxed.downcast_mut::<T>())?;
        Some(span.in_scope(|| func(t)))
    }

    /// Borrow a resource mutably by erased handle and run a closure with it.
    pub(crate) async fn with_async<T: Send + 'static, R>(
        &self,
        id: ResourceHandle<T>,
        func: impl for<'a> FnOnce(&'a mut T) -> BoxFuture<'a, R>,
    ) -> Option<R> {
        let (data, span) = {
            let entry = self.resources.get(id.0)?;
            (entry.data.clone(), entry.span.clone())
        };
        let boxed = {
            let mut guard = data.lock().ok()?;
            guard.take()?
        };
        let mut resource = boxed.downcast::<T>().ok()?;
        let result = func(resource.as_mut()).instrument(span).await;
        let boxed: Box<dyn Any + Send> = resource;
        let mut guard = data.lock().ok()?;
        *guard = Some(boxed);
        Some(result)
    }

    /// Create or retrieve a shared guest handle for the resource id.
    pub fn share_handle(&self, id: ResourceId) -> Result<GuestResourceId, RegistryError> {
        let shared = {
            let mut handles = self
                .handles
                .lock()
                .map_err(|_| RegistryError::LockPoisoned)?;
            handles.share_handle(id)
        }?;

        self.record_shared_handle(id, shared);

        Ok(shared)
    }

    /// Resolve a shared guest handle into its resource id.
    pub fn resolve_shared(&self, handle: GuestResourceId) -> Option<ResourceId> {
        let resolved = {
            let handles = self.handles.lock().ok()?;
            handles.resolve_shared(handle)
        };
        if let Some(id) = resolved
            && let Some(resource) = self.resources.get(id)
        {
            debug!(parent: &resource.span, shared_handle = handle, "resolve shared handle");
        }
        resolved
    }

    /// Return the shared guest handle for a resource id, if one exists.
    pub fn shared_handle(&self, id: ResourceId) -> Option<GuestResourceId> {
        let handles = self.handles.lock().ok()?;
        handles.shared_handle(id)
    }

    /// Fetch metadata for a resource.
    pub fn metadata(&self, id: ResourceId) -> Option<ResourceMetadata> {
        let resource = self.resources.get(id)?;
        let owner = self.relations.lock().ok()?.owner(id);
        Some(ResourceMetadata {
            id,
            owner,
            kind: resource.kind,
        })
    }

    /// Return the recorded owner for a resource.
    pub fn owner(&self, id: ResourceId) -> Option<ResourceId> {
        self.relations.lock().ok()?.owner(id)
    }

    /// Return the resources owned by the provided resource id.
    pub fn owned_resources(&self, owner: ResourceId) -> Vec<ResourceId> {
        self.relations
            .lock()
            .map(|relations| relations.owned_by(owner))
            .unwrap_or_default()
    }

    /// Return the recorded parent for a resource.
    pub fn parent(&self, id: ResourceId) -> Option<ResourceId> {
        self.relations.lock().ok()?.parent(id)
    }

    /// Return the children linked to the provided resource id.
    pub fn children(&self, id: ResourceId) -> Vec<ResourceId> {
        self.relations
            .lock()
            .map(|relations| relations.children(id))
            .unwrap_or_default()
    }

    /// Return the process id associated with the provided instance id.
    pub fn instance_process(&self, instance_id: ResourceId) -> Option<ResourceId> {
        self.relations.lock().ok()?.instance_process(instance_id)
    }

    /// Return the instance id associated with the provided process id.
    pub fn process_instance(&self, process_id: ResourceId) -> Option<ResourceId> {
        self.relations.lock().ok()?.process_instance(process_id)
    }

    /// Return the registered log channel resource for the process, if present.
    pub fn log_channel(&self, process_id: ResourceId) -> Option<ResourceId> {
        self.relations.lock().ok()?.log_channel(process_id)
    }

    /// Return the registered log channel handle for the process, if present.
    pub fn log_channel_handle(&self, process_id: ResourceId) -> Option<GuestResourceId> {
        let channel_id = self.log_channel(process_id)?;
        self.shared_handle(channel_id)
    }

    fn record_resource_added<T: 'static>(&self, id: ResourceId) {
        if let Some(resource) = self.resources.get(id) {
            resource.span.record("resource_id", field::display(id));
            resource
                .span
                .record("resource_type", field::display(std::any::type_name::<T>()));
            debug!(parent: &resource.span, "resource registered");
        }
    }

    fn record_resource_reserved(&self, id: ResourceId) {
        if let Some(resource) = self.resources.get(id) {
            resource.span.record("resource_id", field::display(id));
            debug!(parent: &resource.span, "resource reserved");
        }
    }

    fn record_resource_initialised<T: 'static>(&self, id: ResourceId) {
        if let Some(resource) = self.resources.get(id) {
            resource
                .span
                .record("resource_type", field::display(std::any::type_name::<T>()));
            debug!(parent: &resource.span, "resource initialised");
        }
    }

    fn record_resource_removed(&self, id: ResourceId) {
        if let Some(resource) = self.resources.get(id) {
            debug!(parent: &resource.span, "resource removed");
        }
    }

    fn record_guest_slot(&self, id: ResourceId, slot: usize) {
        if let Some(resource) = self.resources.get(id) {
            resource.span.record("guest_slot", field::display(slot));
            debug!(parent: &resource.span, guest_slot = %slot, "resource slot assigned");
        }
    }

    fn record_slot_detached(&self, id: ResourceId, slot: usize) {
        if let Some(resource) = self.resources.get(id) {
            debug!(parent: &resource.span, guest_slot = %slot, "resource slot detached");
        }
    }

    fn record_shared_handle(&self, id: ResourceId, shared: GuestResourceId) {
        if let Some(resource) = self.resources.get(id) {
            resource.span.record("shared_id", field::display(shared));
            debug!(parent: &resource.span, shared_handle = shared, "resource shared");
        } else {
            debug!(resource_id = %id, shared_handle = shared, "resource shared");
        }
    }

    /// Record a host pointer identifier for the specified resource.
    pub(crate) fn record_host_ptr(&self, id: ResourceId, ptr: &str) {
        if let Some(resource) = self.resources.get(id) {
            resource.span.record("host_ptr", field::display(ptr));
            debug!(parent: &resource.span, host_ptr = %ptr, "resource host pointer");
        }
    }

    /// Record the parent resource that produced this resource.
    pub(crate) fn record_parent(&self, id: ResourceId, parent: ResourceId) {
        if let Ok(mut relations) = self.relations.lock() {
            relations.set_parent(id, parent);
        }
        if let Some(resource) = self.resources.get(id) {
            resource.span.record("parent_id", field::display(parent));
            debug!(parent: &resource.span, parent_id = %parent, "resource parent linked");
        }
    }

    /// Associate a process instance with a registry instance.
    pub(crate) fn set_instance_process(
        &self,
        instance_id: ResourceId,
        process_id: ResourceId,
    ) -> Result<(), RegistryError> {
        if self.resources.get(instance_id).is_none() {
            return Err(RegistryError::InvalidReservation);
        }
        if self.resources.get(process_id).is_none() {
            return Err(RegistryError::InvalidReservation);
        }
        let mut relations = self
            .relations
            .lock()
            .map_err(|_| RegistryError::LockPoisoned)?;
        relations.set_instance_process(instance_id, process_id);
        relations.set_owner(instance_id, process_id);
        Ok(())
    }

    /// Associate a process with its log channel resource.
    pub(crate) fn set_log_channel(
        &self,
        process_id: ResourceId,
        channel_id: ResourceId,
    ) -> Result<(), RegistryError> {
        if self.resources.get(process_id).is_none() {
            return Err(RegistryError::InvalidReservation);
        }
        if self.resources.get(channel_id).is_none() {
            return Err(RegistryError::InvalidReservation);
        }
        let mut relations = self
            .relations
            .lock()
            .map_err(|_| RegistryError::LockPoisoned)?;
        relations.set_log_channel(process_id, channel_id);
        Ok(())
    }
}

impl InstanceRegistry {
    fn with_instance_state<R>(&self, f: impl FnOnce(&mut InstanceState) -> R) -> Option<R> {
        self.registry.with(ResourceHandle::new(self.instance_id), f)
    }

    /// Attach a mailbox used for guest async wake-ups.
    pub fn load_mailbox(&mut self, mb: &'static GuestMailbox) {
        let _ = self.with_instance_state(|state| state.mailbox = Some(mb));
    }

    /// Create a lightweight registrar for instance-scoped resources.
    pub fn registrar(&self) -> InstanceRegistrar {
        InstanceRegistrar {
            registry: self.registry.clone(),
            instance_id: self.instance_id,
        }
    }

    /// Refresh mailbox base pointers after guest memory growth.
    pub fn refresh_mailbox(&self, base: usize) {
        if let Some(mb) = self.mailbox() {
            mb.refresh_base(base);
        }
    }

    /// Close the mailbox to prevent further guest wake-ups.
    pub fn close_mailbox(&self) {
        if let Some(mb) = self.mailbox() {
            mb.close();
        }
    }

    /// Set a hard memory limit for this instance.
    pub fn set_memory_limit(&mut self, bytes: usize) {
        let _ = self.with_instance_state(|state| {
            state.limits = StoreLimitsBuilder::new().memory_size(bytes).build();
        });
    }

    fn insert_instance_handle(&self, resource_id: ResourceId) -> Result<usize, RegistryError> {
        let mut handles = self
            .registry
            .handles
            .lock()
            .map_err(|_| RegistryError::LockPoisoned)?;
        Ok(handles.insert_instance(self.instance_id, resource_id))
    }

    fn remove_instance_handle(&self, handle: usize) -> Option<ResourceId> {
        let mut handles = self.registry.handles.lock().ok()?;
        handles.remove_instance(self.instance_id, handle)
    }

    fn resolve_instance_handle(&self, handle: usize) -> Option<ResourceId> {
        let handles = self.registry.handles.lock().ok()?;
        handles.resolve_instance(self.instance_id, handle)
    }

    fn insert_future_handle(&self, resource_id: ResourceId) -> Result<usize, RegistryError> {
        let mut handles = self
            .registry
            .handles
            .lock()
            .map_err(|_| RegistryError::LockPoisoned)?;
        Ok(handles.insert_future(self.instance_id, resource_id))
    }

    fn resolve_future_handle(&self, handle: usize) -> Option<ResourceId> {
        let handles = self.registry.handles.lock().ok()?;
        handles.resolve_future(self.instance_id, handle)
    }

    fn remove_future_handle(&self, handle: usize) -> Option<ResourceId> {
        let mut handles = self.registry.handles.lock().ok()?;
        handles.remove_future(self.instance_id, handle)
    }

    /// Insert a resource entry and return its slot index.
    pub fn insert<T: Send + 'static>(
        &mut self,
        entry: T,
        owner: Option<ResourceId>,
        kind: ResourceType,
    ) -> Result<usize, RegistryError> {
        let owner = self.process_id()?.or(owner);
        let entry = self.registry.add(entry, owner, kind)?;
        let resource_id = entry.0;
        let slot = self.insert_instance_handle(resource_id)?;
        self.registry.record_guest_slot(resource_id, slot);
        Ok(slot)
    }

    /// Insert a resource ID and return its slot index.
    pub fn insert_id(&mut self, id: ResourceId) -> Result<usize, RegistryError> {
        let slot = self.insert_instance_handle(id)?;
        self.registry.record_guest_slot(id, slot);
        Ok(slot)
    }

    /// Retrieve the entry for the given slot.
    pub fn entry(&self, idx: usize) -> Option<ResourceId> {
        self.resolve_instance_handle(idx)
    }

    /// Borrow a resource by table index and apply a closure.
    pub fn with<T: 'static, R>(&self, idx: ResourceId, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let resource = self.resolve_instance_handle(idx)?;
        self.registry
            .with::<T, R>(ResourceHandle(resource, PhantomData), f)
    }

    /// Attach custom extension data to the instance.
    pub fn insert_extension<T: Any + Send + Sync>(&mut self, value: T) {
        let ext: Arc<dyn Any + Send + Sync> = Arc::new(value);
        let _ = self.with_instance_state(|state| {
            state.extensions.insert(TypeId::of::<T>(), ext);
        });
    }

    /// Borrow extension data by type.
    pub fn extension<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.with_instance_state(|state| {
            state
                .extensions
                .get(&TypeId::of::<T>())
                .and_then(|boxed| Arc::clone(boxed).downcast::<T>().ok())
        })
        .flatten()
    }

    /// Remove an entry by slot index.
    pub fn remove<T: 'static>(&mut self, idx: usize) -> Option<T> {
        let resource_id = self.remove_instance_handle(idx)?;
        self.registry
            .remove(ResourceHandle(resource_id, PhantomData))
    }

    /// Remove a slot entry without deleting the underlying resource.
    pub fn detach_slot(&mut self, idx: usize) -> Option<ResourceId> {
        let resource_id = self.remove_instance_handle(idx);
        if let Some(resource_id) = resource_id {
            self.registry.record_slot_detached(resource_id, idx);
        }
        resource_id
    }

    /// Produce a waker for the specified guest task.
    pub fn waker(&self, task_id: usize) -> Waker {
        self.mailbox()
            .expect("mailbox is not instantiated")
            .waker(task_id)
    }

    /// Access the guest mailbox backing async wake-ups.
    pub fn mailbox(&self) -> Option<&'static GuestMailbox> {
        self.with_instance_state(|state| state.mailbox).flatten()
    }

    /// Get a reference to the global registry.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Clone the underlying global registry reference.
    pub fn registry_arc(&self) -> Arc<Registry> {
        self.registry.clone()
    }

    /// Set the process identifier for this instance. Must be called before guest
    /// code can create resources.
    pub fn set_process_id(&mut self, process_id: ResourceId) {
        let _ = self.with_instance_state(|state| state.process_id = Some(process_id));
        let _ = self
            .registry
            .set_instance_process(self.instance_id, process_id);
    }

    fn process_id(&self) -> Result<Option<ResourceId>, RegistryError> {
        self.with_instance_state(|state| state.process_id)
            .ok_or(RegistryError::MissingInstance)
    }

    /// Grant a resource capability to the specified session entry.
    pub fn grant_session_resource(
        &self,
        session_slot: usize,
        capability: Capability,
        resource: ResourceId,
    ) -> Result<bool, KernelError> {
        self.with::<Session, _>(session_slot, |session| {
            session.grant_resource(capability, resource)
        })
        .ok_or(KernelError::InvalidHandle)
    }

    /// Revoke a resource capability from the specified session entry.
    pub fn revoke_session_resource(
        &self,
        session_slot: usize,
        capability: Capability,
        resource: ResourceId,
    ) -> Result<Result<bool, SessionError>, KernelError> {
        self.with::<Session, _>(session_slot, |session| {
            session.revoke_resource(capability, resource)
        })
        .ok_or(KernelError::InvalidHandle)
    }

    /// Insert a guest future and return its handle.
    pub fn insert_future(
        &mut self,
        state: Arc<FutureSharedState<GuestResult<Vec<u8>>>>,
    ) -> Result<usize, RegistryError> {
        let owner = self.process_id()?;
        let entry = self.registry.add(state, owner, ResourceType::Future)?;
        let handle = self.insert_future_handle(entry.0)?;
        Ok(handle)
    }

    /// Retrieve the shared state for a given future handle.
    pub(crate) fn future_state(&self, handle: usize) -> Option<GuestFuture> {
        let resource_id = self.resolve_future_handle(handle)?;
        self.registry.with(
            ResourceHandle::new(resource_id),
            |state: &mut GuestFuture| Arc::clone(state),
        )
    }

    /// Remove a future handle, returning the shared state if present.
    pub fn remove_future(&mut self, handle: usize) -> Option<GuestFuture> {
        let resource_id = self.remove_future_handle(handle)?;
        self.registry
            .remove(ResourceHandle::<GuestFuture>::new(resource_id))
    }
}

impl InstanceRegistrar {
    fn with_instance_state<R>(&self, f: impl FnOnce(&mut InstanceState) -> R) -> Option<R> {
        self.registry.with(ResourceHandle::new(self.instance_id), f)
    }

    fn process_id(&self) -> Result<Option<ResourceId>, RegistryError> {
        self.with_instance_state(|state| state.process_id)
            .ok_or(RegistryError::MissingInstance)
    }

    fn insert_instance_handle(&self, resource_id: ResourceId) -> Result<usize, RegistryError> {
        let mut handles = self
            .registry
            .handles
            .lock()
            .map_err(|_| RegistryError::LockPoisoned)?;
        Ok(handles.insert_instance(self.instance_id, resource_id))
    }

    fn resolve_instance_handle(&self, handle: usize) -> Option<ResourceId> {
        let handles = self.registry.handles.lock().ok()?;
        handles.resolve_instance(self.instance_id, handle)
    }

    /// Insert a resource entry and return its slot index.
    pub fn insert<T: Send + 'static>(
        &self,
        entry: T,
        owner: Option<ResourceId>,
        kind: ResourceType,
    ) -> Result<usize, RegistryError> {
        let owner = self.process_id()?.or(owner);
        let entry = self.registry.add(entry, owner, kind)?;
        let resource_id = entry.0;
        let slot = self.insert_instance_handle(resource_id)?;
        self.registry.record_guest_slot(resource_id, slot);
        Ok(slot)
    }

    /// Insert a resource ID and return its slot index.
    pub fn insert_id(&self, id: ResourceId) -> Result<usize, RegistryError> {
        let slot = self.insert_instance_handle(id)?;
        self.registry.record_guest_slot(id, slot);
        Ok(slot)
    }

    /// Retrieve the entry for the given slot.
    pub fn entry(&self, idx: usize) -> Option<ResourceId> {
        self.resolve_instance_handle(idx)
    }
}

impl Drop for InstanceRegistry {
    fn drop(&mut self) {
        if let Some(mb) = self.mailbox() {
            mb.close();
        }
        self.registry.discard(self.instance_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn detach_slot_returns_resource_id_without_dropping() {
        let registry = Registry::new();
        let resource = registry
            .add(5u32, None, ResourceType::Other)
            .expect("insert resource");
        let id = resource.into_id();

        let mut instance = registry.instance().expect("instance registry");
        let slot = instance.insert_id(id).expect("insert id");
        let detached = instance.detach_slot(slot).expect("detach handle");
        assert_eq!(detached, id);

        let value = registry
            .with(ResourceHandle::<u32>::new(detached), |value| *value)
            .expect("resource present");
        assert_eq!(value, 5);
    }

    #[test]
    fn shared_handle_is_stable_and_cleared_on_remove() {
        let registry = Registry::new();
        let resource = registry
            .add(10u32, None, ResourceType::Other)
            .expect("insert resource");
        let id = resource.into_id();

        let handle_a = registry.share_handle(id).expect("share handle");
        let handle_b = registry.share_handle(id).expect("share handle");
        assert_eq!(handle_a, handle_b);

        let removed = registry.remove(ResourceHandle::<u32>::new(id));
        assert_eq!(removed, Some(10));

        assert!(registry.resolve_shared(handle_a).is_none());
        assert!(registry.shared_handle(id).is_none());
    }

    #[test]
    fn instance_process_relation_is_recorded() {
        let registry = Registry::new();
        let process = registry
            .add((), None, ResourceType::Process)
            .expect("insert process");
        let process_id = process.into_id();

        let mut instance = registry.instance().expect("instance registry");
        let instance_id = instance.instance_id;
        instance.set_process_id(process_id);

        assert_eq!(registry.instance_process(instance_id), Some(process_id));
        assert_eq!(registry.process_instance(process_id), Some(instance_id));
        assert_eq!(registry.owner(instance_id), Some(process_id));
    }

    #[test]
    fn parent_child_relation_roundtrip() {
        let registry = Registry::new();
        let parent = registry
            .add((), None, ResourceType::Other)
            .expect("insert parent")
            .into_id();
        let child = registry
            .add((), None, ResourceType::Other)
            .expect("insert child")
            .into_id();

        registry.record_parent(child, parent);
        assert_eq!(registry.parent(child), Some(parent));
        assert!(registry.children(parent).contains(&child));

        registry.discard(child);
        assert!(!registry.children(parent).contains(&child));
    }

    #[test]
    fn owned_resources_updates_on_remove() {
        let registry = Registry::new();
        let owner = registry
            .add((), None, ResourceType::Other)
            .expect("insert owner")
            .into_id();
        let first = registry
            .add(5u32, Some(owner), ResourceType::Other)
            .expect("insert owned")
            .into_id();
        let second = registry
            .add(6u64, Some(owner), ResourceType::Other)
            .expect("insert owned")
            .into_id();

        let owned = registry.owned_resources(owner);
        assert!(owned.contains(&first));
        assert!(owned.contains(&second));

        registry.remove(ResourceHandle::<u32>::new(first));
        let owned = registry.owned_resources(owner);
        assert!(!owned.contains(&first));
        assert!(owned.contains(&second));
    }

    #[test]
    fn future_handle_roundtrip() {
        let registry = Registry::new();
        let mut instance = registry.instance().expect("instance registry");
        let state = FutureSharedState::<GuestResult<Vec<u8>>>::new();
        let handle = instance
            .insert_future(Arc::clone(&state))
            .expect("insert future");

        let resolved = instance.future_state(handle).expect("future state");
        assert!(Arc::ptr_eq(&state, &resolved));

        let removed = instance.remove_future(handle).expect("remove future");
        assert!(Arc::ptr_eq(&state, &removed));
        assert!(instance.future_state(handle).is_none());
    }

    #[test]
    fn instance_handle_reuse() {
        let registry = Registry::new();
        let mut instance = registry.instance().expect("instance registry");
        let _slot_a = instance
            .insert(1u32, None, ResourceType::Other)
            .expect("insert resource");
        let slot_b = instance
            .insert(2u32, None, ResourceType::Other)
            .expect("insert resource");
        let removed = instance.remove::<u32>(slot_b).expect("remove resource");
        assert_eq!(removed, 2);

        let slot_c = instance
            .insert(3u32, None, ResourceType::Other)
            .expect("insert resource");
        assert_eq!(slot_c, slot_b);
    }

    #[test]
    fn registrar_inserts_into_instance_table_and_sets_owner() {
        let registry = Registry::new();
        let process = registry
            .add((), None, ResourceType::Process)
            .expect("insert process");
        let process_id = process.into_id();

        let mut instance = registry.instance().expect("instance registry");
        instance.set_process_id(process_id);
        let registrar = instance.registrar();

        let slot = registrar
            .insert(42u32, None, ResourceType::Other)
            .expect("registrar insert");
        let resource_id = instance.entry(slot).expect("entry present");
        let value = registry
            .with(ResourceHandle::<u32>::new(resource_id), |value| *value)
            .expect("resource present");
        assert_eq!(value, 42);
        assert_eq!(registry.owner(resource_id), Some(process_id));
    }
}
