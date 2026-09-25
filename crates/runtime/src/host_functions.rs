use std::sync::Arc;

use selium_abi::{
    ActivityEvent, CompletionState, HostcallEnvelope, ProcessId, pack_hostcall_status,
};
use wasmtiny::{
    FunctionType, NumType, ValType, WasmApplication, WasmError, WasmValue,
    runtime::{HostCaller, HostFunc},
};

use crate::{
    Result,
    mailbox::GuestMailbox,
    runtime::Runtime,
    wasm::{
        guest_memory, read_guest_memory, register_optional_host_function, wasm_i32_arg,
        wasm_i64_arg, write_guest_memory,
    },
};

struct MarkReadyHostFunc {
    runtime: Runtime,
    process_id: selium_abi::ProcessId,
}

struct ProcessIdHostFunc {
    process_id: selium_abi::ProcessId,
}

struct HostcallCreateHostFunc {
    runtime: Runtime,
    process_id: ProcessId,
}

struct HostcallPollHostFunc {
    runtime: Runtime,
    process_id: ProcessId,
}

struct HostcallDropHostFunc {
    runtime: Runtime,
    process_id: ProcessId,
}

struct MailboxRegisterHostFunc {
    runtime: Runtime,
    process_id: ProcessId,
}

impl HostFunc for MarkReadyHostFunc {
    fn call(
        &self,
        _caller: &mut HostCaller<'_>,
        _args: &[WasmValue],
    ) -> wasmtiny::runtime::Result<Vec<WasmValue>> {
        self.runtime
            .kernel
            .processes()
            .record_activity(ActivityEvent {
                kind: selium_abi::ActivityKind::GuestReady,
                process_id: Some(self.process_id),
                message: "guest ready".to_string(),
            });
        Ok(Vec::new())
    }

    fn function_type(&self) -> Option<&FunctionType> {
        None
    }
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
        let ptr = wasm_i32_arg(args, 0)?.cast_unsigned();
        let len = wasm_i32_arg(args, 1)?.cast_unsigned() as usize;
        let request_bytes = read_guest_memory(caller, ptr, len)?;
        let envelope = match selium_abi::decode_rkyv::<HostcallEnvelope>(&request_bytes) {
            Ok(envelope) => envelope,
            Err(_) => {
                let status = pack_hostcall_status(selium_abi::HOSTCALL_STATUS_FAILED, 0);
                return Ok(vec![WasmValue::I64(status as i64)]);
            }
        };
        let (status, operation_id) = self.runtime.begin_hostcall_with_task(
            self.process_id,
            envelope.request,
            envelope.task_id,
            guest_memory(caller).ok(),
        );
        // Kick outbound network proxies after every guest→host transition.
        self.runtime.kick_network_waiters();
        Ok(vec![WasmValue::I64(
            pack_hostcall_status(status, operation_id as u32) as i64,
        )])
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
        let operation_id = wasm_i64_arg(args, 0)?.cast_unsigned();
        let out_ptr = wasm_i32_arg(args, 1)?.cast_unsigned();
        let out_capacity = wasm_i32_arg(args, 2)?.cast_unsigned() as usize;
        let state = self.runtime.poll_hostcall(self.process_id, operation_id);
        // Kick outbound network proxies on every guest→host transition.
        self.runtime.kick_network_waiters();
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
        let operation_id = wasm_i64_arg(args, 0)?.cast_unsigned();
        let dropped = self.runtime.drop_hostcall(self.process_id, operation_id);
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
        self.runtime
            .register_mailbox(self.process_id, Arc::new(GuestMailbox::new(memory, base)));
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
            "mark_ready".to_string(),
            Box::new(MarkReadyHostFunc {
                runtime: runtime.clone(),
                process_id,
            }),
            FunctionType::empty(),
        ),
        (
            "selium".to_string(),
            "hostcall_create".to_string(),
            Box::new(HostcallCreateHostFunc {
                runtime: runtime.clone(),
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
                runtime: runtime.clone(),
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
                runtime: runtime.clone(),
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
                runtime: runtime.clone(),
                process_id,
            }),
            FunctionType::new(
                vec![ValType::Num(NumType::I32), ValType::Num(NumType::I32)],
                vec![],
            ),
        ),
    ]
}
