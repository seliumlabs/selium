#[derive(Debug, Clone)]
pub enum DeflateLibrary {
    Gzip,
    Zlib,
}

impl Default for DeflateLibrary {
    fn default() -> Self {
        Self::Gzip
    }
}
