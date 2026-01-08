use std::str;

use thiserror::Error;
use wasmtime::{AsContext, Caller};

use crate::{
    KernelError,
    drivers::Capability,
    registry::{InstanceRegistry, RegistryError},
};
use selium_abi::{
    DRIVER_ERROR_MESSAGE_CODE, DRIVER_RESULT_PENDING, RkyvEncode, WORD_SIZE, decode_rkyv,
    driver_encode_error, driver_encode_ready, encode_driver_error_message, encode_rkyv,
};
pub use selium_abi::{GuestInt, GuestUint};

pub type GuestResult<T, E = GuestError> = Result<T, E>;

#[derive(Error, Debug)]
pub enum GuestError {
    // Data errors
    #[error("invalid argument")]
    InvalidArgument,
    #[error("Invalid UTF-8 in guest input")]
    InvalidUtf8,
    #[error("Invalid guest memory slice")]
    MemorySlice,
    #[error("resource not found")]
    NotFound,
    #[error("permission denied")]
    PermissionDenied,

    // System errors
    #[error("The kernel encountered an error. Please report this to your administrator.")]
    Kernel(#[from] KernelError),
    #[error("The kernel Registry encountered an error. Please report this to your administrator.")]
    Registry(#[from] RegistryError),
    #[error("Stable identifier already exists")]
    StableIdExists,
    #[error("internal error: {0}")]
    Subsystem(String),
    #[error("This function would block")]
    WouldBlock,
}

impl GuestError {
    fn encode_for_guest(
        self,
        caller: &mut Caller<'_, InstanceRegistry>,
        ptr: GuestInt,
        len: GuestUint,
    ) -> Result<GuestUint, KernelError> {
        if matches!(self, GuestError::WouldBlock) {
            return Ok(DRIVER_RESULT_PENDING);
        }

        let bytes = encode_driver_error_message(&self.to_string())
            .map_err(|err| KernelError::Driver(err.to_string()))?;
        write_encoded(caller, ptr, len, &bytes)?;
        Ok(driver_encode_error(DRIVER_ERROR_MESSAGE_CODE))
    }
}

pub fn write_poll_result(
    caller: &mut Caller<'_, InstanceRegistry>,
    ptr: GuestInt,
    len: GuestUint,
    result: GuestResult<Vec<u8>>,
) -> Result<GuestUint, KernelError> {
    match result {
        Ok(bytes) => write_encoded(caller, ptr, len, &bytes),
        Err(err) => err.encode_for_guest(caller, ptr, len),
    }
}

pub fn write_rkyv_value<T>(
    caller: &mut Caller<'_, InstanceRegistry>,
    ptr: GuestInt,
    len: GuestUint,
    value: T,
) -> Result<GuestUint, KernelError>
where
    T: RkyvEncode,
{
    let bytes = encode_value(&value)?;
    write_encoded(caller, ptr, len, &bytes)
}

pub fn read_rkyv_value<T>(
    caller: &mut Caller<'_, InstanceRegistry>,
    ptr: GuestInt,
    len: GuestUint,
) -> Result<T, KernelError>
where
    T: rkyv::Archive + Sized,
    for<'a> T::Archived: 'a
        + rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    let bytes = read_guest_bytes(caller, ptr, len)?;
    decode_value(&bytes)
}

fn encode_value<T>(value: &T) -> Result<Vec<u8>, KernelError>
where
    T: RkyvEncode,
{
    encode_rkyv(value).map_err(|err| KernelError::Driver(err.to_string()))
}

fn decode_value<T>(bytes: &[u8]) -> Result<T, KernelError>
where
    T: rkyv::Archive + Sized,
    for<'a> T::Archived: 'a
        + rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    decode_rkyv(bytes).map_err(|err| KernelError::Driver(err.to_string()))
}

fn read_guest_bytes(
    caller: &mut Caller<'_, InstanceRegistry>,
    ptr: GuestInt,
    len: GuestUint,
) -> Result<Vec<u8>, KernelError> {
    let memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or(KernelError::MemoryMissing)?;

    let start = usize::try_from(ptr).map_err(KernelError::IntConvert)?;
    let len = usize::try_from(len).map_err(KernelError::IntConvert)?;
    let end = start.checked_add(len).ok_or(KernelError::MemoryCapacity)?;

    let ctx = caller.as_context();
    let data = memory
        .data(&ctx)
        .get(start..end)
        .ok_or(KernelError::MemoryCapacity)?;
    Ok(data.to_vec())
}

fn write_encoded(
    caller: &mut Caller<'_, InstanceRegistry>,
    ptr: GuestInt,
    len: GuestUint,
    bytes: &[u8],
) -> Result<GuestUint, KernelError> {
    let memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or(KernelError::MemoryMissing)?;
    let capacity = usize::try_from(len).map_err(KernelError::IntConvert)?;
    if capacity < bytes.len() {
        return Err(KernelError::MemoryCapacity);
    }

    let offset = usize::try_from(ptr).map_err(KernelError::IntConvert)?;
    memory.write(caller, offset, bytes)?;

    encode_ready_len(bytes.len())
}

pub fn read_u32(data: &[u8], index: usize) -> GuestResult<u32> {
    let offset = index * WORD_SIZE;
    let bytes = data
        .get(offset..offset + WORD_SIZE)
        .ok_or(GuestError::MemorySlice)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| GuestError::MemorySlice)?,
    ))
}

pub fn read_i32(data: &[u8], index: usize) -> GuestResult<i32> {
    let offset = index * WORD_SIZE;
    let bytes = data
        .get(offset..offset + WORD_SIZE)
        .ok_or(GuestError::MemorySlice)?;
    Ok(i32::from_le_bytes(
        bytes.try_into().map_err(|_| GuestError::MemorySlice)?,
    ))
}

pub fn read_utf8(
    memory: &wasmtime::Memory,
    ctx: &impl AsContext<Data = InstanceRegistry>,
    ptr: GuestInt,
    len: GuestUint,
) -> GuestResult<String> {
    let ptr = usize::try_from(ptr).map_err(|_| GuestError::InvalidArgument)?;
    let len = usize::try_from(len).map_err(|_| GuestError::InvalidArgument)?;
    let data = memory
        .data(ctx)
        .get(ptr..ptr + len)
        .ok_or(GuestError::MemorySlice)?;
    let s = str::from_utf8(data).map_err(|_| GuestError::InvalidUtf8)?;
    Ok(s.to_string())
}

pub fn read_capabilities(
    memory: &wasmtime::Memory,
    ctx: &impl AsContext<Data = InstanceRegistry>,
    ptr: GuestInt,
    count: GuestUint,
) -> GuestResult<Vec<Capability>> {
    let ptr = usize::try_from(ptr).map_err(|_| GuestError::InvalidArgument)?;
    let count = usize::try_from(count).map_err(|_| GuestError::InvalidArgument)?;
    let data = memory
        .data(ctx)
        .get(ptr..ptr + count)
        .ok_or(GuestError::MemorySlice)?;
    let mut caps = Vec::with_capacity(count);
    for byte in data {
        caps.push(Capability::try_from(*byte).map_err(|_| GuestError::InvalidArgument)?);
    }
    Ok(caps)
}
pub fn encode_ready_len(len: usize) -> Result<GuestUint, KernelError> {
    let guest_len = GuestUint::try_from(len).map_err(|_| KernelError::MemoryCapacity)?;
    driver_encode_ready(guest_len).ok_or(KernelError::MemoryCapacity)
}
