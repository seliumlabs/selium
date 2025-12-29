use std::{convert::TryFrom, sync::Arc};

use selium_abi::hostcalls::Hostcall;
use selium_abi::{RkyvEncode, encode_rkyv};
use tracing::{debug, trace};
use wasmtime::{Caller, Linker};

use crate::{
    KernelError,
    futures::FutureSharedState,
    guest_data::{
        GuestError, GuestInt, GuestResult, GuestUint, read_rkyv_value, write_poll_result,
    },
    registry::InstanceRegistry,
};

/// `Contract` is used by kernel drivers to define a consistent method for guest execution.
/// This allows [`Operation`]s to expose the driver contract to the guest without having
/// to know its internal structure.
pub trait Contract {
    type Input: RkyvEncode + Send;
    type Output: RkyvEncode + Send;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + Send + 'static;
}

/// An asynchronous system task that a guest can execute in a non-blocking fashion.
pub struct Operation<Driver> {
    driver: Driver,
    module: &'static str,
}

/// Trait object for operations that can be linked into a Wasmtime linker.
pub trait LinkableOperation: Send + Sync {
    fn link(&self, linker: &mut Linker<InstanceRegistry>) -> Result<(), KernelError>;
}

struct OperationLinker<Driver> {
    operation: Arc<Operation<Driver>>,
}

impl<Driver> LinkableOperation for OperationLinker<Driver>
where
    Driver: Contract + Send + Sync + 'static,
    for<'a> <Driver::Input as rkyv::Archive>::Archived: 'a
        + rkyv::Deserialize<Driver::Input, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    for<'a> <Driver::Output as rkyv::Archive>::Archived: 'a
        + rkyv::Deserialize<Driver::Output, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    fn link(&self, linker: &mut Linker<InstanceRegistry>) -> Result<(), KernelError> {
        self.operation.link(linker)
    }
}

impl<Driver> Operation<Driver>
where
    Driver: Contract,
    for<'a> <Driver::Input as rkyv::Archive>::Archived: 'a
        + rkyv::Deserialize<Driver::Input, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    for<'a> <Driver::Output as rkyv::Archive>::Archived: 'a
        + rkyv::Deserialize<Driver::Output, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    pub fn new(driver: Driver, module: &'static str) -> Arc<Self> {
        Arc::new(Self { driver, module })
    }

    /// Create an operation from a canonical hostcall descriptor.
    pub fn from_hostcall(
        driver: Driver,
        hostcall: &'static Hostcall<Driver::Input, Driver::Output>,
    ) -> Arc<Self> {
        Self::new(driver, hostcall.name())
    }
}

impl<Driver> Operation<Driver>
where
    Driver: Contract + Send + Sync + 'static,
    for<'a> <Driver::Input as rkyv::Archive>::Archived: 'a
        + rkyv::Deserialize<Driver::Input, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    for<'a> <Driver::Output as rkyv::Archive>::Archived: 'a
        + rkyv::Deserialize<Driver::Output, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    pub fn link(
        self: &Arc<Self>,
        linker: &mut Linker<InstanceRegistry>,
    ) -> Result<(), KernelError> {
        let this = self.clone();
        linker.func_wrap(
            self.module,
            "create",
            move |caller: Caller<'_, InstanceRegistry>, args_ptr: GuestInt, args_len: GuestUint| {
                this.create(caller, args_ptr, args_len).map_err(Into::into)
            },
        )?;

        let this = self.clone();
        linker.func_wrap(
            self.module,
            "poll",
            move |caller: Caller<'_, InstanceRegistry>,
                  state_id: GuestUint,
                  task_id: GuestUint,
                  result_ptr: GuestInt,
                  result_capacity: GuestUint| {
                this.poll(caller, state_id, task_id, result_ptr, result_capacity)
                    .map_err(Into::into)
            },
        )?;

        let this = self.clone();
        linker.func_wrap(
            self.module,
            "drop",
            move |caller: Caller<'_, InstanceRegistry>,
                  state_id: GuestUint,
                  result_ptr: GuestInt,
                  result_capacity: GuestUint| {
                this.drop(caller, state_id, result_ptr, result_capacity)
                    .map_err(Into::into)
            },
        )?;

        Ok(())
    }

    fn create(
        self: &Arc<Self>,
        mut caller: Caller<'_, InstanceRegistry>,
        ptr: GuestInt,
        len: GuestUint,
    ) -> Result<GuestUint, KernelError> {
        trace!("Creating future for {}", self.module);

        let input = read_rkyv_value::<Driver::Input>(&mut caller, ptr, len)?;
        let task = self.driver.to_future(&mut caller, input);
        let state = FutureSharedState::new();
        let shared = Arc::clone(&state);
        tokio::spawn(async move {
            let result = task.await.and_then(|out| {
                encode_rkyv(&out)
                    .map_err(|err| GuestError::Kernel(KernelError::Driver(err.to_string())))
            });
            shared.resolve(result);
        });

        let handle = caller.data_mut().insert_future(Arc::clone(&state));

        Ok(handle as GuestUint)
    }

    fn poll(
        self: &Arc<Self>,
        mut caller: Caller<'_, InstanceRegistry>,
        state_id: GuestUint,
        task_id: GuestUint,
        ptr: GuestInt,
        capacity: GuestUint,
    ) -> Result<GuestUint, KernelError> {
        trace!("Polling future for {}", self.module);

        let state_id = usize::try_from(state_id)?;
        let task_id = usize::try_from(task_id)?;

        if let Some(base) = mailbox_base(&mut caller) {
            caller.data().refresh_mailbox(base);
        }

        let guest_result = {
            let registry = caller.data_mut();
            match registry.future_state(state_id) {
                Some(state) => {
                    let waker = registry.waker(task_id);
                    state.register_waker(waker);

                    match state.take_result() {
                        None => Err(GuestError::WouldBlock),
                        Some(output) => {
                            registry.remove_future(state_id);
                            output
                        }
                    }
                }
                None => Err(GuestError::NotFound),
            }
        };

        let written = write_poll_result(
            &mut caller,
            ptr,
            capacity,
            guest_result.inspect_err(|e| {
                if !matches!(e, GuestError::WouldBlock) {
                    debug!("Future failed with error: {e}");
                }
            }),
        )?;
        Ok(written as GuestUint)
    }

    fn drop(
        self: &Arc<Self>,
        mut caller: Caller<'_, InstanceRegistry>,
        state_id: GuestUint,
        ptr: GuestInt,
        capacity: GuestUint,
    ) -> Result<GuestUint, KernelError> {
        trace!("Dropping future for {}", self.module);

        let state_id = usize::try_from(state_id)?;

        let guest_result = {
            let registry = caller.data_mut();
            if let Some(state) = registry.remove_future(state_id) {
                state.abandon();
                Ok(Vec::new())
            } else {
                Err(GuestError::NotFound)
            }
        };

        let written = write_poll_result(&mut caller, ptr, capacity, guest_result)?;
        Ok(written as GuestUint)
    }
}

impl<Driver> Operation<Driver>
where
    Driver: Contract + Send + Sync + 'static,
    for<'a> <Driver::Input as rkyv::Archive>::Archived: 'a
        + rkyv::Deserialize<Driver::Input, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
    for<'a> <Driver::Output as rkyv::Archive>::Archived: 'a
        + rkyv::Deserialize<Driver::Output, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>,
{
    pub fn as_linkable(self: &Arc<Self>) -> Arc<dyn LinkableOperation> {
        Arc::new(OperationLinker {
            operation: Arc::clone(self),
        })
    }
}

fn mailbox_base(caller: &mut Caller<'_, InstanceRegistry>) -> Option<usize> {
    caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .map(|memory| memory.data_ptr(&mut *caller) as usize)
}
