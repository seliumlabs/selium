use anyhow::Result;
use std::path::PathBuf;

pub trait ClientAuth {
    type Output;

    fn with_certificate_authority<T: Into<PathBuf>>(self, ca_path: T) -> Result<Self::Output>;
}
