pub mod comp;
pub mod decomp;

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
    fn zstd_fastest() {
        let payload = generate_payload();
        let compressed = comp::fastest().compress(payload.clone()).unwrap();
        let output = decomp::new().decompress(compressed).unwrap();

        assert_eq!(payload, output)
    }

    #[test]
    fn zstd_balanced() {
        let payload = generate_payload();
        let compressed = comp::balanced().compress(payload.clone()).unwrap();
        let output = decomp::new().decompress(compressed).unwrap();

        assert_eq!(payload, output)
    }

    #[test]
    fn zstd_highest_ratio() {
        let payload = generate_payload();
        let compressed = comp::highest_ratio().compress(payload.clone()).unwrap();
        let output = decomp::new().decompress(compressed).unwrap();

        assert_eq!(payload, output)
    }
}
