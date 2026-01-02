//! Shared Application Binary Interface helpers for Selium host ↔ guest calls.

use rkyv::{
    Archive, Deserialize, Serialize,
    api::high::{HighDeserializer, HighSerializer, HighValidator},
    rancor::Error as RancorError,
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};
use std::{
    cmp::Ordering,
    fmt::{Display, Formatter},
};
use thiserror::Error;

pub mod hostcalls;
mod io;
mod net;
mod process;
mod session;

// pub use external::*;
pub use hostcalls::*;
pub use io::*;
pub use net::*;
pub use process::*;
pub use session::*;

/// Guest pointer-sized signed integer.
pub type GuestInt = i32;
/// Guest pointer-sized unsigned integer.
pub type GuestUint = u32;
/// Guest-facing resource identifiers.
pub type GuestResourceId = u64;
/// Guest pointer-sized atomic unsigned integer.
pub type GuestAtomicUint = std::sync::atomic::AtomicU32;

/// Size, in bytes, of a guest machine word.
pub const WORD_SIZE: usize = 4;
/// Marker bit used to differentiate driver poll results from payload lengths.
const DRIVER_RESULT_SPECIAL_FLAG: GuestUint = 1 << 31;
/// Maximum payload length representable in a driver poll result word.
pub const DRIVER_RESULT_READY_MAX: GuestUint = DRIVER_RESULT_SPECIAL_FLAG - 1;
/// Word signalling the host is still processing the driver future.
pub const DRIVER_RESULT_PENDING: GuestUint = DRIVER_RESULT_SPECIAL_FLAG;
/// Error code indicating the payload buffer contains a driver error string.
pub const DRIVER_ERROR_MESSAGE_CODE: GuestUint = 1;

/// Shared constants describing the guest↔host waker mailbox layout.
pub mod mailbox {
    use super::{GuestAtomicUint, GuestUint, WORD_SIZE};

    /// Number of wake entries the ring buffer can hold.
    pub const CAPACITY: GuestUint = 256;
    /// Size in bytes of each ring entry.
    pub const SLOT_SIZE: usize = core::mem::size_of::<GuestUint>();
    /// Offset of the ready flag within the mailbox region.
    pub const FLAG_OFFSET: usize = WORD_SIZE;
    /// Offset of the head cursor within the mailbox region.
    pub const HEAD_OFFSET: usize = WORD_SIZE * 2;
    /// Offset of the tail cursor within the mailbox region.
    pub const TAIL_OFFSET: usize = WORD_SIZE * 3;
    /// Offset of the ring buffer within the mailbox region.
    pub const RING_OFFSET: usize = WORD_SIZE * 4;

    /// Atomic cell used for each mailbox slot.
    pub type Cell = GuestAtomicUint;
}

/// Size in bytes of the guest mailbox region (head/tail cursors + wake ring).
const MAILBOX_BYTES: usize =
    mailbox::RING_OFFSET + (mailbox::CAPACITY as usize * mailbox::SLOT_SIZE);

/// Default offset used by [`CallPlan`] when laying out transient buffers.
///
/// The mailbox occupies the first page of guest memory; buffers must start after it to avoid
/// clobbering the wake ring.
pub const DEFAULT_BUFFER_BASE: GuestUint = MAILBOX_BYTES as GuestUint;

/// Trait for values that can be encoded using Selium's rkyv settings.
pub trait RkyvEncode:
    Archive + for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RancorError>>
{
}

impl<T> RkyvEncode for T where
    T: Archive + for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RancorError>>
{
}

/// Decoded driver poll result.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DriverPollResult {
    /// Host completed the call and wrote `len` bytes into the result buffer.
    Ready(GuestUint),
    /// Host has not completed execution; guest should poll again later.
    Pending,
    /// Host reported an error; `code` identifies the error class.
    Error(GuestUint),
}

/// Kernel capability identifiers shared between host and guest.
#[repr(u8)]
#[derive(
    Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Archive, Serialize, Deserialize,
)]
#[rkyv(bytecheck())]
pub enum Capability {
    SessionLifecycle = 0,
    ChannelLifecycle = 1,
    ChannelReader = 2,
    ChannelWriter = 3,
    ProcessLifecycle = 4,
    NetBind = 5,
    NetAccept = 6,
    NetConnect = 7,
    NetRead = 8,
    NetWrite = 9,
}

impl Capability {
    /// All capabilities understood by the Selium kernel ABI.
    pub const ALL: [Capability; 10] = [
        Capability::SessionLifecycle,
        Capability::ChannelLifecycle,
        Capability::ChannelReader,
        Capability::ChannelWriter,
        Capability::ProcessLifecycle,
        Capability::NetBind,
        Capability::NetAccept,
        Capability::NetConnect,
        Capability::NetRead,
        Capability::NetWrite,
    ];
}

/// Error produced when decoding a capability identifier fails.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("unknown capability identifier")]
pub struct CapabilityDecodeError;

/// Scalar value kinds supported by the ABI.
#[derive(Debug, Clone, Copy, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum AbiScalarValue {
    /// 8-bit signed integer.
    I8(i8),
    /// 8-bit unsigned integer.
    U8(u8),
    /// 16-bit signed integer.
    I16(i16),
    /// 16-bit unsigned integer.
    U16(u16),
    /// 32-bit signed integer.
    I32(i32),
    /// 32-bit unsigned integer.
    U32(u32),
    /// 64-bit signed integer.
    I64(i64),
    /// 64-bit unsigned integer.
    U64(u64),
    /// 32-bit IEEE float.
    F32(f32),
    /// 64-bit IEEE float.
    F64(f64),
}

/// Scalar kinds that can be part of an ABI signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum AbiScalarType {
    /// 8-bit signed integer.
    I8,
    /// 8-bit unsigned integer.
    U8,
    /// 16-bit signed integer.
    I16,
    /// 16-bit unsigned integer.
    U16,
    /// 32-bit signed integer.
    I32,
    /// 32-bit unsigned integer.
    U32,
    /// 64-bit signed integer.
    I64,
    /// 64-bit unsigned integer.
    U64,
    /// 32-bit IEEE float.
    F32,
    /// 64-bit IEEE float.
    F64,
}

/// Logical parameter kinds supported by the ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum AbiParam {
    /// An immediate scalar value.
    Scalar(AbiScalarType),
    /// A byte buffer passed via (ptr, len) pair.
    Buffer,
}

/// Description of a guest entrypoint's parameters and results.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct AbiSignature {
    params: Vec<AbiParam>,
    results: Vec<AbiParam>,
}

/// Values supplied for a call.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum AbiValue {
    /// Scalar argument.
    Scalar(AbiScalarValue),
    /// Buffer argument (passed via pointer/length).
    Buffer(Vec<u8>),
}

/// Planned argument + buffer layout suitable for host execution.
#[derive(Debug, Clone)]
pub struct CallPlan {
    args: Vec<AbiScalarValue>,
    writes: Vec<MemoryWrite>,
    base_offset: GuestUint,
}

/// Host-side write required to materialise a buffer value.
#[derive(Debug, Clone)]
pub struct MemoryWrite {
    /// Offset inside the guest linear memory.
    pub offset: GuestUint,
    /// Bytes that should be copied to guest memory.
    pub bytes: Vec<u8>,
}

/// Errors raised when a call plan cannot be created.
#[derive(Debug, Error)]
pub enum CallPlanError {
    /// Number of supplied values does not match the signature.
    #[error("parameter count mismatch: expected {expected}, got {actual}")]
    ParameterCount { expected: usize, actual: usize },
    /// A value does not satisfy the parameter kind.
    #[error("value mismatch at index {index}: {reason}")]
    ValueMismatch { index: usize, reason: &'static str },
    /// A buffer could not be laid out due to arithmetic overflow.
    #[error("buffer layout overflowed guest address space")]
    BufferOverflow,
}

/// Errors returned when serialising or deserialising rkyv payloads.
#[derive(Debug, Error)]
pub enum RkyvError {
    /// Encoding the payload failed.
    #[error("rkyv encode failed: {0}")]
    Encode(String),
    /// Decoding the payload failed.
    #[error("rkyv decode failed: {0}")]
    Decode(String),
}

impl Capability {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for Capability {
    type Error = CapabilityDecodeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Capability::SessionLifecycle),
            1 => Ok(Capability::ChannelLifecycle),
            2 => Ok(Capability::ChannelReader),
            3 => Ok(Capability::ChannelWriter),
            4 => Ok(Capability::ProcessLifecycle),
            5 => Ok(Capability::NetBind),
            6 => Ok(Capability::NetAccept),
            7 => Ok(Capability::NetConnect),
            8 => Ok(Capability::NetRead),
            9 => Ok(Capability::NetWrite),
            _ => Err(CapabilityDecodeError),
        }
    }
}

impl From<Capability> for u8 {
    fn from(value: Capability) -> Self {
        value.as_u8()
    }
}

impl Display for Capability {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::SessionLifecycle => write!(f, "SessionLifecycle"),
            Capability::ChannelLifecycle => write!(f, "ChannelLifecycle"),
            Capability::ChannelReader => write!(f, "ChannelReader"),
            Capability::ChannelWriter => write!(f, "ChannelWriter"),
            Capability::ProcessLifecycle => write!(f, "ProcessLifecycle"),
            Capability::NetBind => write!(f, "NetBind"),
            Capability::NetAccept => write!(f, "NetAccept"),
            Capability::NetConnect => write!(f, "NetConnect"),
            Capability::NetRead => write!(f, "NetRead"),
            Capability::NetWrite => write!(f, "NetWrite"),
        }
    }
}

impl AbiScalarValue {
    pub fn kind(&self) -> AbiScalarType {
        match self {
            AbiScalarValue::I8(_) => AbiScalarType::I8,
            AbiScalarValue::U8(_) => AbiScalarType::U8,
            AbiScalarValue::I16(_) => AbiScalarType::I16,
            AbiScalarValue::U16(_) => AbiScalarType::U16,
            AbiScalarValue::I32(_) => AbiScalarType::I32,
            AbiScalarValue::U32(_) => AbiScalarType::U32,
            AbiScalarValue::I64(_) => AbiScalarType::I64,
            AbiScalarValue::U64(_) => AbiScalarType::U64,
            AbiScalarValue::F32(_) => AbiScalarType::F32,
            AbiScalarValue::F64(_) => AbiScalarType::F64,
        }
    }
}

impl AbiSignature {
    pub fn new(params: Vec<AbiParam>, results: Vec<AbiParam>) -> Self {
        Self { params, results }
    }

    pub fn params(&self) -> &[AbiParam] {
        &self.params
    }

    pub fn results(&self) -> &[AbiParam] {
        &self.results
    }
}

impl From<i32> for AbiValue {
    fn from(value: i32) -> Self {
        Self::Scalar(AbiScalarValue::I32(value))
    }
}

impl From<u16> for AbiValue {
    fn from(value: u16) -> Self {
        Self::Scalar(AbiScalarValue::U16(value))
    }
}

impl From<i16> for AbiValue {
    fn from(value: i16) -> Self {
        Self::Scalar(AbiScalarValue::I16(value))
    }
}

impl From<u8> for AbiValue {
    fn from(value: u8) -> Self {
        Self::Scalar(AbiScalarValue::U8(value))
    }
}

impl From<i8> for AbiValue {
    fn from(value: i8) -> Self {
        Self::Scalar(AbiScalarValue::I8(value))
    }
}

impl From<u32> for AbiValue {
    fn from(value: u32) -> Self {
        Self::Scalar(AbiScalarValue::U32(value))
    }
}

impl From<i64> for AbiValue {
    fn from(value: i64) -> Self {
        Self::Scalar(AbiScalarValue::I64(value))
    }
}

impl From<u64> for AbiValue {
    fn from(value: u64) -> Self {
        Self::Scalar(AbiScalarValue::U64(value))
    }
}

impl From<f32> for AbiValue {
    fn from(value: f32) -> Self {
        Self::Scalar(AbiScalarValue::F32(value))
    }
}

impl From<f64> for AbiValue {
    fn from(value: f64) -> Self {
        Self::Scalar(AbiScalarValue::F64(value))
    }
}

impl From<Vec<u8>> for AbiValue {
    fn from(value: Vec<u8>) -> Self {
        Self::Buffer(value)
    }
}

impl CallPlan {
    pub fn new(signature: &AbiSignature, values: &[AbiValue]) -> Result<Self, CallPlanError> {
        Self::with_base(signature, values, DEFAULT_BUFFER_BASE)
    }

    pub fn with_base(
        signature: &AbiSignature,
        values: &[AbiValue],
        base_offset: GuestUint,
    ) -> Result<Self, CallPlanError> {
        if signature.params.len() != values.len() {
            return Err(CallPlanError::ParameterCount {
                expected: signature.params.len(),
                actual: values.len(),
            });
        }

        let mut args = Vec::new();
        let mut writes = Vec::new();
        let mut cursor = base_offset;

        for (index, (param, value)) in signature.params.iter().zip(values).enumerate() {
            match (param, value) {
                (AbiParam::Scalar(expected), AbiValue::Scalar(actual)) => {
                    if actual.kind() != *expected {
                        return Err(CallPlanError::ValueMismatch {
                            index,
                            reason: "scalar type mismatch",
                        });
                    }
                    append_scalar_args(&mut args, *expected, *actual, index)?;
                }
                (AbiParam::Buffer, AbiValue::Buffer(bytes)) => {
                    let len_u32 =
                        u32::try_from(bytes.len()).map_err(|_| CallPlanError::BufferOverflow)?;
                    if len_u32 == 0 {
                        args.push(AbiScalarValue::I32(0));
                        args.push(AbiScalarValue::I32(0));
                        continue;
                    }

                    let ptr = cursor;
                    cursor = align_offset(cursor, bytes.len())?;

                    args.push(AbiScalarValue::I32(i32::try_from(ptr).map_err(|_| {
                        CallPlanError::ValueMismatch {
                            index,
                            reason: "pointer does not fit i32",
                        }
                    })?));
                    args.push(AbiScalarValue::I32(i32::try_from(len_u32).map_err(
                        |_| CallPlanError::ValueMismatch {
                            index,
                            reason: "length does not fit i32",
                        },
                    )?));
                    writes.push(MemoryWrite {
                        offset: ptr,
                        bytes: bytes.clone(),
                    });
                }
                (AbiParam::Scalar(_), AbiValue::Buffer(_)) => {
                    return Err(CallPlanError::ValueMismatch {
                        index,
                        reason: "expected scalar, found buffer",
                    });
                }
                (AbiParam::Buffer, AbiValue::Scalar(_)) => {
                    return Err(CallPlanError::ValueMismatch {
                        index,
                        reason: "expected buffer, found scalar",
                    });
                }
            }
        }

        Ok(Self {
            args,
            writes,
            base_offset,
        })
    }

    pub fn params(&self) -> &[AbiScalarValue] {
        &self.args
    }

    pub fn memory_writes(&self) -> &[MemoryWrite] {
        &self.writes
    }

    pub fn base_offset(&self) -> GuestUint {
        self.base_offset
    }
}

impl From<DriverPollResult> for GuestUint {
    fn from(value: DriverPollResult) -> Self {
        match value {
            DriverPollResult::Ready(len) => len,
            DriverPollResult::Pending => DRIVER_RESULT_PENDING,
            DriverPollResult::Error(code) => driver_encode_error(code),
        }
    }
}

impl TryFrom<GuestUint> for DriverPollResult {
    type Error = CapabilityDecodeError;

    fn try_from(word: GuestUint) -> Result<Self, <Self as TryFrom<GuestUint>>::Error> {
        Ok(driver_decode_result(word))
    }
}

/// Encode a value into rkyv bytes using Selium's settings.
pub fn encode_rkyv<T>(value: &T) -> Result<Vec<u8>, RkyvError>
where
    T: RkyvEncode,
{
    rkyv::to_bytes::<RancorError>(value)
        .map(|bytes| bytes.into_vec())
        .map_err(|err| RkyvError::Encode(err.to_string()))
}

/// Decode a value from rkyv bytes using Selium's settings.
pub fn decode_rkyv<T>(bytes: &[u8]) -> Result<T, RkyvError>
where
    T: Archive + Sized,
    for<'a> T::Archived: 'a
        + Deserialize<T, HighDeserializer<RancorError>>
        + rkyv::bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    rkyv::from_bytes::<T, RancorError>(bytes).map_err(|err| RkyvError::Decode(err.to_string()))
}

/// Encode a human-readable driver error message for guest consumption.
pub fn encode_driver_error_message(message: &str) -> Result<Vec<u8>, RkyvError> {
    let encoded = encode_rkyv(&message.to_string())?;
    let len = u32::try_from(encoded.len()).map_err(|_| {
        RkyvError::Encode("driver error message length does not fit u32".to_string())
    })?;
    let mut bytes = Vec::with_capacity(encoded.len() + 4);
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(&encoded);
    Ok(bytes)
}

/// Decode a driver error message payload written by the kernel.
pub fn decode_driver_error_message(bytes: &[u8]) -> Result<String, RkyvError> {
    let prefix = bytes
        .get(..4)
        .ok_or_else(|| RkyvError::Decode("driver error message missing length".to_string()))?;
    let len = u32::from_le_bytes(
        prefix
            .try_into()
            .map_err(|_| RkyvError::Decode("driver error message length malformed".to_string()))?,
    ) as usize;
    let payload = bytes.get(4..4 + len).ok_or_else(|| {
        RkyvError::Decode("driver error message length exceeds buffer".to_string())
    })?;
    decode_rkyv::<String>(payload)
}

pub fn driver_encode_ready(len: GuestUint) -> Option<GuestUint> {
    if len > DRIVER_RESULT_READY_MAX {
        None
    } else {
        Some(len)
    }
}

pub fn driver_encode_error(mut code: GuestUint) -> GuestUint {
    if code == 0 {
        code = DRIVER_ERROR_MESSAGE_CODE;
    }
    DRIVER_RESULT_SPECIAL_FLAG | (code & DRIVER_RESULT_READY_MAX)
}

pub fn driver_decode_result(word: GuestUint) -> DriverPollResult {
    if word < DRIVER_RESULT_SPECIAL_FLAG {
        DriverPollResult::Ready(word)
    } else if word == DRIVER_RESULT_SPECIAL_FLAG {
        DriverPollResult::Pending
    } else {
        DriverPollResult::Error(word & DRIVER_RESULT_READY_MAX)
    }
}

fn append_scalar_args(
    args: &mut Vec<AbiScalarValue>,
    expected: AbiScalarType,
    value: AbiScalarValue,
    index: usize,
) -> Result<(), CallPlanError> {
    match (expected, value) {
        (AbiScalarType::I8, AbiScalarValue::I8(v)) => {
            args.push(AbiScalarValue::I32(i32::from(v)));
        }
        (AbiScalarType::U8, AbiScalarValue::U8(v)) => {
            args.push(AbiScalarValue::I32(i32::from(v)));
        }
        (AbiScalarType::I16, AbiScalarValue::I16(v)) => {
            args.push(AbiScalarValue::I32(i32::from(v)))
        }
        (AbiScalarType::U16, AbiScalarValue::U16(v)) => {
            args.push(AbiScalarValue::I32(i32::from(v)))
        }
        (AbiScalarType::I32, AbiScalarValue::I32(v)) => args.push(AbiScalarValue::I32(v)),
        (AbiScalarType::U32, AbiScalarValue::U32(v)) => {
            args.push(AbiScalarValue::I32(i32::from_ne_bytes(v.to_ne_bytes())))
        }
        (AbiScalarType::F32, AbiScalarValue::F32(v)) => args.push(AbiScalarValue::F32(v)),
        (AbiScalarType::F64, AbiScalarValue::F64(v)) => args.push(AbiScalarValue::F64(v)),
        (AbiScalarType::I64, AbiScalarValue::I64(v)) => {
            let (lo, hi) = split_i64(v);
            args.push(AbiScalarValue::I32(lo));
            args.push(AbiScalarValue::I32(hi));
        }
        (AbiScalarType::U64, AbiScalarValue::U64(v)) => {
            let (lo, hi) = split_u64(v);
            args.push(AbiScalarValue::I32(lo));
            args.push(AbiScalarValue::I32(hi));
        }
        _ => {
            return Err(CallPlanError::ValueMismatch {
                index,
                reason: "scalar type mismatch",
            });
        }
    }

    Ok(())
}

fn split_i64(value: i64) -> (i32, i32) {
    let bytes = value.to_le_bytes();
    let lo = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let hi = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (lo, hi)
}

fn split_u64(value: u64) -> (i32, i32) {
    let bytes = value.to_le_bytes();
    let lo = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let hi = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (lo, hi)
}

fn align_offset(current: GuestUint, len_bytes: usize) -> Result<GuestUint, CallPlanError> {
    let len = GuestUint::try_from(len_bytes).map_err(|_| CallPlanError::BufferOverflow)?;
    let align = GuestUint::try_from(WORD_SIZE).expect("word size fits into GuestUint");
    let rounded = match len.cmp(&GuestUint::from(0u8)) {
        Ordering::Equal => GuestUint::from(0u8),
        _ => {
            let remainder = len % align;
            if remainder == GuestUint::from(0u8) {
                len
            } else {
                len.checked_add(align - remainder)
                    .ok_or(CallPlanError::BufferOverflow)?
            }
        }
    };
    current
        .checked_add(rounded)
        .ok_or(CallPlanError::BufferOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_buffer_base_leaves_mailbox_intact() {
        let mailbox_end =
            (mailbox::RING_OFFSET + (mailbox::CAPACITY as usize * mailbox::SLOT_SIZE)) as GuestUint;
        assert!(
            DEFAULT_BUFFER_BASE >= mailbox_end,
            "default buffer base {DEFAULT_BUFFER_BASE} overlaps mailbox (ends at {mailbox_end})"
        );
    }

    #[test]
    fn call_plan_flattens_integer_widths() {
        let signature = AbiSignature::new(
            vec![
                AbiParam::Scalar(AbiScalarType::U16),
                AbiParam::Scalar(AbiScalarType::U64),
            ],
            Vec::new(),
        );
        let values = vec![
            AbiValue::Scalar(AbiScalarValue::U16(7)),
            AbiValue::Scalar(AbiScalarValue::U64(0x0102_0304_0506_0708)),
        ];

        let plan = CallPlan::new(&signature, &values).expect("call plan creation should succeed");
        let params = plan.params();

        assert_eq!(
            params.len(),
            3,
            "u16 should flatten to one word and u64 to two"
        );

        let first = params[0];
        assert_eq!(first, AbiScalarValue::I32(7));

        let lo_word = match params[1] {
            AbiScalarValue::I32(v) => u32::from_ne_bytes(v.to_ne_bytes()),
            other => panic!("expected low u64 word, found {other:?}"),
        };
        let hi_word = match params[2] {
            AbiScalarValue::I32(v) => u32::from_ne_bytes(v.to_ne_bytes()),
            other => panic!("expected high u64 word, found {other:?}"),
        };

        let combined = (u64::from(hi_word) << 32) | u64::from(lo_word);
        assert_eq!(combined, 0x0102_0304_0506_0708);
    }
}
