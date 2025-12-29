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

pub type ResourceId = usize;
type GuestFuture = Arc<FutureSharedState<GuestResult<Vec<u8>>>>;
type GuestFutureTable = Vec<Option<GuestFuture>>;

#[derive(Clone)]
pub struct ResourceHandle<T>(ResourceId, PhantomData<T>);

#[derive(Clone)]
pub struct InstanceRegistrar {
    registry: Arc<Registry>,
    process_id: Arc<Mutex<Option<ResourceId>>>,
    table: Arc<Mutex<Vec<Option<ResourceId>>>>,
}

/// A single resource in the [`Registry`].
pub struct Resource {
    data: Arc<Mutex<Option<Box<dyn Any + Send>>>>,
    owner: Option<ResourceId>,
    kind: ResourceType,
    /// Tracing span for resource attribution.
    span: Span,
}

/// High-level classification of a resource stored in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Process,
    Channel,
    Reader,
    Writer,
    Session,
    Network,
    Other,
}

/// Metadata describing a registered resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceMetadata {
    pub id: ResourceId,
    pub owner: Option<ResourceId>,
    pub kind: ResourceType,
}

/// Registry of guest resources.
pub struct Registry {
    resources: Slab<Resource>,
    shared: Mutex<Vec<Option<ResourceId>>>,
}

pub struct InstanceRegistry {
    /// Pointer to global registry
    registry: Arc<Registry>,
    /// Instance's resource table
    table: Arc<Mutex<Vec<Option<ResourceId>>>>,
    /// Process identifier associated with this instance.
    process_id: Arc<Mutex<Option<ResourceId>>>,
    /// Waker mailbox instance for guest async support
    mailbox: Option<&'static GuestMailbox>,
    /// Guest-visible future table
    futures: GuestFutureTable,
    /// Arbitrary per-instance extension data.
    extensions: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    /// Resource limits applied to this store.
    limits: StoreLimits,
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
}

/// Stable identity associated with a running process instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessIdentity(ResourceId);

impl ProcessIdentity {
    pub fn new(id: ResourceId) -> Self {
        Self(id)
    }

    /// Return the raw numeric representation of this identity.
    pub fn raw(&self) -> ResourceId {
        self.0
    }
}

impl<T> ResourceHandle<T> {
    pub fn new(id: ResourceId) -> ResourceHandle<T> {
        Self(id, PhantomData)
    }

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
            shared: Mutex::new(Vec::new()),
        });

        // Reserve the first ID (id=0) for system use
        registry.resources.insert(Resource {
            data: Arc::new(Mutex::new(Some(Box::new(())))),
            owner: None,
            kind: ResourceType::Other,
            span: Self::resource_span(ResourceType::Other, None),
        });

        registry
    }

    /// Create an [`InstanceRegistry`] view tied to this registry.
    pub fn instance(self: &Arc<Self>) -> InstanceRegistry {
        InstanceRegistry {
            registry: self.clone(),
            table: Arc::new(Mutex::new(Vec::new())),
            process_id: Arc::new(Mutex::new(None)),
            mailbox: None,
            futures: Vec::new(),
            extensions: HashMap::new(),
            limits: StoreLimits::default(),
        }
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
            owner,
            kind,
            span: Self::resource_span(kind, owner),
        };
        let raw = self
            .resources
            .insert(r)
            .ok_or(RegistryError::CapacityExhausted)?;
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
            owner,
            kind,
            span: Self::resource_span(kind, owner),
        };

        let id = self
            .resources
            .insert(r)
            .ok_or(RegistryError::CapacityExhausted)?;
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
        self.clear_shared(id.0);
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
        self.clear_shared(id);
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

    pub fn share_handle(&self, id: ResourceId) -> Result<GuestResourceId, RegistryError> {
        let shared = {
            let mut table = self
                .shared
                .lock()
                .map_err(|_| RegistryError::LockPoisoned)?;

            if let Some((index, _)) = table
                .iter()
                .enumerate()
                .find(|(_, entry)| matches!(entry, Some(stored) if *stored == id))
            {
                GuestResourceId::try_from(index).map_err(|_| RegistryError::CapacityExhausted)
            } else if let Some((index, slot)) = table
                .iter_mut()
                .enumerate()
                .find(|(_, entry)| entry.is_none())
            {
                *slot = Some(id);
                GuestResourceId::try_from(index).map_err(|_| RegistryError::CapacityExhausted)
            } else {
                let index = table.len();
                table.push(Some(id));
                GuestResourceId::try_from(index).map_err(|_| RegistryError::CapacityExhausted)
            }
        }?;

        self.record_shared_handle(id, shared);

        Ok(shared)
    }

    pub fn resolve_shared(&self, handle: GuestResourceId) -> Option<ResourceId> {
        let idx = usize::try_from(handle).ok()?;
        let table = self.shared.lock().ok()?;
        let resolved = table.get(idx).and_then(|entry| *entry);
        if let Some(id) = resolved
            && let Some(resource) = self.resources.get(id)
        {
            debug!(parent: &resource.span, shared_handle = handle, "resolve shared handle");
        }
        resolved
    }

    /// Fetch metadata for a resource.
    pub fn metadata(&self, id: ResourceId) -> Option<ResourceMetadata> {
        self.resources.get(id).map(|resource| ResourceMetadata {
            id,
            owner: resource.owner,
            kind: resource.kind,
        })
    }

    fn clear_shared(&self, id: ResourceId) {
        if let Ok(mut table) = self.shared.lock() {
            for entry in table.iter_mut() {
                if matches!(entry, Some(stored) if *stored == id) {
                    *entry = None;
                }
            }
        }
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
        if let Some(resource) = self.resources.get(id) {
            resource.span.record("parent_id", field::display(parent));
            debug!(parent: &resource.span, parent_id = %parent, "resource parent linked");
        }
    }
}

impl InstanceRegistry {
    pub fn load_mailbox(&mut self, mb: &'static GuestMailbox) {
        self.mailbox = Some(mb);
    }

    pub fn registrar(&self) -> InstanceRegistrar {
        InstanceRegistrar {
            registry: self.registry.clone(),
            process_id: Arc::clone(&self.process_id),
            table: Arc::clone(&self.table),
        }
    }

    pub fn refresh_mailbox(&self, base: usize) {
        if let Some(mb) = self.mailbox {
            mb.refresh_base(base);
        }
    }

    pub fn close_mailbox(&self) {
        if let Some(mb) = self.mailbox {
            mb.close();
        }
    }

    pub fn limits_mut(&mut self) -> &mut StoreLimits {
        &mut self.limits
    }

    pub fn set_memory_limit(&mut self, bytes: usize) {
        self.limits = StoreLimitsBuilder::new().memory_size(bytes).build();
    }

    fn allocate_slot(&self, table: &mut Vec<Option<ResourceId>>) -> usize {
        if let Some((idx, _slot)) = table
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.is_none())
        {
            return idx;
        }

        table.push(None);
        table.len() - 1
    }

    /// Insert a resource entry and return its slot index.
    pub fn insert<T: Send + 'static>(
        &mut self,
        entry: T,
        owner: Option<ResourceId>,
        kind: ResourceType,
    ) -> Result<usize, RegistryError> {
        let mut table = self.table.lock().map_err(|_| RegistryError::LockPoisoned)?;
        let slot = self.allocate_slot(&mut table);
        let mut resource_id = None;
        if let Some(slot_entry) = table.get_mut(slot) {
            let owner = self.process_id()?.or(owner);
            let entry = self.registry.add(entry, owner, kind)?;
            resource_id = Some(entry.0);
            *slot_entry = Some(entry.0);
        }
        drop(table);
        if let Some(resource_id) = resource_id {
            self.registry.record_guest_slot(resource_id, slot);
        }
        Ok(slot)
    }

    /// Insert a resource ID and return its slot index.
    pub fn insert_id(&mut self, id: ResourceId) -> Result<usize, RegistryError> {
        let mut table = self.table.lock().map_err(|_| RegistryError::LockPoisoned)?;
        let slot = self.allocate_slot(&mut table);
        let mut resource_id = None;
        if let Some(slot_entry) = table.get_mut(slot) {
            *slot_entry = Some(id);
            resource_id = Some(id);
        }
        drop(table);
        if let Some(resource_id) = resource_id {
            self.registry.record_guest_slot(resource_id, slot);
        }
        Ok(slot)
    }

    /// Retrieve the entry for the given slot.
    pub fn entry(&self, idx: usize) -> Option<ResourceId> {
        let table = self.table.lock().ok()?;
        table.get(idx).and_then(|entry| *entry)
    }

    /// Borrow a resource by table index and apply a closure.
    pub fn with<T: 'static, R>(&self, idx: ResourceId, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let table = self.table.lock().ok()?;
        let resource = (*table.get(idx)?)?;
        self.registry
            .with::<T, R>(ResourceHandle(resource, PhantomData), f)
    }

    /// Attach custom extension data to the instance.
    pub fn insert_extension<T: Any + Send + Sync>(&mut self, value: T) {
        self.extensions.insert(TypeId::of::<T>(), Box::new(value));
    }

    /// Borrow extension data by type.
    pub fn extension<T: Any>(&self) -> Option<&T> {
        self.extensions
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
    }

    /// Mutably borrow extension data by type.
    pub fn extension_mut<T: Any>(&mut self) -> Option<&mut T> {
        self.extensions
            .get_mut(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_mut::<T>())
    }

    /// Remove extension data by type, returning ownership.
    pub fn remove_extension<T: Any>(&mut self) -> Option<T> {
        self.extensions
            .remove(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast::<T>().ok())
            .map(|boxed| *boxed)
    }

    /// Remove an entry by slot index.
    pub fn remove<T: 'static>(&mut self, idx: usize) -> Option<T> {
        let mut table = self.table.lock().ok()?;
        table
            .get_mut(idx)?
            .take()
            .and_then(|id| self.registry.remove(ResourceHandle(id, PhantomData)))
    }

    /// Remove a slot entry without deleting the underlying resource.
    pub fn detach_slot(&mut self, idx: usize) -> Option<ResourceId> {
        let mut table = self.table.lock().ok()?;
        let resource_id = table.get_mut(idx)?.take();
        drop(table);
        if let Some(resource_id) = resource_id {
            self.registry.record_slot_detached(resource_id, idx);
        }
        resource_id
    }

    /// Produce a waker for the specified guest task.
    pub fn waker(&self, task_id: usize) -> Waker {
        self.mailbox
            .expect("mailbox is not instantiated")
            .waker(task_id)
    }

    /// Access the guest mailbox backing async wake-ups.
    pub fn mailbox(&self) -> Option<&'static GuestMailbox> {
        self.mailbox
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
        if let Ok(mut id) = self.process_id.lock() {
            *id = Some(process_id);
        }
    }

    fn process_id(&self) -> Result<Option<ResourceId>, RegistryError> {
        self.process_id
            .lock()
            .map(|guard| *guard)
            .map_err(|_| RegistryError::LockPoisoned)
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
    pub fn insert_future(&mut self, state: Arc<FutureSharedState<GuestResult<Vec<u8>>>>) -> usize {
        if let Some((idx, slot)) = self
            .futures
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.is_none())
        {
            *slot = Some(state);
            return idx;
        }

        self.futures.push(Some(state));
        self.futures.len() - 1
    }

    /// Retrieve the shared state for a given future handle.
    pub(crate) fn future_state(
        &self,
        handle: usize,
    ) -> Option<Arc<FutureSharedState<GuestResult<Vec<u8>>>>> {
        self.futures.get(handle)?.clone()
    }

    /// Remove a future handle, returning the shared state if present.
    pub fn remove_future(
        &mut self,
        handle: usize,
    ) -> Option<Arc<FutureSharedState<GuestResult<Vec<u8>>>>> {
        self.futures.get_mut(handle)?.take()
    }
}

impl Drop for InstanceRegistry {
    fn drop(&mut self) {
        if let Some(mb) = self.mailbox {
            mb.close();
        }
    }
}

impl InstanceRegistrar {
    fn allocate_slot(&self, table: &mut Vec<Option<ResourceId>>) -> usize {
        if let Some((idx, _slot)) = table
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.is_none())
        {
            return idx;
        }

        table.push(None);
        table.len() - 1
    }

    pub fn insert<T: Send + 'static>(
        &self,
        entry: T,
        owner: Option<ResourceId>,
        kind: ResourceType,
    ) -> Result<usize, RegistryError> {
        let mut table = self.table.lock().map_err(|_| RegistryError::LockPoisoned)?;
        let slot = self.allocate_slot(&mut table);
        let mut resource_id = None;
        if let Some(slot_entry) = table.get_mut(slot) {
            let owner = self.process_id()?.or(owner);
            let entry = self.registry.add(entry, owner, kind)?;
            resource_id = Some(entry.0);
            *slot_entry = Some(entry.0);
        }
        drop(table);
        if let Some(resource_id) = resource_id {
            self.registry.record_guest_slot(resource_id, slot);
        }
        Ok(slot)
    }

    pub fn insert_id(&self, id: ResourceId) -> Result<usize, RegistryError> {
        let mut table = self.table.lock().map_err(|_| RegistryError::LockPoisoned)?;
        let slot = self.allocate_slot(&mut table);
        let mut resource_id = None;
        if let Some(slot_entry) = table.get_mut(slot) {
            *slot_entry = Some(id);
            resource_id = Some(id);
        }
        drop(table);
        if let Some(resource_id) = resource_id {
            self.registry.record_guest_slot(resource_id, slot);
        }
        Ok(slot)
    }

    pub fn entry(&self, idx: usize) -> Option<ResourceId> {
        let table = self.table.lock().ok()?;
        table.get(idx).and_then(|entry| *entry)
    }

    fn process_id(&self) -> Result<Option<ResourceId>, RegistryError> {
        self.process_id
            .lock()
            .map(|guard| *guard)
            .map_err(|_| RegistryError::LockPoisoned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_slot_returns_resource_id_without_dropping() {
        let registry = Registry::new();
        let resource = registry
            .add(5u32, None, ResourceType::Other)
            .expect("insert resource");
        let id = resource.into_id();

        let mut instance = registry.instance();
        let slot = instance.insert_id(id).expect("insert id");
        let detached = instance.detach_slot(slot).expect("detach handle");
        assert_eq!(detached, id);

        let value = registry
            .with(ResourceHandle::<u32>::new(detached), |value| *value)
            .expect("resource present");
        assert_eq!(value, 5);
    }
}
