use std::sync::Arc;

use selium_abi::{CompletionState, HostcallEnvelope, ProcessId, pack_hostcall_status};
use wasmtiny::{
    FunctionType, NumType, ValType, WasmApplication, WasmError, WasmValue,
    runtime::{HostCaller, HostFunc},
};

use crate::{
    Result,
    mailbox::GuestMailbox,
    runtime::{Runtime, WeakRuntime},
    wasm::{
        guest_memory, read_guest_memory, register_optional_host_function, wasm_i32_arg,
        wasm_i64_arg, write_guest_memory,
    },
};

struct ProcessIdHostFunc {
    process_id: selium_abi::ProcessId,
}

struct HostcallCreateHostFunc {
    runtime: WeakRuntime,
    process_id: ProcessId,
}

struct HostcallPollHostFunc {
    runtime: WeakRuntime,
    process_id: ProcessId,
}

struct HostcallDropHostFunc {
    runtime: WeakRuntime,
    process_id: ProcessId,
}

struct MailboxRegisterHostFunc {
    runtime: WeakRuntime,
    process_id: ProcessId,
}

impl HostFunc for ProcessIdHostFunc {
    fn call(
        &self,
        _caller: &mut HostCaller<'_>,
        _args: &[WasmValue],
    ) -> wasmtiny::runtime::Result<Vec<WasmValue>> {
        Ok(vec![WasmValue::I64(self.process_id as i64)])
    }

    fn function_type(&self) -> Option<&FunctionType> {
        None
    }
}

impl HostFunc for HostcallCreateHostFunc {
    fn call(
        &self,
        caller: &mut HostCaller<'_>,
        args: &[WasmValue],
    ) -> wasmtiny::runtime::Result<Vec<WasmValue>> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| WasmError::Runtime("runtime has shut down".to_string()))?;
        let ptr = wasm_i32_arg(args, 0)?.cast_unsigned();
        let len = wasm_i32_arg(args, 1)?.cast_unsigned() as usize;
        // Creation returns the full u64 operation id; completion status and
        // structured errors are obtained through hostcall_poll. Zero means the
        // process's bounded operation budget is exhausted. No status packing
        // truncates large operation ids at this ABI boundary.
        let reject = |error: selium_abi::AbiError| {
            let (_status, operation_id) = runtime.reject_hostcall(self.process_id, error);
            Ok(vec![WasmValue::I64(operation_id as i64)])
        };
        // Reject oversized declarations before allocating or decoding.
        if len > selium_abi::HOSTCALL_ENVELOPE_MAX_BYTES {
            return reject(selium_abi::AbiError::new(
                selium_abi::AbiErrorCode::MalformedPayload,
                format!(
                    "hostcall envelope length {len} exceeds the maximum of {} bytes",
                    selium_abi::HOSTCALL_ENVELOPE_MAX_BYTES
                ),
            ));
        }
        let request_bytes = match read_guest_memory(caller, ptr, len) {
            Ok(bytes) => bytes,
            Err(error) => {
                return reject(selium_abi::AbiError::new(
                    selium_abi::AbiErrorCode::MalformedPayload,
                    format!("hostcall envelope range is outside guest memory: {error}"),
                ));
            }
        };
        // Validate the fixed ABI preamble before any rkyv enum dispatch. An
        // incompatible guest must fail loudly rather than have its request
        // decoded as another variant layout.
        let payload = match selium_abi::hostcall_request_payload(&request_bytes) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, process_id = self.process_id, "rejecting hostcall request preamble");
                return reject(error.to_abi_error());
            }
        };
        let envelope = match selium_abi::decode_rkyv::<HostcallEnvelope>(payload) {
            Ok(envelope) => envelope,
            Err(error) => {
                return reject(selium_abi::AbiError::new(
                    selium_abi::AbiErrorCode::MalformedPayload,
                    format!("hostcall request could not be decoded: {error}"),
                ));
            }
        };
        let (_status, operation_id) = runtime.begin_hostcall_with_task(
            self.process_id,
            envelope.request,
            envelope.task_id,
            guest_memory(caller).ok(),
        );
        // Kick outbound network proxies after every guest→host transition.
        runtime.kick_network_waiters();
        Ok(vec![WasmValue::I64(operation_id as i64)])
    }

    fn function_type(&self) -> Option<&FunctionType> {
        None
    }
}

impl HostFunc for HostcallPollHostFunc {
    fn call(
        &self,
        caller: &mut HostCaller<'_>,
        args: &[WasmValue],
    ) -> wasmtiny::runtime::Result<Vec<WasmValue>> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| WasmError::Runtime("runtime has shut down".to_string()))?;
        let operation_id = wasm_i64_arg(args, 0)?.cast_unsigned();
        let out_ptr = wasm_i32_arg(args, 1)?.cast_unsigned();
        let out_capacity = wasm_i32_arg(args, 2)?.cast_unsigned() as usize;
        let state = runtime.poll_hostcall(self.process_id, operation_id);
        // Kick outbound network proxies on every guest→host transition.
        runtime.kick_network_waiters();
        let status = match state {
            CompletionState::Ready(_) => selium_abi::HOSTCALL_STATUS_READY,
            CompletionState::Pending { .. } => selium_abi::HOSTCALL_STATUS_PENDING,
            CompletionState::Failed(_) => selium_abi::HOSTCALL_STATUS_FAILED,
        };
        let encoded = selium_abi::encode_rkyv(&state)
            .map_err(|error| WasmError::Runtime(error.to_string()))?;
        if encoded.len() > out_capacity {
            return Ok(vec![WasmValue::I64(pack_hostcall_status(
                selium_abi::HOSTCALL_STATUS_OUTPUT_TOO_SMALL,
                encoded.len() as u32,
            ) as i64)]);
        }
        write_guest_memory(caller, out_ptr, &encoded)?;
        Ok(vec![WasmValue::I64(
            pack_hostcall_status(status, encoded.len() as u32) as i64,
        )])
    }

    fn function_type(&self) -> Option<&FunctionType> {
        None
    }
}

impl HostFunc for HostcallDropHostFunc {
    fn call(
        &self,
        _caller: &mut HostCaller<'_>,
        args: &[WasmValue],
    ) -> wasmtiny::runtime::Result<Vec<WasmValue>> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| WasmError::Runtime("runtime has shut down".to_string()))?;
        let operation_id = wasm_i64_arg(args, 0)?.cast_unsigned();
        let dropped = runtime.drop_hostcall(self.process_id, operation_id);
        Ok(vec![WasmValue::I32(u32::from(dropped) as i32)])
    }

    fn function_type(&self) -> Option<&FunctionType> {
        None
    }
}

impl HostFunc for MailboxRegisterHostFunc {
    fn call(
        &self,
        caller: &mut HostCaller<'_>,
        args: &[WasmValue],
    ) -> wasmtiny::runtime::Result<Vec<WasmValue>> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| WasmError::Runtime("runtime has shut down".to_string()))?;
        let base = wasm_i32_arg(args, 0)?.cast_unsigned();
        let len = wasm_i32_arg(args, 1)?.cast_unsigned() as usize;
        if len < selium_abi::mailbox::BYTE_LEN {
            return Err(WasmError::Runtime("guest mailbox is too small".to_string()));
        }
        let memory = guest_memory(caller)?;
        memory
            .lock()
            .map_err(|_lock_err| WasmError::Runtime("guest memory lock poisoned".to_string()))?
            .write_u32(
                base + selium_abi::mailbox::CAPACITY_OFFSET as u32,
                selium_abi::mailbox::CAPACITY as u32,
            )?;
        runtime.register_mailbox(self.process_id, Arc::new(GuestMailbox::new(memory, base)));
        Ok(Vec::new())
    }

    fn function_type(&self) -> Option<&FunctionType> {
        None
    }
}

impl Runtime {
    pub(crate) fn register_runtime_host_functions(
        &self,
        app: &mut WasmApplication,
        module_index: u32,
        process_id: selium_abi::ProcessId,
    ) -> Result<()> {
        for (module, name, func, func_type) in selium_host_imports(self, process_id) {
            register_optional_host_function(app, module_index, &module, &name, func, func_type)?;
        }
        Ok(())
    }

    /// The `selium` host-import surface as AOT externs, for instantiating a
    /// multithreaded guest's AOT instance. Only the imports the module
    /// actually declares are bound by the engine; extra entries are unused.
    pub(crate) fn runtime_aot_imports(
        &self,
        process_id: ProcessId,
    ) -> Vec<(String, String, wasmtiny::aot::AotExtern)> {
        selium_host_imports(self, process_id)
            .into_iter()
            .map(|(module, name, func, _func_type)| {
                (
                    module,
                    name,
                    wasmtiny::aot::AotExtern::HostFunc(Arc::from(func)),
                )
            })
            .collect()
    }

    pub(crate) fn register_mailbox(&self, process_id: ProcessId, mailbox: Arc<GuestMailbox>) {
        self.mailboxes.lock().insert(process_id, mailbox);
    }
}

/// The `selium` host-import surface: (module, name, function, function type).
/// Shared by the interpreter (`register_runtime_host_functions`) and the AOT
/// (`runtime_aot_imports`) execution paths so both see the same bridge.
fn selium_host_imports(
    runtime: &Runtime,
    process_id: ProcessId,
) -> Vec<(String, String, Box<dyn HostFunc>, FunctionType)> {
    vec![
        (
            "selium".to_string(),
            "process_id".to_string(),
            Box::new(ProcessIdHostFunc { process_id }),
            FunctionType::new(vec![], vec![ValType::Num(NumType::I64)]),
        ),
        (
            "selium".to_string(),
            "hostcall_create".to_string(),
            Box::new(HostcallCreateHostFunc {
                runtime: runtime.background_handle(),
                process_id,
            }),
            FunctionType::new(
                vec![ValType::Num(NumType::I32), ValType::Num(NumType::I32)],
                vec![ValType::Num(NumType::I64)],
            ),
        ),
        (
            "selium".to_string(),
            "hostcall_poll".to_string(),
            Box::new(HostcallPollHostFunc {
                runtime: runtime.background_handle(),
                process_id,
            }),
            FunctionType::new(
                vec![
                    ValType::Num(NumType::I64),
                    ValType::Num(NumType::I32),
                    ValType::Num(NumType::I32),
                ],
                vec![ValType::Num(NumType::I64)],
            ),
        ),
        (
            "selium".to_string(),
            "hostcall_drop".to_string(),
            Box::new(HostcallDropHostFunc {
                runtime: runtime.background_handle(),
                process_id,
            }),
            FunctionType::new(
                vec![ValType::Num(NumType::I64)],
                vec![ValType::Num(NumType::I32)],
            ),
        ),
        (
            "selium".to_string(),
            "mailbox_register".to_string(),
            Box::new(MailboxRegisterHostFunc {
                runtime: runtime.background_handle(),
                process_id,
            }),
            FunctionType::new(
                vec![ValType::Num(NumType::I32), ValType::Num(NumType::I32)],
                vec![],
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_abi_version(mut module: Vec<u8>, version: u32) -> Vec<u8> {
        let name = selium_abi::SELIUM_ABI_VERSION_SECTION.as_bytes();
        module.push(0);
        module.push((1 + name.len() + std::mem::size_of::<u32>()) as u8);
        module.push(name.len() as u8);
        module.extend_from_slice(name);
        module.extend_from_slice(&version.to_le_bytes());
        module
    }

    #[test]
    fn create_transport_preserves_the_full_u64_operation_id() {
        let envelope = selium_abi::HostcallEnvelope {
            request: selium_abi::HostcallRequest::SelfInfo,
            task_id: None,
        };
        let encoded = selium_abi::encode_hostcall_request(&envelope).expect("encode request");
        let data: String = encoded.iter().map(|byte| format!("\\{byte:02x}")).collect();
        let module = wat::parse_str(format!(
            r#"(module
                (import "selium" "hostcall_create"
                  (func $create (param i32 i32) (result i64)))
                (memory 1)
                (data (i32.const 0) "{data}")
                (func (export "boot") (result i64)
                  (call $create (i32.const 0) (i32.const {len}))))"#,
            len = encoded.len(),
        ))
        .expect("compile boundary guest");
        let module = with_abi_version(module, selium_abi::SELIUM_ABI_VERSION);
        let runtime = Runtime::default();
        *runtime.next_operation_id.lock() = u64::MAX;
        let guest = runtime
            .spawn_system_guest(crate::SystemGuestDescriptor {
                name: "operation-boundary".to_string(),
                module_id: "operation-boundary-module".to_string(),
                module_bytes: module,
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            })
            .expect("spawn boundary guest");

        assert_eq!(
            runtime.entrypoint_results(guest.process_id),
            Some(vec![WasmValue::I64(-1)]),
            "u64::MAX crosses the i64 WASM return slot bit-for-bit"
        );
        assert!(matches!(
            runtime.poll_hostcall(guest.process_id, u64::MAX),
            CompletionState::Ready(selium_abi::HostcallOutput::SelfInfo { .. })
        ));
        assert!(runtime.drop_hostcall(guest.process_id, u64::MAX));
    }
}
