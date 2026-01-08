use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

mod driver;
pub use driver::FilesystemStoreReadDriver;
use path_security::validate_path;
use selium_kernel::drivers::module_store::ModuleStoreError;

pub struct FilesystemStore {
    base_dir: PathBuf,
}

impl FilesystemStore {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().into(),
        }
    }

    pub fn fetch(&self, path: impl AsRef<Path>) -> Result<Vec<u8>, ModuleStoreError> {
        let fq_path = validate_path(path.as_ref(), &self.base_dir).map_err(|e| {
            ModuleStoreError::InvalidPath(
                self.base_dir.as_path().join(path.as_ref()),
                e.to_string(),
            )
        })?;

        let mut fh =
            File::open(fq_path).map_err(|e| ModuleStoreError::Filesystem(e.to_string()))?;
        // @todo Set memory limit!
        let mut buf = Vec::new();
        fh.read_to_end(&mut buf)
            .map_err(|e| ModuleStoreError::Filesystem(e.to_string()))?;

        Ok(buf)
    }
}
