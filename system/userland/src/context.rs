//! Guest environment handle for read-only lookups.

use core::future::Future;

use crate::{DependencyId, FromHandle, driver::DriverError, singleton};
use selium_abi::GuestResourceId;

/// Descriptor that identifies a singleton dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyDescriptor {
    /// Human-readable dependency name.
    pub name: &'static str,
    /// Stable identifier for the dependency.
    pub id: DependencyId,
}

impl DependencyDescriptor {
    /// Create a new descriptor from a name and precomputed identifier.
    pub const fn new(name: &'static str, id: DependencyId) -> Self {
        Self { name, id }
    }
}

/// Dependency that can be resolved from the guest environment.
pub trait Dependency: Sized {
    /// Handle type required to build the dependency.
    type Handle: FromHandle<Handles = GuestResourceId>;
    /// Error type used by the implementor.
    type Error: std::error::Error;

    /// Static descriptor used to locate the dependency.
    const DESCRIPTOR: DependencyDescriptor;

    /// Build the dependency from the raw handle.
    fn from_handle(handle: Self::Handle) -> impl Future<Output = Result<Self, Self::Error>>;
}

/// Read-only view of the guest environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context {
    _private: (),
}

impl Context {
    /// Return the current guest environment handle.
    pub fn current() -> Self {
        Self { _private: () }
    }

    /// Look up a singleton dependency by type.
    pub async fn singleton<T>(&self) -> Result<T, T::Error>
    where
        T: Dependency,
        T::Error: From<DriverError>,
    {
        let raw = singleton::lookup(T::DESCRIPTOR.id).await?;
        let handle = unsafe { T::Handle::from_handle(raw) };
        T::from_handle(handle).await
    }

    /// Look up a singleton dependency and trap on failure.
    pub async fn require<T>(&self) -> T
    where
        T: Dependency,
        T::Error: From<DriverError>,
    {
        match self.singleton::<T>().await {
            Ok(value) => value,
            Err(err) => panic!("dependency {} lookup failed: {err}", T::DESCRIPTOR.name),
        }
    }
}
