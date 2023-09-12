pub mod brotli;
pub mod deflate;
pub mod lz4;
pub mod zstd;

#[cfg(test)]
mod test {
    use super::*;
    use bytes::Bytes;
    use fake::faker::lorem::en::Sentence;
    use fake::Fake;
    use selium_traits::compression::{Compress, CompressionLevel, Decompress};

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
