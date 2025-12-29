use std::path::PathBuf;

use thiserror::Error;
// use wasmtime::Linker;

// use crate::{KernelError, registry::InstanceRegistry};

pub trait ModuleStoreReadCapability {
    fn read(&self, module_id: &str) -> Result<Vec<u8>, ModuleStoreError>;
}

// @todo Should this capability be linked?
// pub trait ModuleStoreReadLinker: ModuleStoreReadCapability {
//     fn link_read(&self, linker: &mut Linker<InstanceRegistry>) -> Result<(), KernelError> {
//         todo!()
//     }
// }

#[derive(Error, Debug)]
pub enum ModuleStoreError {
    #[error("Path validation failed for {0}: {1}")]
    InvalidPath(PathBuf, String),
    #[error("Error reading filesytem: {0}")]
    Filesystem(String),
}

// impl<T> ModuleStoreReadLinker for T where T: ModuleStoreReadCapability + 'static {}

impl From<ModuleStoreError> for i32 {
    fn from(value: ModuleStoreError) -> Self {
        match value {
            ModuleStoreError::InvalidPath(_, _) => -300,
            ModuleStoreError::Filesystem(_) => -301,
        }
    }
}
