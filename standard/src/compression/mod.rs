pub mod deflate;
pub mod zstd;

#[cfg(test)]
mod test {
    use super::*;
    use bytes::Bytes;
    use fake::faker::lorem::en::Sentence;
    use fake::Fake;
    use selium_traits::compression::{Compress, Decompress};

    fn generate_payload() -> Bytes {
        let payload: String = Sentence(0..1).fake();
        Bytes::from(payload)
    }

    #[test]
    fn zlib_fastest() {
        let payload = generate_payload();

        let compressed = deflate::comp::zlib()
            .fastest()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::decomp::zlib().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zlib_balanced() {
        let payload = generate_payload();

        let compressed = deflate::comp::zlib()
            .balanced()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::decomp::zlib().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zlib_highest_ratio() {
        let payload = generate_payload();

        let compressed = deflate::comp::zlib()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::decomp::zlib().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_fastest() {
        let payload = generate_payload();

        let compressed = deflate::comp::gzip()
            .fastest()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::decomp::gzip().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_balanced() {
        let payload = generate_payload();

        let compressed = deflate::comp::gzip()
            .balanced()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::decomp::gzip().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_highest_ratio() {
        let payload = generate_payload();

        let compressed = deflate::comp::gzip()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = deflate::decomp::gzip().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zstd_fastest() {
        let payload = generate_payload();
        let compressed = zstd::comp::fastest().compress(payload.clone()).unwrap();
        let output = zstd::decomp::new().decompress(compressed).unwrap();

        assert_eq!(payload, output)
    }

    #[test]
    fn zstd_balanced() {
        let payload = generate_payload();
        let compressed = zstd::comp::balanced().compress(payload.clone()).unwrap();
        let output = zstd::decomp::new().decompress(compressed).unwrap();

        assert_eq!(payload, output)
    }

    #[test]
    fn zstd_highest_ratio() {
        let payload = generate_payload();

        let compressed = zstd::comp::highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = zstd::decomp::new().decompress(compressed).unwrap();

        assert_eq!(payload, output)
    }
}
