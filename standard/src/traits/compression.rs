use anyhow::Result;
use bytes::Bytes;

/// Interface to adapt compression implementations for use with Selium.
pub trait Compress {
    /// Fallibly compress the `input` bytes, and output as bytes.
    fn compress(&self, input: Bytes) -> Result<Bytes>;
}

/// Interface to adapt compression implementations for use with Selium.
pub trait Decompress {
    /// Fallibly decompress the `input` bytes, and output as bytes.
    fn decompress(&self, input: Bytes) -> Result<Bytes>;
}

/// Interface for applicable compression algorithms and implementations that allow users to
/// specify a compression level.
pub trait CompressionLevel {
    /// Sets the compression level to the highest possible level for the algorithm/implementation.
    fn highest_ratio(self) -> Self;
    /// Sets the compression level to a balance between speed and size. 
    ///
    /// Typically set to the default compression level for the specific algorithm/implementation.
    fn balanced(self) -> Self;
    /// Sets the compression level to the fastest possible speed supported by the
    /// algorithm/implementation.
    fn fastest(self) -> Self;
    /// Allows a user to set the compression level to a specific level.
    fn level(self, level: u32) -> Self;
}
