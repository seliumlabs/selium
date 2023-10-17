/// Used by [DeflateComp](crate::compression::deflate::DeflateComp) and
/// [DeflateDecomp](crate::compression::deflate::DeflateDecomp) to specify the preferred DEFLATE
/// implementation.
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
