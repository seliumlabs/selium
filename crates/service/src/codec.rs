use selium_abi::{deframe_bytes, frame_bytes};

use crate::{EncodingError, FlatMsg};

/// Decodes a framed Flatbuffers value received by a guest interface.
pub fn decode_typed<T>(bytes: &[u8]) -> Result<T, EncodingError>
where
    T: FlatMsg,
{
    let payload = deframe_bytes(bytes)?;
    FlatMsg::decode(payload).map_err(EncodingError::Decode)
}

/// Encodes a value as framed Flatbuffers bytes for a guest interface.
pub fn encode_typed<T>(value: &T) -> Result<Vec<u8>, EncodingError>
where
    T: FlatMsg,
{
    frame_bytes(&FlatMsg::encode(value)).map_err(EncodingError::Framing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct DemoPayload {
        message: String,
    }

    impl FlatMsg for DemoPayload {
        fn encode(value: &Self) -> Vec<u8> {
            // Simple encoding: length-prefixed string
            let mut result = Vec::new();
            result.extend_from_slice(&(value.message.len() as u32).to_le_bytes());
            result.extend_from_slice(value.message.as_bytes());
            result
        }

        fn decode(bytes: &[u8]) -> std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
            if bytes.len() < 4 {
                return Err(flatbuffers::InvalidFlatbuffer::ApparentSizeTooLarge);
            }
            let len = u32::from_le_bytes(
                bytes[0..4]
                    .try_into()
                    .map_err(|_error| flatbuffers::InvalidFlatbuffer::ApparentSizeTooLarge)?,
            ) as usize;
            if bytes.len() < 4 + len {
                return Err(flatbuffers::InvalidFlatbuffer::ApparentSizeTooLarge);
            }
            let message = std::str::from_utf8(&bytes[4..4 + len])
                .map_err(|e| flatbuffers::InvalidFlatbuffer::Utf8Error {
                    error: e,
                    range: 4..4 + len,
                    error_trace: Default::default(),
                })?
                .to_string();
            Ok(Self { message })
        }
    }

    #[test]
    fn typed_codec_round_trips() {
        let payload = DemoPayload {
            message: "hello".to_string(),
        };

        let encoded = encode_typed(&payload).expect("encode payload");
        let decoded: DemoPayload = decode_typed(&encoded).expect("decode payload");

        assert_eq!(decoded, payload);
    }
}
