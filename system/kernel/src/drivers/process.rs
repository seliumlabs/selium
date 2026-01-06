use std::{
    convert::TryFrom,
    future::{Future, ready},
    marker::PhantomData,
    sync::Arc,
};

use selium_abi::{
    AbiParam, AbiScalarType, AbiScalarValue, AbiValue, EntrypointArg, EntrypointInvocation,
    GuestResourceId, ProcessLogLookup, ProcessLogRegistration, ProcessStart,
};
use tracing::debug;
use wasmtime::Caller;

use crate::{
    KernelError,
    drivers::Capability,
    guest_data::{GuestError, GuestResult},
    operation::{Contract, Operation},
    registry::{
        InstanceRegistry, ProcessIdentity, Registry, ResourceHandle, ResourceId, ResourceType,
    },
};

type ProcessLifecycleOps<C> = (
    Arc<Operation<ProcessStartDriver<C>>>,
    Arc<Operation<ProcessStopDriver<C>>>,
);

type ProcessLogOps<C> = (
    Arc<Operation<ProcessRegisterLogDriver<C>>>,
    Arc<Operation<ProcessLogLookupDriver<C>>>,
);

/// Capability responsible for starting/stopping guest instances.
pub trait ProcessLifecycleCapability {
    type Process: Send + ProcessHandleExt;
    type Error: Into<GuestError>;

    /// Start a new process, identified by `module_id` and `name`
    fn start(
        &self,
        registry: &Arc<Registry>,
        process_id: ResourceId,
        module_id: &str,
        name: &str,
        capabilities: Vec<Capability>,
        entrypoint: EntrypointInvocation,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Stop a running process
    fn stop(
        &self,
        instance: &mut Self::Process,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Process entry stored in the registry.
#[derive(Debug)]
pub struct ProcessInstance<P> {
    identity: ProcessIdentity,
    inner: P,
}

/// Shared metadata surface for processes managed by the kernel.
pub trait ProcessHandleExt {
    /// Stable identity for this process.
    fn identity(&self) -> ProcessIdentity;
}

impl<P> ProcessInstance<P> {
    pub fn new(identity: ProcessIdentity, inner: P) -> Self {
        Self { identity, inner }
    }

    /// Return the stable identity assigned to this process.
    pub fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    /// Access the underlying process handle.
    pub fn inner_mut(&mut self) -> &mut P {
        &mut self.inner
    }

    /// Borrow the underlying process handle.
    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// Consume the wrapper and return the inner process handle.
    pub fn into_inner(self) -> P {
        self.inner
    }
}

impl<P> ProcessHandleExt for ProcessInstance<P> {
    fn identity(&self) -> ProcessIdentity {
        self.identity
    }
}

pub struct ProcessStartDriver<Impl>(Impl);
pub struct ProcessStopDriver<Impl>(Impl);
/// Hostcall driver that records the logging channel exported by a process.
pub struct ProcessRegisterLogDriver<Impl>(PhantomData<Impl>);
/// Hostcall driver that fetches the logging channel for a running process.
pub struct ProcessLogLookupDriver<Impl>(PhantomData<Impl>);

impl<T> ProcessLifecycleCapability for Arc<T>
where
    T: ProcessLifecycleCapability,
{
    type Process = T::Process;
    type Error = T::Error;

    fn start(
        &self,
        registry: &Arc<Registry>,
        process_id: ResourceId,
        module_id: &str,
        name: &str,
        capabilities: Vec<Capability>,
        entrypoint: EntrypointInvocation,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.as_ref().start(
            registry,
            process_id,
            module_id,
            name,
            capabilities,
            entrypoint,
        )
    }

    fn stop(
        &self,
        instance: &mut Self::Process,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.as_ref().stop(instance)
    }
}

impl<Impl> Contract for ProcessStartDriver<Impl>
where
    Impl: ProcessLifecycleCapability + Clone + Send + 'static,
{
    type Input = ProcessStart;
    type Output = GuestResourceId;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let registry = caller.data().registry_arc();
        let ProcessStart {
            module_id,
            name,
            capabilities,
            entrypoint,
        } = input;

        let preparation =
            (|| -> GuestResult<(String, String, Vec<Capability>, EntrypointInvocation)> {
                entrypoint
                    .validate()
                    .map_err(|err| GuestError::from(KernelError::Driver(err.to_string())))?;
                let entrypoint = resolve_entrypoint_resources(entrypoint, caller.data())?;
                Ok((module_id, name, capabilities, entrypoint))
            })();

        async move {
            let (module_id, name, capabilities, entrypoint) = preparation?;
            debug!(%module_id, %name, capabilities = ?capabilities, "process_start requested");
            let process_id = registry
                .reserve(None, ResourceType::Process)
                .map_err(GuestError::from)?;

            match inner
                .start(
                    &registry,
                    process_id,
                    &module_id,
                    &name,
                    capabilities,
                    entrypoint,
                )
                .await
            {
                Ok(()) => {}
                Err(err) => {
                    registry.discard(process_id);
                    return Err(err.into());
                }
            }

            let handle = GuestResourceId::try_from(process_id)
                .map_err(|_| GuestError::from(KernelError::InvalidHandle))?;
            Ok(handle)
        }
    }
}

impl<Impl> Contract for ProcessStopDriver<Impl>
where
    Impl: ProcessLifecycleCapability + Clone + Send + 'static,
{
    type Input = GuestResourceId;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let registry = caller.data().registry_arc();

        async move {
            let handle = ResourceId::try_from(input).map_err(|_| GuestError::InvalidArgument)?;
            if let Some(meta) = registry.metadata(handle)
                && meta.kind != ResourceType::Process
            {
                return Err(GuestError::InvalidArgument);
            }
            let mut process = registry
                .remove(ResourceHandle::<Impl::Process>::new(handle))
                .ok_or(GuestError::NotFound)?;
            inner.stop(&mut process).await.map_err(Into::into)?;
            Ok(())
        }
    }
}

impl<Impl> Contract for ProcessRegisterLogDriver<Impl>
where
    Impl: ProcessLifecycleCapability + Clone + Send + 'static,
{
    type Input = ProcessLogRegistration;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let identity = caller
            .data()
            .extension::<ProcessIdentity>()
            .map(|identity| *identity);
        let registry = caller.data().registry_arc();

        ready((|| -> GuestResult<Self::Output> {
            let identity = identity.ok_or(GuestError::PermissionDenied)?;
            let process_id = identity.raw();
            let channel_id = registry
                .resolve_shared(input.channel)
                .ok_or(GuestError::NotFound)?;

            match registry.metadata(process_id) {
                Some(meta) if meta.kind == ResourceType::Process => {}
                Some(_) => return Err(GuestError::InvalidArgument),
                None => return Err(GuestError::NotFound),
            }

            registry
                .set_log_channel(process_id, channel_id)
                .map_err(GuestError::from)?;
            Ok(())
        })())
    }
}

impl<Impl> Contract for ProcessLogLookupDriver<Impl>
where
    Impl: ProcessLifecycleCapability + Clone + Send + 'static,
{
    type Input = ProcessLogLookup;
    type Output = GuestResourceId;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let registry = caller.data().registry_arc();

        ready(
            ResourceId::try_from(input.process_id)
                .map_err(|_| GuestError::InvalidArgument)
                .and_then(|id| match registry.metadata(id) {
                    Some(meta) if meta.kind == ResourceType::Process => {
                        registry.log_channel_handle(id).ok_or(GuestError::NotFound)
                    }
                    Some(_) => Err(GuestError::InvalidArgument),
                    None => Err(GuestError::NotFound),
                }),
        )
    }
}

/// Helpers for working with entrypoint invocations inside the kernel.
pub trait EntrypointInvocationExt {
    fn materialise_values(
        &self,
        registry: &mut InstanceRegistry,
    ) -> Result<Vec<AbiValue>, KernelError>;
}

impl EntrypointInvocationExt for EntrypointInvocation {
    fn materialise_values(
        &self,
        registry: &mut InstanceRegistry,
    ) -> Result<Vec<AbiValue>, KernelError> {
        let mut values = Vec::with_capacity(self.args.len());

        for (index, (param, arg)) in self
            .signature
            .params()
            .iter()
            .zip(self.args.iter())
            .enumerate()
        {
            match (param, arg) {
                (AbiParam::Scalar(_), EntrypointArg::Scalar(value)) => {
                    values.push(AbiValue::Scalar(*value));
                }
                (AbiParam::Scalar(AbiScalarType::I32), EntrypointArg::Resource(resource_id)) => {
                    let resource =
                        ResourceId::try_from(*resource_id).map_err(KernelError::IntConvert)?;
                    let slot = registry.insert_id(resource).map_err(KernelError::from)?;
                    let slot = i32::try_from(slot).map_err(KernelError::IntConvert)?;
                    values.push(AbiValue::Scalar(AbiScalarValue::I32(slot)));
                }
                (AbiParam::Scalar(AbiScalarType::U64), EntrypointArg::Resource(resource_id)) => {
                    if registry.registry().resolve_shared(*resource_id).is_none() {
                        return Err(KernelError::Driver(format!(
                            "argument {index} references unknown shared resource"
                        )));
                    }
                    values.push(AbiValue::Scalar(AbiScalarValue::U64(*resource_id)));
                }
                (AbiParam::Buffer, EntrypointArg::Buffer(bytes)) => {
                    values.push(AbiValue::Buffer(bytes.clone()));
                }
                _ => {
                    return Err(KernelError::Driver(format!(
                        "argument {index} incompatible with signature"
                    )));
                }
            }
        }

        Ok(values)
    }
}

fn resolve_entrypoint_resources(
    entrypoint: EntrypointInvocation,
    registry: &InstanceRegistry,
) -> GuestResult<EntrypointInvocation> {
    let signature = entrypoint.signature;
    let mut resolved = Vec::with_capacity(entrypoint.args.len());

    for (index, (param, arg)) in signature
        .params()
        .iter()
        .zip(entrypoint.args.into_iter())
        .enumerate()
    {
        let arg = match (param, arg) {
            (AbiParam::Scalar(AbiScalarType::I32), EntrypointArg::Resource(handle)) => {
                let slot = usize::try_from(handle)
                    .map_err(|err| GuestError::from(KernelError::IntConvert(err)))?;
                let rid = registry.entry(slot).ok_or(GuestError::NotFound)?;
                let rid = GuestResourceId::try_from(rid).map_err(|_| {
                    GuestError::from(KernelError::Driver("invalid handle".to_string()))
                })?;
                EntrypointArg::Resource(rid)
            }
            (AbiParam::Scalar(AbiScalarType::U64), EntrypointArg::Resource(handle)) => {
                if registry.registry().resolve_shared(handle).is_none() {
                    return Err(GuestError::from(KernelError::Driver(format!(
                        "argument {index} references unknown shared resource"
                    ))));
                }
                EntrypointArg::Resource(handle)
            }
            (_, arg) => arg,
        };
        resolved.push(arg);
    }

    Ok(EntrypointInvocation {
        signature,
        args: resolved,
    })
}

pub fn lifecycle_ops<C>(cap: C) -> ProcessLifecycleOps<C>
where
    C: ProcessLifecycleCapability + Clone + Send + 'static,
{
    (
        Operation::from_hostcall(
            ProcessStartDriver(cap.clone()),
            selium_abi::hostcall_contract!(PROCESS_START),
        ),
        Operation::from_hostcall(
            ProcessStopDriver(cap),
            selium_abi::hostcall_contract!(PROCESS_STOP),
        ),
    )
}

pub fn log_ops<C>() -> ProcessLogOps<C>
where
    C: ProcessLifecycleCapability + Clone + Send + 'static,
{
    (
        Operation::from_hostcall(
            ProcessRegisterLogDriver(PhantomData),
            selium_abi::hostcall_contract!(PROCESS_REGISTER_LOG),
        ),
        Operation::from_hostcall(
            ProcessLogLookupDriver(PhantomData),
            selium_abi::hostcall_contract!(PROCESS_LOG_CHANNEL),
        ),
    )
}
