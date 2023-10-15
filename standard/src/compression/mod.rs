//! Implementations for many widely used compression algorithms, including
//! DEFLATE, lz4, zstd and brotli.
//!
//! `Selium Labs` makes a best effort to include support for a generous selection of
//! popular and effective compression algorithms and implementations. These offerings
//! can reduce the time (and pain!) of curating the many compression libraries available
//! on crates.io, and reading extensive documentation in order to implement the library
//! in your own application.
//!
//! While the burden of implementation is handled by each offering in the compression module,
//! a level of flexibility is still afforded to users, by allowing specific properties of
//! applicable algorithms, such as the compression level, to be configured via a high-level
//! interface. Certain algorithms may also feature popular implementations, which can be toggled,
//! such as the gzip and zlib implementations derived from the DEFLATE algorithm.
//!
//! # Support for custom implementations
//!
//! Is there a popular compression algorithm that we've missed? One option is to raise a request
//! via the Selium monorepo issue register. However, if you require immediate support, or if you
//! perhaps use a proprietary compression algorithm that you'd either like to remain closed-source,
//! or is simply too niche to be added to the Selium Standard offerings, you can quickly, and
//! easily add support via the `Compression` and `Decompression` traits in the traits module.
//!
//! The `Selium` client's stream builders expect any type that implements the respective traits, be
//! it `Compress` for a `Publisher` stream, or `Decompress` for a `Subscriber` stream. The
//! standard compression algorithms also implement these traits.
//!
//! ## Example
//!
//! As a contrived example, we'll add an implementation for one of the simplest and most classic
//! compression algorithms, Run-Length Encoding (RLE). Using a sequence of characters, for example,
//! you would take an input string of `AAAAABBBCCCCCCDDAA`, and produce a new sequence of `A5B3C6A2`.
//!
//! To begin, let's create a new unit struct called `RunLengthEncoder` that derives `Clone` (to
//! allow `Publisher` streams using the struct to be cloned/forked):
//!
//! ```
//! #[derive(Clone)]
//! pub struct RunLengthEncoder;
//! ```
//!
//! Next, we'll implement the `Compress` trait for our new type:
//!
//! ```
//! # #[derive(Clone)]
//! # pub struct RunLengthEncoder;
//! # use anyhow::{Result, Context};
//! # use bytes::{Bytes, BytesMut};
//! # use selium_std::traits::compression::Compress;
//! impl Compress for RunLengthEncoder {
//!   fn compress(&self, input: Bytes) -> Result<Bytes> {
//!       // Make sure we have at least one byte in the input sequence.
//!       let first = input.first().context("Cannot compress empty sequence.")?;
//!
//!       let buffer = BytesMut::new();
//!       let mut occurrences = 1u8;
//!
//!       // Put the first byte into the buffer to start tallying
//!       buffer.put_u8(first);
//!
//!       for byte in input {
//!           // Get the last byte we inserted into the buffer.
//!           // Safety: We know this should never panic.
//!           let last_byte = buffer.last().unwrap();
//!
//!           if last_byte == byte {
//!               // If we've encountered the same byte again, increment the
//!               // occurrences tally.
//!               occurrences += 1;  
//!           } else {
//!               // Otherwise, put the last number of occurrences to complete
//!               // the pair, and then put the next byte to start tallying
//!               // occurrences again.
//!               buffer.extend_from_slice(&[occurrences, *byte]) ;
//!               occurrences = 1;
//!           }
//!       }
//!
//!       // Once we've broken out of the loop, put the number of occurrences onto the
//!       // buffer to finalise the sequence.
//!       buffer.put_u8(occurrences);
//!
//!       Ok(buffer)
//!   }
//! }
//! ```
//!
//! Finally, implement `Decompress` for `RunLengthEncoder` to handle the decompression side of the
//! equation.
//!
//! ```
//! # #[derive(Clone)]
//! # pub struct RunLengthEncoder;
//! # use anyhow::{Result, Context};
//! # use bytes::{Bytes, BytesMut};
//! # use selium_std::traits::compression::Decompress;
//! impl Decompress for RunLengthEncoder {
//!     fn decompress(&self, input: Bytes) -> Result<Bytes> {
//!         // Make sure we have an even amount of bytes in the sequence
//!         if !(input.len() & 1) {
//!             bail!("Invalid RLE sequence");
//!         }
//!
//!         let mut output = BytesMut::new();
//!
//!         for i in input.len() / 2 {
//!            // Pop the byte representing the number of occurrences
//!            let occurrences = input.get_u8();
//!
//!            // Pop the next byte in the compressed sequence
//!            let byte = input.get_u8();
//!
//!            // Unpack the compressed byte into the output `occurrences`
//!            // amount of times
//!            output.extend_from_slice(vec![occurrences; byte]);
//!         }
//!
//!         Ok(output.into())
//!     }
//! }
//! ```
//!
//! The full code, including imports and usage, should look like the following:
//!
//!
//! ```
//! use bytes:{Bytes, BytesMut};
//! use anyhow::{Result, Context};
//! use bytes::{Bytes, BytesMut};
//! use selium_std::traits::compression::Decompress;
//!
//! #[derive(Clone)]
//! pub struct RunLengthEncoder;
//!
//! impl Compress for RunLengthEncoder {
//!   fn compress(&self, input: Bytes) -> Result<Bytes> {
//!       let first = input.first().context("Cannot compress empty sequence.")?;
//!       let buffer = BytesMut::new();
//!       let mut occurrences = 1u8;
//!
//!       buffer.put_u8(first);
//!
//!       for byte in input {
//!           let last_byte = buffer.last().unwrap();
//!
//!           if last_byte == byte {
//!               occurrences += 1;  
//!           } else {
//!               buffer.extend_from_slice(&[occurrences, *byte]) ;
//!               occurrences = 1;
//!           }
//!       }
//!
//!       buffer.put_u8(occurrences);
//!
//!       Ok(buffer)
//!   }
//!
//! impl Decompress for RunLengthEncoder {
//!     fn decompress(&self, input: Bytes) -> Result<Bytes> {
//!         if !(input.len() & 1) {
//!             bail!("Invalid RLE sequence");
//!         }
//!
//!         let mut output = BytesMut::new();
//!
//!         for i in input.len() / 2 {
//!            let occurrences = input.get_u8();
//!            let byte = input.get_u8();
//!            output.extend_from_slice(vec![occurrences; byte]);
//!         }
//!
//!         Ok(output.into())
//!     }
//! }
//!
//! fn main() {
//!     let input = Bytes::from("AAAAAAAABBBBCCCCCDDEAA");
//!     let expected = Bytes::from("A8B4C5D2E1A2");
//!     let encoder = RunLengthEncoder;
//!
//!     let compressed = encoder.compress(input.clone()).unwrap();
//!     assert_eq!(compressed, expected);
//!
//!     let decompressed = encoder.decompress(compressed);
//!     assert_eq!(decompressed, input);
//! }
//! ```
//!
//! Now your `RunLengthEncoder` type can be used with Selium streams, which will automatically
//! compress/decompress messages for you, by hooking into the `Compress` and `Decompress`
//! implementations.
//!

pub mod brotli;
pub mod deflate;
pub mod lz4;
pub mod zstd;

#[cfg(test)]
mod test {
    use super::*;
    use crate::traits::compression::{Compress, CompressionLevel, Decompress};
    use bytes::Bytes;
    use fake::faker::lorem::en::Sentence;
    use fake::Fake;

    fn generate_payload() -> Bytes {
        let payload: String = Sentence(0..1).fake();
        Bytes::from(payload)
    }

    #[test]
    fn zlib_fastest() {
        let payload = generate_payload();

        let compressed = deflate::DeflateComp::zlib()
            .fastest()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::DeflateDecomp::zlib()
            .decompress(compressed)
            .unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zlib_balanced() {
        let payload = generate_payload();

        let compressed = deflate::DeflateComp::zlib()
            .balanced()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::DeflateDecomp::zlib()
            .decompress(compressed)
            .unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zlib_highest_ratio() {
        let payload = generate_payload();

        let compressed = deflate::DeflateComp::zlib()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::DeflateDecomp::zlib()
            .decompress(compressed)
            .unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_fastest() {
        let payload = generate_payload();

        let compressed = deflate::DeflateComp::gzip()
            .fastest()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::DeflateDecomp::gzip()
            .decompress(compressed)
            .unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_balanced() {
        let payload = generate_payload();

        let compressed = deflate::DeflateComp::gzip()
            .balanced()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::DeflateDecomp::gzip()
            .decompress(compressed)
            .unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_highest_ratio() {
        let payload = generate_payload();

        let compressed = deflate::DeflateComp::gzip()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::DeflateDecomp::gzip()
            .decompress(compressed)
            .unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zstd_fastest() {
        let payload = generate_payload();

        let compressed = zstd::ZstdComp::new()
            .fastest()
            .compress(payload.clone())
            .unwrap();

        let output = zstd::ZstdDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zstd_balanced() {
        let payload = generate_payload();

        let compressed = zstd::ZstdComp::new()
            .balanced()
            .compress(payload.clone())
            .unwrap();

        let output = zstd::ZstdDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zstd_highest_ratio() {
        let payload = generate_payload();

        let compressed = zstd::ZstdComp::new()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = zstd::ZstdDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_text_fastest() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::text()
            .fastest()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_text_balanced() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::text()
            .balanced()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_text_highest_ratio() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::text()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_generic_fastest() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::generic()
            .fastest()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_generic_balanced() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::generic()
            .balanced()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_generic_highest_ratio() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::generic()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_font_fastest() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::font()
            .fastest()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_font_balanced() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::font()
            .balanced()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn brotli_font_highest_ratio() {
        let payload = generate_payload();

        let compressed = brotli::BrotliComp::font()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = brotli::BrotliDecomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn lz4() {
        let payload = generate_payload();
        let compressed = lz4::Lz4Comp.compress(payload.clone()).unwrap();
        let output = lz4::Lz4Decomp.decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }
}
