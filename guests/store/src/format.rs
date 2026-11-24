//! The store's own on-volume layout and record framing.
//!
//! Nothing here touches the host: the module is pure so the store's format can
//! be exercised natively (recovery tests, host-side seeding) as well as from
//! the `wasm32-unknown-unknown` guest.
//!
//! Layout — every object is a flat volume name under the tenant it belongs to:
//!
//! ```text
//! log:<name>#<segment>      a log's records, framed, one volume per segment
//! checkpoint:<name>         a log's named checkpoints, framed
//! blob:<store>/<blob-id>    blob content (the id is content-derived)
//! manifest:<store>          a blob store's manifests, framed
//! ```
//!
//! `:` and `#` and `/` are separators here, never data: [`encode`] percent-
//! escapes everything outside `[A-Za-z0-9._-]`, so a parsed name round-trips.

use sha2::{Digest, Sha256};

/// Character separating a blob store name from a blob id.
const BLOB_SEP: char = '/';
/// Byte length of a frame's header (body length + checksum).
pub const FRAME_HEADER_BYTES: usize = 8;
/// Character separating an object's kind from its name in a volume name.
const KIND_SEP: char = ':';
/// Character separating a log name from its segment index.
const SEGMENT_SEP: char = '#';

/// A durable-log record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Monotonic log sequence number.
    pub sequence: u64,
    /// Record timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// User-supplied header key/value pairs.
    pub headers: Vec<(String, String)>,
    /// Record payload bytes.
    pub payload: Vec<u8>,
}

/// A malformed or unreadable store artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatError(pub String);

/// Bounds-checked little-endian reader.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], FormatError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| FormatError("field length overflow".to_string()))?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| FormatError("truncated record body".to_string()))?;
        self.offset = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, FormatError> {
        let raw: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_error| FormatError("bad u32".to_string()))?;
        Ok(u32::from_le_bytes(raw))
    }

    fn u64(&mut self) -> Result<u64, FormatError> {
        let raw: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_error| FormatError("bad u64".to_string()))?;
        Ok(u64::from_le_bytes(raw))
    }

    fn bytes(&mut self) -> Result<&'a [u8], FormatError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

/// Content-derived blob id: the lowercase hex SHA-256 of the blob bytes.
pub fn blob_id(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

/// Finds the valid content-addressed prefix of a recovered blob volume.
///
/// Blob volumes are append-only, so an interrupted retry can leave a valid
/// acknowledged blob followed by a torn or duplicate tail. Hashing each prefix
/// incrementally recovers that acknowledged prefix without deleting it. The
/// returned length is the longest prefix whose SHA-256 is `blob_id`; callers
/// must never expose bytes beyond it.
pub fn valid_blob_prefix(bytes: &[u8], blob_id: &str) -> Option<usize> {
    let mut hasher = Sha256::new();
    let mut valid = (hex_digest(hasher.clone().finalize()) == blob_id).then_some(0);
    for (index, byte) in bytes.iter().enumerate() {
        hasher.update([*byte]);
        if hex_digest(hasher.clone().finalize()) == blob_id {
            valid = Some(index + 1);
        }
    }
    valid
}

/// Volume name holding blob `blob_id` of blob store `store`.
pub fn blob_volume(store: &str, blob_id: &str) -> String {
    format!(
        "blob{}{}{}{}",
        KIND_SEP,
        encode(store),
        BLOB_SEP,
        encode(blob_id)
    )
}

/// Volume name holding the named checkpoints of log `log`.
pub fn checkpoint_volume(log: &str) -> String {
    format!("checkpoint{}{}", KIND_SEP, encode(log))
}

/// IEEE CRC-32, the frame checksum.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _bit in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Inverts [`encode`].
pub fn decode(encoded: &str) -> Result<String, FormatError> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = *bytes
            .get(index)
            .ok_or_else(|| FormatError(format!("unterminated name {encoded:?}")))?;
        if byte == b'%' {
            let hi = hex_digit(*bytes.get(index + 1).ok_or_else(|| truncated(encoded))?)?;
            let lo = hex_digit(*bytes.get(index + 2).ok_or_else(|| truncated(encoded))?)?;
            out.push(hi * 16 + lo);
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_error| FormatError(format!("non-utf8 name {encoded:?}")))
}

/// Decodes a checkpoint body.
pub fn decode_checkpoint(body: &[u8]) -> Result<(String, u64), FormatError> {
    let mut reader = Reader::new(body);
    let name = String::from_utf8(reader.bytes()?.to_vec())
        .map_err(|_error| FormatError("non-utf8 checkpoint name".to_string()))?;
    let sequence = reader.u64()?;
    Ok((name, sequence))
}

/// Decodes a manifest body.
pub fn decode_manifest(body: &[u8]) -> Result<(String, String), FormatError> {
    let mut reader = Reader::new(body);
    let name = String::from_utf8(reader.bytes()?.to_vec())
        .map_err(|_error| FormatError("non-utf8 manifest name".to_string()))?;
    let blob_id = String::from_utf8(reader.bytes()?.to_vec())
        .map_err(|_error| FormatError("non-utf8 blob id".to_string()))?;
    Ok((name, blob_id))
}

/// Decodes a [`Record`] body.
pub fn decode_record(body: &[u8]) -> Result<Record, FormatError> {
    let mut reader = Reader::new(body);
    let sequence = reader.u64()?;
    let timestamp_ms = reader.u64()?;
    let header_count = reader.u32()?;
    let mut headers = Vec::with_capacity(header_count as usize);
    for _index in 0..header_count {
        let key = String::from_utf8(reader.bytes()?.to_vec())
            .map_err(|_error| FormatError("non-utf8 header key".to_string()))?;
        let value = String::from_utf8(reader.bytes()?.to_vec())
            .map_err(|_error| FormatError("non-utf8 header value".to_string()))?;
        headers.push((key, value));
    }
    let payload = reader.bytes()?.to_vec();
    Ok(Record {
        sequence,
        timestamp_ms,
        headers,
        payload,
    })
}

/// Percent-encodes every byte outside `[A-Za-z0-9._-]`.
pub fn encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if is_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            push_hex(&mut out, byte);
        }
    }
    out
}

/// Encodes a checkpoint body: `(name, sequence)`.
pub fn encode_checkpoint(name: &str, sequence: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, name.as_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out
}

/// Encodes a manifest body: `(name, blob_id)`.
pub fn encode_manifest(name: &str, blob_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, name.as_bytes());
    put_bytes(&mut out, blob_id.as_bytes());
    out
}

/// Encodes a [`Record`] body (framed by [`frame`]).
pub fn encode_record(record: &Record) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&record.sequence.to_le_bytes());
    out.extend_from_slice(&record.timestamp_ms.to_le_bytes());
    out.extend_from_slice(&(record.headers.len() as u32).to_le_bytes());
    for (key, value) in &record.headers {
        put_bytes(&mut out, key.as_bytes());
        put_bytes(&mut out, value.as_bytes());
    }
    put_bytes(&mut out, &record.payload);
    out
}

/// Wraps a body in a length- and checksum-framed record.
pub fn frame(body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + body.len());
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&crc32(body).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Volume name for segment `segment` of log `log`.
pub fn log_volume(log: &str, segment: u64) -> String {
    format!(
        "log{}{}{}",
        KIND_SEP,
        encode(log),
        log_segment_suffix(segment)
    )
}

/// Volume name holding the manifests of blob store `store`.
pub fn manifest_volume(store: &str) -> String {
    format!("manifest{}{}", KIND_SEP, encode(store))
}

/// Parses a blob volume name back into `(store, blob_id)`.
pub fn parse_blob_volume(name: &str) -> Option<Result<(String, String), FormatError>> {
    let rest = name.strip_prefix(&format!("blob{KIND_SEP}"))?;
    let (store, blob_id) = rest.split_once(BLOB_SEP)?;
    Some(match (decode(store), decode(blob_id)) {
        (Ok(store), Ok(blob_id)) => Ok((store, blob_id)),
        (Err(error), _) | (_, Err(error)) => Err(error),
    })
}

/// Parses a checkpoint volume name back into the log name.
pub fn parse_checkpoint_volume(name: &str) -> Option<Result<String, FormatError>> {
    let rest = name.strip_prefix(&format!("checkpoint{KIND_SEP}"))?;
    // Segment separators never appear in a checkpoint volume name.
    if rest.contains(SEGMENT_SEP) {
        return None;
    }
    Some(decode(rest))
}

/// Parses a log volume name back into `(log, segment)`.
///
/// Returns `None` for any other kind of volume.
pub fn parse_log_volume(name: &str) -> Option<Result<(String, u64), FormatError>> {
    let rest = name.strip_prefix(&format!("log{KIND_SEP}"))?;
    let (encoded, segment) = rest.rsplit_once(SEGMENT_SEP)?;
    let segment = segment.parse::<u64>().ok()?;
    Some(decode(encoded).map(|log| (log, segment)))
}

/// Parses a manifest volume name back into the blob store name.
pub fn parse_manifest_volume(name: &str) -> Option<Result<String, FormatError>> {
    let rest = name.strip_prefix(&format!("manifest{KIND_SEP}"))?;
    if rest.contains(BLOB_SEP) {
        return None;
    }
    Some(decode(rest))
}

/// Scans framed bytes into complete bodies.
///
/// Returns the complete bodies and the offset at which framing stops — either
/// the end of `bytes` (the tail is exactly a complete set of frames) or the
/// offset of a torn frame, which the caller discards.
pub fn scan_frames(bytes: &[u8]) -> (Vec<&[u8]>, usize) {
    let mut bodies = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let Some(header) = bytes.get(offset..offset + FRAME_HEADER_BYTES) else {
            break;
        };
        let Some(length) = header.get(0..4).and_then(|raw| raw.try_into().ok()) else {
            break;
        };
        let Some(checksum) = header.get(4..8).and_then(|raw| raw.try_into().ok()) else {
            break;
        };
        let length = u32::from_le_bytes(length) as usize;
        let checksum = u32::from_le_bytes(checksum);
        let body_start = offset + FRAME_HEADER_BYTES;
        let Some(body) = bytes.get(body_start..body_start + length) else {
            break;
        };
        if crc32(body) != checksum {
            break;
        }
        bodies.push(body);
        offset = body_start + length;
    }
    (bodies, offset)
}

/// Returns the offset of the first torn frame, or `None` when the tail is clean.
pub fn torn_tail(bytes: &[u8]) -> Option<usize> {
    let (_, end) = scan_frames(bytes);
    (end < bytes.len()).then_some(end)
}

fn hex_digit(byte: u8) -> Result<u8, FormatError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(FormatError(format!("bad hex digit {:?}", byte as char))),
    }
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn log_segment_suffix(segment: u64) -> String {
    format!("{SEGMENT_SEP}{segment}")
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        push_hex(&mut out, *byte);
    }
    out
}

/// Pushes one byte as two lowercase hex digits.
fn push_hex(out: &mut String, byte: u8) {
    if let Some(high) = char::from_digit(u32::from(byte >> 4), 16) {
        out.push(high);
    }
    if let Some(low) = char::from_digit(u32::from(byte & 0x0f), 16) {
        out.push(low);
    }
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn truncated(encoded: &str) -> FormatError {
    FormatError(format!("truncated escape in {encoded:?}"))
}
