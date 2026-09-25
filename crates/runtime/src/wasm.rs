use selium_memory::WASM_PAGE_SIZE;
use wasmtiny::{
    FunctionType, WasmApplication, WasmError, WasmValue,
    runtime::{HostCaller, HostFunc, SharedMemory},
};

use crate::{Error, Result, config::SystemGuestArg, error::map_wasm_error};

/// Decodes raw `WasmValue` argument bytes (the existing child-spawn ABI
/// form) into integer [`SystemGuestArg`]s.
///
/// The signed-to-unsigned casts are bit-preserving: the encode side stores
/// `u64` values into the `I64`/`I32` slots and the decode side restores the
/// original bits.
#[expect(clippy::cast_sign_loss, reason = "bit-preserving ABI round-trip")]
pub(crate) fn decode_integer_arguments(arguments: &[Vec<u8>]) -> Result<Vec<SystemGuestArg>> {
    arguments
        .iter()
        .map(|raw| {
            let Some((value, used)) = WasmValue::from_bytes(raw) else {
                return Err(Error::InvalidEntrypointArgument);
            };
            if used != raw.len() {
                return Err(Error::InvalidEntrypointArgument);
            }
            match value {
                WasmValue::I64(value) => Ok(SystemGuestArg::Integer(value as u64)),
                WasmValue::I32(value) => Ok(SystemGuestArg::Integer(value as u64)),
                _ => Err(Error::InvalidEntrypointArgument),
            }
        })
        .collect()
}

pub(crate) fn decode_wasm_arguments(arguments: &[Vec<u8>]) -> Result<Vec<WasmValue>> {
    arguments
        .iter()
        .map(|argument| {
            let Some((value, used)) = WasmValue::from_bytes(argument) else {
                return Err(Error::InvalidEntrypointArgument);
            };
            if used != argument.len() {
                return Err(Error::InvalidEntrypointArgument);
            }
            Ok(value)
        })
        .collect()
}

/// Encodes a single `WasmValue` into the tagged byte form expected by
/// [`decode_wasm_arguments`].
pub(crate) fn encode_wasm_value(value: WasmValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.to_bytes(&mut bytes);
    bytes
}

pub(crate) fn guest_memory(caller: &HostCaller<'_>) -> wasmtiny::runtime::Result<SharedMemory> {
    caller
        .memory(0)
        .ok_or_else(|| WasmError::Runtime("guest module does not expose memory".to_string()))
}

pub(crate) fn read_guest_memory(
    caller: &HostCaller<'_>,
    ptr: u32,
    len: usize,
) -> wasmtiny::runtime::Result<Vec<u8>> {
    let memory = guest_memory(caller)?;
    let mut bytes = vec![0; len];
    memory
        .lock()
        .map_err(|_lock_err| WasmError::Runtime("guest memory lock poisoned".to_string()))?
        .read(ptr, &mut bytes)?;
    Ok(bytes)
}

pub(crate) fn register_optional_host_function(
    app: &mut WasmApplication,
    module_index: u32,
    import_module: &str,
    name: &str,
    func: Box<dyn HostFunc>,
    func_type: FunctionType,
) -> Result<()> {
    match app.register_host_function(module_index, import_module, name, func, func_type) {
        Ok(()) => Ok(()),
        Err(WasmError::Instantiate(message))
            if message == format!("import {import_module}.{name} not found") =>
        {
            Ok(())
        }
        Err(error) => Err(map_wasm_error(error)),
    }
}

/// Resolves structured [`SystemGuestArg`]s into the flattened `WasmValue`
/// slot list passed to the entrypoint export.
///
/// Integer arguments become a single `I64`; pointer arguments have their
/// payload copied into the guest's linear memory first and become two `I64`
/// slots: `(address, length)`.
pub(crate) fn resolve_entrypoint_arguments(
    app: &mut WasmApplication,
    module_index: u32,
    arguments: &[SystemGuestArg],
) -> Result<Vec<WasmValue>> {
    let mut encoded: Vec<Vec<u8>> = Vec::new();
    for argument in arguments {
        match argument {
            SystemGuestArg::Integer(value) => {
                encoded.push(encode_wasm_value(WasmValue::I64(*value as i64)));
            }
            SystemGuestArg::Pointer(payload) => {
                let address = write_entrypoint_bytes(app, module_index, payload)?;
                encoded.push(encode_wasm_value(WasmValue::I64(address as i64)));
                encoded.push(encode_wasm_value(WasmValue::I64(payload.len() as i64)));
            }
        }
    }
    decode_wasm_arguments(&encoded)
}

pub(crate) fn wasm_i32_arg(args: &[WasmValue], index: usize) -> wasmtiny::runtime::Result<i32> {
    args.get(index)
        .ok_or_else(|| WasmError::Runtime(format!("missing argument {index}")))?
        .i32()
}

pub(crate) fn wasm_i64_arg(args: &[WasmValue], index: usize) -> wasmtiny::runtime::Result<i64> {
    args.get(index)
        .ok_or_else(|| WasmError::Runtime(format!("missing argument {index}")))?
        .i64()
}

pub(crate) fn write_guest_memory(
    caller: &HostCaller<'_>,
    ptr: u32,
    bytes: &[u8],
) -> wasmtiny::runtime::Result<()> {
    let memory = guest_memory(caller)?;
    memory
        .lock()
        .map_err(|_lock_err| WasmError::Runtime("guest memory lock poisoned".to_string()))?
        .write(ptr, bytes)
}

/// Grows the guest's linear memory, copies `bytes` into it, and returns the
/// byte address the entrypoint should be handed as the pointer argument.
fn write_entrypoint_bytes(
    app: &mut WasmApplication,
    module_index: u32,
    bytes: &[u8],
) -> Result<u64> {
    let old_pages = app
        .runtime
        .memory_size(module_index)
        .map_err(map_wasm_error)?
        .unsigned_abs() as usize;

    let delta_pages = bytes.len().div_ceil(WASM_PAGE_SIZE as usize) as u32;
    if delta_pages > 0 {
        app.runtime
            .memory_grow(module_index, delta_pages)
            .map_err(map_wasm_error)?;
    }

    let base = old_pages * WASM_PAGE_SIZE as usize;

    if !bytes.is_empty() {
        let binding = app
            .runtime
            .get_module(module_index)
            .and_then(|module| module.memory_binding(0))
            .ok_or_else(|| Error::Host("guest does not expose a linear memory".to_string()))?;
        let mut memory = binding
            .lock()
            .map_err(|_lock_err| Error::Host("guest memory lock poisoned".to_string()))?;
        memory.write(base as u32, bytes).map_err(map_wasm_error)?;
    }

    Ok(base as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtiny::WasmApplication;

    fn load_module(memory_decl: &str) -> (WasmApplication, u32) {
        let wat =
            format!("(module (memory (export \"memory\") {memory_decl}) (func (export \"boot\")))");
        let module_bytes = wat::parse_str(wat).expect("compile wat module");
        let mut app = WasmApplication::new();
        let index = app
            .load_module_from_memory(&module_bytes)
            .expect("load module");
        app.instantiate(index).expect("instantiate module");
        (app, index)
    }

    #[test]
    fn integer_arguments_encode_as_i64_slots() {
        let (mut app, index) = load_module("1");
        let values = resolve_entrypoint_arguments(
            &mut app,
            index,
            &[
                SystemGuestArg::Integer(0),
                SystemGuestArg::Integer(u32::MAX as u64),
                SystemGuestArg::Integer(u64::MAX),
            ],
        )
        .expect("resolve arguments");
        assert_eq!(
            values,
            vec![
                WasmValue::I64(0),
                WasmValue::I64(u32::MAX as i64),
                WasmValue::I64(u64::MAX as i64),
            ]
        );
    }

    #[test]
    fn pointer_argument_bytes_land_in_guest_memory() {
        let (mut app, index) = load_module("1");
        let payload = b"udp://127.0.0.1:53".to_vec();
        let values = resolve_entrypoint_arguments(
            &mut app,
            index,
            &[
                SystemGuestArg::Integer(42),
                SystemGuestArg::Pointer(payload.clone()),
            ],
        )
        .expect("resolve arguments");

        // Integer first, then (address, length) for the pointer.
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], WasmValue::I64(42));
        let WasmValue::I64(addr) = values[1] else {
            panic!("expected address slot, got {:?}", values[1]);
        };
        let WasmValue::I64(len) = values[2] else {
            panic!("expected length slot, got {:?}", values[2]);
        };
        assert_eq!(len, payload.len() as i64);

        let memory = app.export_memory(index, "memory").expect("export memory");
        let mut buf = vec![0u8; payload.len()];
        memory
            .read(u32::try_from(addr).expect("address fits u32"), &mut buf)
            .expect("read payload");
        assert_eq!(buf, payload);
    }

    #[test]
    fn empty_pointer_argument_carries_zero_length() {
        let (mut app, index) = load_module("1");
        let values =
            resolve_entrypoint_arguments(&mut app, index, &[SystemGuestArg::Pointer(Vec::new())])
                .expect("resolve arguments");
        assert_eq!(values.len(), 2);
        assert_eq!(values[1], WasmValue::I64(0));
    }

    #[test]
    fn oversized_pointer_payload_fails_instead_of_truncating() {
        // The module caps its memory at one page, so a payload requiring a
        // second page must fail rather than be truncated or dropped.
        let (mut app, index) = load_module("1 1");
        let payload = vec![0xAAu8; WASM_PAGE_SIZE as usize + 1];
        let result =
            resolve_entrypoint_arguments(&mut app, index, &[SystemGuestArg::Pointer(payload)]);
        assert!(result.is_err(), "oversized payload must fail bootstrap");
    }
}
