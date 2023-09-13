#[derive(Debug)]
pub enum DeflateLibrary {
    Gzip,
    Zlib,
}

impl Default for DeflateLibrary {
    fn default() -> Self {
        Self::Gzip
    }
}
