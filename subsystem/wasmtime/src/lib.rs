//! Wasmtime subsystem integration for Selium runtime.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use selium_abi::EntrypointInvocation;
use selium_abi::{
    self, AbiParam, AbiScalarType, AbiScalarValue, AbiSignature, AbiValue, CallPlan, CallPlanError,
    hostcalls,
};
use selium_kernel::{
    KernelError,
    drivers::{Capability, module_store::ModuleStoreError, process::EntrypointInvocationExt},
    futures::FutureSharedState,
    guest_async::GuestAsync,
    guest_data::{GuestError, GuestInt, GuestUint, write_poll_result},
    mailbox,
    operation::LinkableOperation,
    registry::{InstanceRegistry, ProcessIdentity, Registry, ResourceId},
};
use thiserror::Error;
use tracing::{debug, warn};
use wasmtime::{Caller, Config, Engine, Func, Linker, Memory, Module, Store, Val, ValType};

mod driver;
pub use driver::WasmtimeDriver;

pub struct WasmRuntime {
    engine: Engine,
    available_caps: RwLock<HashMap<Capability, Vec<Arc<dyn LinkableOperation>>>>,
    guest_async: Arc<GuestAsync>,
}

const PREALLOC_PAGES: u64 = 256;

#[derive(Error, Debug)]
pub enum Error {
    #[error("The requested capability ({0}) is not part of this kernel")]
    CapabilityUnavailable(Capability),
    #[error("Selium kernel error: {0}")]
    Kernel(#[from] KernelError),
    #[error("Module store error: {0}")]
    ModuleStore(#[from] ModuleStoreError),
    #[error("Wasmtime error: {0}")]
    Wasmtime(#[from] wasmtime::Error),
    #[error("The lock guarding the Capability registry has been poisoned")]
    CapabilityRegistryPoisoned,
}

impl From<CallPlanError> for Error {
    fn from(value: CallPlanError) -> Self {
        Self::Kernel(KernelError::Driver(value.to_string()))
    }
}

impl WasmRuntime {
    pub fn new(
        available_caps: HashMap<Capability, Vec<Arc<dyn LinkableOperation>>>,
        guest_async: Arc<GuestAsync>,
    ) -> Result<Self, Error> {
        let mut config = Config::new();
        config.async_support(true);
        config.memory_may_move(false);

        Ok(Self {
            engine: Engine::new(&config)?,
            available_caps: RwLock::new(available_caps),
            guest_async,
        })
    }

    pub fn extend_capability(
        &self,
        capability: Capability,
        operations: impl IntoIterator<Item = Arc<dyn LinkableOperation>>,
    ) -> Result<(), Error> {
        let mut map = self
            .available_caps
            .write()
            .map_err(|_| Error::CapabilityRegistryPoisoned)?;
        let entry = map.entry(capability).or_default();
        entry.extend(operations);
        Ok(())
    }

    pub async fn run(
        &self,
        registry: &Arc<Registry>,
        process_id: ResourceId,
        module: Module,
        name: &str,
        capabilities: &[Capability],
        entrypoint: EntrypointInvocation,
    ) -> Result<(), Error> {
        let mut linker = Linker::new(&self.engine);
        let operations_to_link = {
            let map = self
                .available_caps
                .read()
                .map_err(|_| Error::CapabilityRegistryPoisoned)?;
            let mut ops = Vec::new();
            let requested: HashSet<Capability> = capabilities.iter().copied().collect();
            for capability in &requested {
                let operations = map
                    .get(capability)
                    .ok_or(Error::CapabilityUnavailable(*capability))?;

                if operations.is_empty() {
                    return Err(Error::CapabilityUnavailable(*capability));
                }

                ops.extend(operations.iter().cloned());
            }
            ops.extend(stub_operations_for_missing(&requested));
            ops
        };

        for op in operations_to_link {
            op.link(&mut linker)?;
        }

        self.guest_async.link(&mut linker)?;

        let instance_registry = registry.instance().map_err(KernelError::from)?;
        let mut store = Store::new(&self.engine, instance_registry);
        store
            .data_mut()
            .set_process_id(process_id)
            .map_err(KernelError::from)?;
        let identity = ProcessIdentity::new(process_id);
        store
            .data_mut()
            .insert_extension(identity)
            .map_err(KernelError::from)?;
        // Limit linear memory growth to keep the mailbox pointers stable across the
        // instance lifetime. We preallocate and then lock the limit to the current
        // size so guest-initiated growth fails fast instead of moving the base
        // address out from under host-side wakers.
        let instance = linker.instantiate_async(&mut store, &module).await?;

        // Initialise waker mailbox
        let memory = instance.get_memory(&mut store, "memory").ok_or_else(|| {
            Error::Kernel(KernelError::Driver("guest memory missing".to_string()))
        })?;
        preallocate_memory(&memory, &mut store);
        let mb = unsafe { mailbox::create_guest_mailbox(&memory, &mut store) };
        store
            .data_mut()
            .load_mailbox(mb)
            .map_err(KernelError::from)?;

        let signature = entrypoint.signature().clone();
        let call_values = {
            let registry = store.data_mut();
            entrypoint.materialise_values(registry)?
        };
        let plan = CallPlan::new(&signature, &call_values)?;
        materialise_plan(&memory, &mut store, &plan)?;

        let func = instance.get_func(&mut store, name).ok_or_else(|| {
            Error::Wasmtime(wasmtime::Error::msg(format!(
                "entrypoint `{name}` not found"
            )))
        })?;
        let func_ty = func.ty(&store);
        let param_types: Vec<ValType> = func_ty.params().collect();
        let result_types: Vec<ValType> = func_ty.results().collect();
        let expected_params = flatten_signature_types(signature.params());
        let expected_results = flatten_signature_types(signature.results());

        let params_match = param_types.len() == expected_params.len()
            && param_types
                .iter()
                .zip(expected_params.iter())
                .all(|(actual, expected)| valtype_eq(actual, expected));

        if !params_match {
            return Err(Error::Kernel(KernelError::Driver(format!(
                "entrypoint `{name}` expects params {:?}, got {:?}",
                expected_params, param_types
            ))));
        }

        let results_match = result_types.len() == expected_results.len()
            && result_types
                .iter()
                .zip(expected_results.iter())
                .all(|(actual, expected)| valtype_eq(actual, expected));

        if !results_match {
            return Err(Error::Kernel(KernelError::Driver(format!(
                "entrypoint expects results {:?}, got {:?}",
                expected_results, result_types
            ))));
        }

        let params = prepare_params(&param_types, plan.params())
            .map_err(|err| Error::Kernel(KernelError::Driver(err)))?;
        let result_template = prepare_results(&result_types)
            .map_err(|err| Error::Kernel(KernelError::Driver(err)))?;
        let signature_clone = signature.clone();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            // Wait for registration before invoking entrypoint. This prevents races between
            // guests registering resources and the process_id being set on the registry.
            if start_rx.await.is_err() {
                return Err(wasmtime::Error::msg("process start cancelled"));
            }
            invoke_entrypoint(
                func,
                store,
                memory,
                params,
                result_template,
                signature_clone,
            )
            .await
        });

        registry
            .initialise(process_id, handle)
            .map_err(|err| Error::Kernel(KernelError::from(err)))?;

        // Trigger entrypoint exec
        start_tx.send(()).map_err(|_| {
            Error::Kernel(KernelError::Driver("process start cancelled".to_string()))
        })?;

        Ok(())
    }
}

fn materialise_plan(
    memory: &Memory,
    store: &mut Store<InstanceRegistry>,
    plan: &CallPlan,
) -> Result<(), Error> {
    for write in plan.memory_writes() {
        if write.bytes.is_empty() {
            continue;
        }

        let start = usize::try_from(write.offset)
            .map_err(|err| Error::Kernel(KernelError::IntConvert(err)))?;
        let end = start
            .checked_add(write.bytes.len())
            .ok_or_else(|| Error::Kernel(KernelError::MemoryCapacity))?;
        let data = memory
            .data_mut(&mut *store)
            .get_mut(start..end)
            .ok_or(Error::Kernel(KernelError::MemoryCapacity))?;
        data.copy_from_slice(&write.bytes);
    }

    Ok(())
}

fn preallocate_memory(memory: &Memory, store: &mut Store<InstanceRegistry>) {
    let mut current = memory.size(&mut *store);
    if current < PREALLOC_PAGES {
        let delta = PREALLOC_PAGES - current;
        if let Err(err) = memory.grow(&mut *store, delta) {
            warn!("failed to preallocate guest memory to {PREALLOC_PAGES} pages: {err:?}");
        }
        current = memory.size(&mut *store);
    }
    let bytes = memory.data_size(&*store);
    debug!(pages = current, bytes, "prepared guest linear memory");
}

fn prepare_params(param_types: &[ValType], scalars: &[AbiScalarValue]) -> Result<Vec<Val>, String> {
    if param_types.len() != scalars.len() {
        return Err(format!(
            "entrypoint expects {} params, got {}",
            param_types.len(),
            scalars.len()
        ));
    }

    scalars
        .iter()
        .zip(param_types.iter())
        .map(|(scalar, ty)| scalar_to_val(scalar, ty))
        .collect()
}

fn prepare_results(result_types: &[ValType]) -> Result<Vec<Val>, String> {
    Ok(result_types
        .iter()
        .map(|ty| default_val(ty.clone()))
        .collect())
}

fn stub_operations_for_missing(requested: &HashSet<Capability>) -> Vec<Arc<dyn LinkableOperation>> {
    let hostcalls_by_capability = hostcalls::by_capability();

    selium_abi::Capability::ALL
        .iter()
        .copied()
        .filter(|capability| !requested.contains(capability))
        .flat_map(|capability| {
            hostcalls_by_capability
                .get(&capability)
                .into_iter()
                .flatten()
                .map(move |meta| {
                    StubOperation::new(meta.name, capability) as Arc<dyn LinkableOperation>
                })
        })
        .collect()
}

struct StubOperation {
    module: &'static str,
    capability: Capability,
}

impl StubOperation {
    fn new(module: &'static str, capability: Capability) -> Arc<Self> {
        Arc::new(Self { module, capability })
    }

    fn create_stub_future(
        mut caller: Caller<'_, InstanceRegistry>,
        module: &'static str,
        capability: Capability,
    ) -> Result<GuestUint, KernelError> {
        debug!(%module, ?capability, "invoking stub capability binding");

        let state = FutureSharedState::new();
        state.resolve(Err(GuestError::PermissionDenied));
        let handle = caller.data_mut().insert_future(state)?;

        GuestUint::try_from(handle).map_err(KernelError::IntConvert)
    }

    fn poll_stub_future(
        mut caller: Caller<'_, InstanceRegistry>,
        state_id: GuestUint,
        _task_id: GuestUint,
        result_ptr: GuestInt,
        result_capacity: GuestUint,
        module: &'static str,
        capability: Capability,
    ) -> Result<GuestUint, KernelError> {
        debug!(%module, ?capability, "polling stub capability binding");

        let state_id = usize::try_from(state_id).map_err(KernelError::IntConvert)?;
        let result = match caller.data_mut().remove_future(state_id) {
            Some(state) => state
                .take_result()
                .unwrap_or(Err(GuestError::PermissionDenied)),
            None => Err(GuestError::NotFound),
        };

        write_poll_result(&mut caller, result_ptr, result_capacity, result)
    }

    fn drop_stub_future(
        mut caller: Caller<'_, InstanceRegistry>,
        state_id: GuestUint,
        result_ptr: GuestInt,
        result_capacity: GuestUint,
        module: &'static str,
        capability: Capability,
    ) -> Result<GuestUint, KernelError> {
        debug!(%module, ?capability, "dropping stub capability binding");

        let state_id = usize::try_from(state_id).map_err(KernelError::IntConvert)?;
        let result = if let Some(state) = caller.data_mut().remove_future(state_id) {
            state.abandon();
            Ok(Vec::new())
        } else {
            Err(GuestError::NotFound)
        };

        write_poll_result(&mut caller, result_ptr, result_capacity, result)
    }
}

impl LinkableOperation for StubOperation {
    fn link(&self, linker: &mut Linker<InstanceRegistry>) -> Result<(), KernelError> {
        let module = self.module;
        let capability = self.capability;
        linker.func_wrap(
            module,
            "create",
            move |caller: Caller<'_, InstanceRegistry>,
                  _args_ptr: GuestInt,
                  _args_len: GuestUint| {
                StubOperation::create_stub_future(caller, module, capability).map_err(Into::into)
            },
        )?;

        let module = self.module;
        let capability = self.capability;
        linker.func_wrap(
            module,
            "poll",
            move |caller: Caller<'_, InstanceRegistry>,
                  state_id: GuestUint,
                  task_id: GuestUint,
                  result_ptr: GuestInt,
                  result_capacity: GuestUint| {
                StubOperation::poll_stub_future(
                    caller,
                    state_id,
                    task_id,
                    result_ptr,
                    result_capacity,
                    module,
                    capability,
                )
                .map_err(Into::into)
            },
        )?;

        let module = self.module;
        let capability = self.capability;
        linker.func_wrap(
            module,
            "drop",
            move |caller: Caller<'_, InstanceRegistry>,
                  state_id: GuestUint,
                  result_ptr: GuestInt,
                  result_capacity: GuestUint| {
                StubOperation::drop_stub_future(
                    caller,
                    state_id,
                    result_ptr,
                    result_capacity,
                    module,
                    capability,
                )
                .map_err(Into::into)
            },
        )?;

        Ok(())
    }
}

async fn invoke_entrypoint(
    func: Func,
    mut store: Store<InstanceRegistry>,
    memory: Memory,
    params: Vec<Val>,
    mut results: Vec<Val>,
    signature: AbiSignature,
) -> Result<Vec<AbiValue>, wasmtime::Error> {
    func.call_async(&mut store, &params, &mut results).await?;
    decode_results(&memory, &store, &results, &signature)
}

fn decode_results(
    memory: &Memory,
    store: &Store<InstanceRegistry>,
    raw: &[Val],
    signature: &AbiSignature,
) -> Result<Vec<AbiValue>, wasmtime::Error> {
    let mut iter = raw.iter();
    let mut values = Vec::new();

    for param in signature.results() {
        match param {
            AbiParam::Scalar(kind) => {
                let scalar = decode_scalar(&mut iter, *kind)?;
                values.push(AbiValue::Scalar(scalar));
            }
            AbiParam::Buffer => {
                let ptr_val = iter
                    .next()
                    .ok_or_else(|| wasmtime::Error::msg("missing buffer pointer"))?;
                let len_val = iter
                    .next()
                    .ok_or_else(|| wasmtime::Error::msg("missing buffer length"))?;
                let ptr = match ptr_val {
                    Val::I32(v) if *v >= 0 => *v as usize,
                    _ => return Err(wasmtime::Error::msg("buffer pointer must be i32")),
                };
                let len = match len_val {
                    Val::I32(v) if *v >= 0 => *v as usize,
                    _ => return Err(wasmtime::Error::msg("buffer length must be i32")),
                };

                if len == 0 {
                    values.push(AbiValue::Buffer(Vec::new()));
                    continue;
                }

                let data = memory
                    .data(store)
                    .get(ptr..ptr + len)
                    .ok_or_else(|| wasmtime::Error::msg("buffer result out of bounds"))?;
                values.push(AbiValue::Buffer(data.to_vec()));
            }
        }
    }

    if iter.next().is_some() {
        return Err(wasmtime::Error::msg("extra values returned by entrypoint"));
    }

    Ok(values)
}

fn scalar_to_val(value: &AbiScalarValue, ty: &ValType) -> Result<Val, String> {
    match (value, ty) {
        (AbiScalarValue::I32(v), ValType::I32) => Ok(Val::I32(*v)),
        (AbiScalarValue::U32(v), ValType::I32) => {
            let bits = i32::from_ne_bytes(v.to_ne_bytes());
            Ok(Val::I32(bits))
        }
        (AbiScalarValue::I16(v), ValType::I32) => Ok(Val::I32(i32::from(*v))),
        (AbiScalarValue::U16(v), ValType::I32) => Ok(Val::I32(i32::from(*v))),
        (AbiScalarValue::I8(v), ValType::I32) => Ok(Val::I32(i32::from(*v))),
        (AbiScalarValue::U8(v), ValType::I32) => Ok(Val::I32(i32::from(*v))),
        (AbiScalarValue::I64(v), ValType::I64) => Ok(Val::I64(*v)),
        (AbiScalarValue::F32(v), ValType::F32) => Ok(Val::F32(v.to_bits())),
        (AbiScalarValue::F64(v), ValType::F64) => Ok(Val::F64(v.to_bits())),
        _ => Err(format!(
            "type mismatch: value {:?} cannot be passed as {:?}",
            value, ty
        )),
    }
}

fn decode_scalar(
    iter: &mut std::slice::Iter<Val>,
    expected: AbiScalarType,
) -> Result<AbiScalarValue, wasmtime::Error> {
    match expected {
        AbiScalarType::I8 => {
            let raw = take_i32(iter, "missing i8 result")?;
            i8::try_from(raw)
                .map(AbiScalarValue::I8)
                .map_err(|_| wasmtime::Error::msg("i8 result out of range"))
        }
        AbiScalarType::U8 => {
            let raw = take_u32(iter, "missing u8 result")?;
            u8::try_from(raw)
                .map(AbiScalarValue::U8)
                .map_err(|_| wasmtime::Error::msg("u8 result out of range"))
        }
        AbiScalarType::I16 => {
            let raw = take_i32(iter, "missing i16 result")?;
            i16::try_from(raw)
                .map(AbiScalarValue::I16)
                .map_err(|_| wasmtime::Error::msg("i16 result out of range"))
        }
        AbiScalarType::U16 => {
            let raw = take_u32(iter, "missing u16 result")?;
            u16::try_from(raw)
                .map(AbiScalarValue::U16)
                .map_err(|_| wasmtime::Error::msg("u16 result out of range"))
        }
        AbiScalarType::I32 => {
            let raw = take_i32(iter, "missing i32 result")?;
            Ok(AbiScalarValue::I32(raw))
        }
        AbiScalarType::U32 => {
            let raw = take_u32(iter, "missing u32 result")?;
            Ok(AbiScalarValue::U32(raw))
        }
        AbiScalarType::I64 => {
            let lo = take_u32(iter, "missing low i64 result")?;
            let hi = take_u32(iter, "missing high i64 result")?;
            let combined = (u64::from(hi) << 32) | u64::from(lo);
            Ok(AbiScalarValue::I64(i64::from_le_bytes(
                combined.to_le_bytes(),
            )))
        }
        AbiScalarType::U64 => {
            let lo = take_u32(iter, "missing low u64 result")?;
            let hi = take_u32(iter, "missing high u64 result")?;
            let combined = (u64::from(hi) << 32) | u64::from(lo);
            Ok(AbiScalarValue::U64(combined))
        }
        AbiScalarType::F32 => {
            let val = iter
                .next()
                .ok_or_else(|| wasmtime::Error::msg("missing f32 result"))?;
            match val {
                Val::F32(bits) => Ok(AbiScalarValue::F32(f32::from_bits(*bits))),
                _ => Err(wasmtime::Error::msg("f32 result must be f32")),
            }
        }
        AbiScalarType::F64 => {
            let val = iter
                .next()
                .ok_or_else(|| wasmtime::Error::msg("missing f64 result"))?;
            match val {
                Val::F64(bits) => Ok(AbiScalarValue::F64(f64::from_bits(*bits))),
                _ => Err(wasmtime::Error::msg("f64 result must be f64")),
            }
        }
    }
}

fn default_val(ty: ValType) -> Val {
    match ty {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0u32),
        ValType::F64 => Val::F64(0u64),
        other => panic!("unsupported Wasm value type in entrypoint: {other:?}"),
    }
}

fn flatten_signature_types(spec: &[AbiParam]) -> Vec<ValType> {
    let mut types = Vec::new();
    for param in spec {
        match param {
            AbiParam::Scalar(kind) => push_scalar_types(*kind, &mut types),
            AbiParam::Buffer => {
                types.push(ValType::I32);
                types.push(ValType::I32);
            }
        }
    }
    types
}

fn push_scalar_types(kind: AbiScalarType, types: &mut Vec<ValType>) {
    match kind {
        AbiScalarType::F32 => types.push(ValType::F32),
        AbiScalarType::F64 => types.push(ValType::F64),
        AbiScalarType::I64 | AbiScalarType::U64 => {
            types.push(ValType::I32);
            types.push(ValType::I32);
        }
        AbiScalarType::I8
        | AbiScalarType::U8
        | AbiScalarType::I16
        | AbiScalarType::U16
        | AbiScalarType::I32
        | AbiScalarType::U32 => types.push(ValType::I32),
    }
}

fn valtype_eq(a: &ValType, b: &ValType) -> bool {
    matches!(
        (a, b),
        (ValType::I32, ValType::I32)
            | (ValType::I64, ValType::I64)
            | (ValType::F32, ValType::F32)
            | (ValType::F64, ValType::F64)
    )
}

fn take_i32(iter: &mut std::slice::Iter<Val>, msg: &str) -> Result<i32, wasmtime::Error> {
    let Some(val) = iter.next() else {
        return Err(wasmtime::Error::msg(msg.to_owned()));
    };

    match val {
        Val::I32(v) => Ok(*v),
        _ => Err(wasmtime::Error::msg(msg.to_owned())),
    }
}

fn take_u32(iter: &mut std::slice::Iter<Val>, msg: &str) -> Result<u32, wasmtime::Error> {
    let raw = take_i32(iter, msg)?;
    Ok(u32::from_ne_bytes(raw.to_ne_bytes()))
}
