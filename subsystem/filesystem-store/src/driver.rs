use std::sync::Arc;

use selium_kernel::drivers::module_store::{ModuleStoreError, ModuleStoreReadCapability};

use crate::FilesystemStore;

pub struct FilesystemStoreReadDriver {
    inner: FilesystemStore,
}

impl FilesystemStoreReadDriver {
    pub fn new(store: FilesystemStore) -> Arc<Self> {
        Arc::new(Self { inner: store })
    }
}

impl ModuleStoreReadCapability for FilesystemStoreReadDriver {
    fn read(&self, module_id: &str) -> Result<Vec<u8>, ModuleStoreError> {
        self.inner.fetch(module_id)
    }
}
