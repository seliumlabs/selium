pub mod comp;
pub mod decomp;
mod types;

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
        let compressed = comp::zlib().fastest().compress(payload.clone()).unwrap();
        let output = decomp::zlib().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zlib_balanced() {
        let payload = generate_payload();
        let compressed = comp::zlib().balanced().compress(payload.clone()).unwrap();
        let output = decomp::zlib().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn zlib_highest_ratio() {
        let payload = generate_payload();

        let compressed = comp::zlib()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = decomp::zlib().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_fastest() {
        let payload = generate_payload();
        let compressed = comp::gzip().fastest().compress(payload.clone()).unwrap();
        let output = decomp::gzip().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_balanced() {
        let payload = generate_payload();
        let compressed = comp::gzip().balanced().compress(payload.clone()).unwrap();
        let output = decomp::gzip().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }

    #[test]
    fn gzip_highest_ratio() {
        let payload = generate_payload();

        let compressed = comp::gzip()
            .highest_ratio()
            .compress(payload.clone())
            .unwrap();

        let output = decomp::gzip().decompress(compressed).unwrap();

        assert_eq!(payload, output);
    }
}
