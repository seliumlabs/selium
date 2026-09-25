//! Frame header for the shared-memory ring protocol.
//!
//! This is a ring-buffer concept, not a transport concept. It lives in
//! `selium-memory` (the lowest common dependency) so both `selium-wire` and
//! `selium-shm` can re-export a single definition without circular
//! dependencies.

use crate::MemoryError;

/// A frame header stored at the start of each message in a ring buffer.
///
/// Length-delimited frame header with a lightweight integrity checksum.
///
/// Layout: `[len: u32 LE] [tag: u32 LE] [flags: u8] [checksum: u32 LE] [pad: 3 bytes]` = 16 bytes
///
/// The checksum is FNV-1a over the first 9 bytes (len, tag, flags), computed at
/// encode time and verified at decode time, so that misaligned reads are
/// detectable. The trailing 3 bytes are zero padding that rounds the header to
/// a clean 16-byte size for pointer maths; they carry no meaning and are
/// ignored on decode.
///
/// **Tag correlation**: In RPC contexts the `tag` field carries the correlation id
/// assigned by the client. All frames belonging to one request (unary reply,
/// server-stream items, bidi-stream items in either direction) share the same
/// correlation tag. This invariant holds across unary, server-streaming, and
/// bidi-streaming patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Payload length in bytes (not including the header).
    pub len: u32,
    /// Semantic tag: `writer_id` in pub/sub contexts, `correlation_id` in RPC contexts.
    pub tag: u32,
    /// Flags for frame metadata.
    pub flags: u8,
}

impl FrameHeader {
    /// Total encoded header size in bytes.
    pub const ENCODED_SIZE: usize = 16;
    /// Frame flag set once the payload bytes are fully written.
    pub const FLAG_READY: u8 = 1;
    /// Frame flag set when a writer abandons a reserved span.
    pub const FLAG_ABORTED: u8 = 1 << 1;
    /// Stream item flag: this frame carries a streaming data item (not the first/last).
    pub const FLAG_STREAM_ITEM: u8 = 1 << 2;
    /// Stream end flag: this frame is the final item in the stream direction.
    pub const FLAG_STREAM_END: u8 = 1 << 3;
    /// Stream cancel flag: the sender requests cancellation of the stream.
    pub const FLAG_STREAM_CANCEL: u8 = 1 << 4;
    /// Stream error flag: this frame terminates the stream with an error.
    ///
    /// The payload carries a UTF-8 error message (not a typed item). Always
    /// combined with [`FLAG_STREAM_END`](Self::FLAG_STREAM_END).
    pub const FLAG_STREAM_ERROR: u8 = 1 << 5;

    /// Encodes the header to a byte array.
    pub fn encode(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[..4].copy_from_slice(&self.len.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.tag.to_le_bytes());
        bytes[8] = self.flags;
        let checksum = Self::checksum(&bytes[..9]);
        bytes[9..13].copy_from_slice(&checksum.to_le_bytes());
        // bytes[13..16] remain zero: trailing padding for a clean 16-byte size.
        bytes
    }

    /// FNV-1a 32-bit hash over the header's first 9 bytes.
    ///
    /// Chosen for being trivially portable to the guest while detecting any single-byte
    /// change and most multi-byte corruption in the header.
    fn checksum(bytes: &[u8]) -> u32 {
        let mut hash = 0x811c_9dc5u32;
        for &byte in bytes {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
        hash
    }

    /// Decodes a header from a byte array, verifying the FNV-1a checksum.
    ///
    /// Returns [`MemoryError::CorruptedHeader`] when the stored checksum does
    /// not match the recomputed value — a misaligned read.
    pub fn decode(bytes: &[u8]) -> crate::Result<Self> {
        if bytes.len() < Self::ENCODED_SIZE {
            return Err(MemoryError::InvalidLayout);
        }
        let stored = u32::from_le_bytes(
            bytes
                .get(9..13)
                .ok_or(MemoryError::InvalidLayout)?
                .try_into()
                .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
        );
        if stored != Self::checksum(bytes.get(..9).ok_or(MemoryError::InvalidLayout)?) {
            return Err(MemoryError::CorruptedHeader);
        }
        let len = u32::from_le_bytes(
            bytes
                .get(..4)
                .ok_or(MemoryError::InvalidLayout)?
                .try_into()
                .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
        );
        let tag = u32::from_le_bytes(
            bytes
                .get(4..8)
                .ok_or(MemoryError::InvalidLayout)?
                .try_into()
                .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
        );
        let flags = bytes.get(8).copied().ok_or(MemoryError::InvalidLayout)?;
        Ok(Self { len, tag, flags })
    }

    /// Returns the total frame size including the header.
    pub fn frame_size(&self) -> u64 {
        Self::ENCODED_SIZE as u64 + self.len as u64
    }

    /// Returns whether this frame has been fully published.
    pub fn is_ready(&self) -> bool {
        self.flags & Self::FLAG_READY != 0
    }

    /// Returns whether this frame represents an abandoned reservation.
    pub fn is_aborted(&self) -> bool {
        self.flags & Self::FLAG_ABORTED != 0
    }

    /// Returns whether this frame is a stream data item.
    pub fn is_stream_item(&self) -> bool {
        self.flags & Self::FLAG_STREAM_ITEM != 0
    }

    /// Returns whether this frame marks the end of a stream direction.
    pub fn is_stream_end(&self) -> bool {
        self.flags & Self::FLAG_STREAM_END != 0
    }

    /// Returns whether this frame requests stream cancellation.
    pub fn is_stream_cancel(&self) -> bool {
        self.flags & Self::FLAG_STREAM_CANCEL != 0
    }

    /// Returns whether this frame terminates the stream with an error.
    pub fn is_stream_error(&self) -> bool {
        self.flags & Self::FLAG_STREAM_ERROR != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_encodes_and_decodes() {
        let header = FrameHeader {
            len: 1024,
            tag: 42,
            flags: 1,
        };
        let encoded = header.encode();
        let decoded = FrameHeader::decode(&encoded).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "unwrap_used lint conflicts with clippy's suggested fix"
    )]
    fn header_requires_full_encoded_size() {
        let header = FrameHeader {
            len: 0,
            tag: 0,
            flags: 0,
        };
        let encoded = header.encode();
        // A short buffer never decodes, regardless of checksum content.
        assert!(FrameHeader::decode(&encoded[..15]).is_err());
        // The all-zeros placeholder without a valid checksum is rejected too:
        // an uncommitted header must be a checksum-valid not-ready frame, not
        // raw zeros, so torn reads stay distinguishable from empty slots.
        assert!(matches!(
            FrameHeader::decode(&[0u8; 16]),
            Err(MemoryError::CorruptedHeader)
        ));
        // A checksum-valid header round-trips.
        assert!(FrameHeader::decode(&encoded).is_ok());
    }

    #[test]
    fn misaligned_header_fails_checksum_verification() {
        let header = FrameHeader {
            len: 1024,
            tag: 42,
            flags: FrameHeader::FLAG_READY,
        };
        let mut encoded = header.encode();
        // Flip one bit in the `len` field: decode must reject it as corrupted.
        encoded[0] ^= 0b0000_0001;
        assert!(matches!(
            FrameHeader::decode(&encoded),
            Err(MemoryError::CorruptedHeader)
        ));

        // A mid-payload read is garbage bytes with a (almost surely) wrong
        // checksum: decode must never accept it as a valid header.
        assert!(matches!(
            FrameHeader::decode(&[0x5a; 16]),
            Err(MemoryError::CorruptedHeader)
        ));
    }

    #[test]
    fn frame_size_includes_header() {
        let header = FrameHeader {
            len: 100,
            tag: 0,
            flags: 0,
        };
        assert_eq!(header.frame_size(), 116);
    }

    #[test]
    fn flags_report_ready_and_aborted_state() {
        let header = FrameHeader {
            len: 0,
            tag: 0,
            flags: FrameHeader::FLAG_READY | FrameHeader::FLAG_ABORTED,
        };

        assert!(header.is_ready());
        assert!(header.is_aborted());
    }

    #[test]
    fn stream_flags_report_independently() {
        let item_header = FrameHeader {
            len: 0,
            tag: 0,
            flags: FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ITEM,
        };
        assert!(item_header.is_ready());
        assert!(item_header.is_stream_item());
        assert!(!item_header.is_stream_end());
        assert!(!item_header.is_stream_cancel());

        let end_header = FrameHeader {
            len: 0,
            tag: 0,
            flags: FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_END,
        };
        assert!(end_header.is_ready());
        assert!(end_header.is_stream_end());
        assert!(!end_header.is_stream_item());

        let cancel_header = FrameHeader {
            len: 0,
            tag: 0,
            flags: FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_CANCEL,
        };
        assert!(cancel_header.is_ready());
        assert!(cancel_header.is_stream_cancel());
        assert!(!cancel_header.is_stream_end());
        assert!(!cancel_header.is_stream_error());

        let error_header = FrameHeader {
            len: 0,
            tag: 0,
            flags: FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ERROR,
        };
        assert!(error_header.is_stream_error());
        assert!(!error_header.is_stream_item());
    }

    #[test]
    fn stream_end_with_item_flag() {
        let header = FrameHeader {
            len: 0,
            tag: 0,
            flags: FrameHeader::FLAG_READY
                | FrameHeader::FLAG_STREAM_ITEM
                | FrameHeader::FLAG_STREAM_END,
        };
        assert!(header.is_stream_item());
        assert!(header.is_stream_end());
    }
}
