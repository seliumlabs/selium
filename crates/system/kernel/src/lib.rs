use std::{
    any::{Any, TypeId},
    collections::HashMap,
    num::TryFromIntError,
    sync::Arc,
};

use thiserror::Error;

use crate::registry::RegistryError;

pub mod drivers;
pub mod futures;
pub mod guest_async;
pub mod guest_data;
pub mod mailbox;
pub mod operation;
pub mod registry;
pub mod session;

pub struct Kernel {
    capabilities: HashMap<TypeId, Arc<dyn Any>>,
}

#[derive(Default)]
pub struct KernelBuilder {
    capabilities: HashMap<TypeId, Arc<dyn Any>>,
}

#[derive(Error, Debug)]
pub enum KernelError {
    #[error("Linker error")]
    LinkerError(#[from] wasmtime::Error),
    #[error("Could not access guest memory")]
    MemoryAccess(#[from] wasmtime::MemoryAccessError),
    #[error("Guest did not reserve enough memory for this call")]
    MemoryCapacity,
    #[error("Could not retrieve guest memory from `Caller`")]
    MemoryMissing,
    #[error("Could not convert int to usize")]
    IntConvert(#[from] TryFromIntError),
    #[error("Invalid resource handle provided by guest")]
    InvalidHandle,
    #[error("Registry error")]
    Registry(#[from] RegistryError),
    #[error("Driver error: {0}")]
    Driver(String),
}

impl Kernel {
    pub fn build() -> KernelBuilder {
        KernelBuilder::default()
    }

    pub fn get<C: 'static>(&self) -> Option<&C> {
        self.capabilities
            .get(&TypeId::of::<C>())
            .and_then(|cap| cap.downcast_ref::<C>())
    }
}

impl KernelBuilder {
    pub fn add_capability<C: 'static>(&mut self, capability: Arc<C>) -> Arc<C> {
        self.capabilities
            .insert(TypeId::of::<C>(), capability.clone());
        capability
    }

    pub fn build(self) -> Result<Kernel, KernelError> {
        Ok(Kernel {
            capabilities: self.capabilities,
        })
    }
}
