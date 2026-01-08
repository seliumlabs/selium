//! Hostcall drivers for singleton dependency registration and lookup.

use std::{
    future::{Future, ready},
    sync::Arc,
};

use wasmtime::Caller;

use crate::{
    guest_data::{GuestError, GuestResult},
    operation::{Contract, Operation},
    registry::InstanceRegistry,
};
use selium_abi::{GuestResourceId, SingletonLookup, SingletonRegister};

type SingletonOps = (
    Arc<Operation<SingletonRegisterDriver>>,
    Arc<Operation<SingletonLookupDriver>>,
);

/// Hostcall driver that registers singleton dependencies.
pub struct SingletonRegisterDriver;
/// Hostcall driver that looks up singleton dependencies.
pub struct SingletonLookupDriver;

impl Contract for SingletonRegisterDriver {
    type Input = SingletonRegister;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let registry = caller.data().registry_arc();
        let SingletonRegister { id, resource } = input;

        ready((|| -> GuestResult<Self::Output> {
            let resource_id = registry
                .resolve_shared(resource)
                .ok_or(GuestError::NotFound)?;
            registry.metadata(resource_id).ok_or(GuestError::NotFound)?;
            let inserted = registry.register_singleton(id, resource_id)?;
            if !inserted {
                return Err(GuestError::StableIdExists);
            }
            Ok(())
        })())
    }
}

impl Contract for SingletonLookupDriver {
    type Input = SingletonLookup;
    type Output = GuestResourceId;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let registry = caller.data().registry_arc();
        let SingletonLookup { id } = input;

        ready((|| -> GuestResult<Self::Output> {
            let resource_id = registry.singleton(id).ok_or(GuestError::NotFound)?;
            registry.metadata(resource_id).ok_or(GuestError::NotFound)?;
            registry.share_handle(resource_id).map_err(GuestError::from)
        })())
    }
}

/// Build hostcall operations for singleton registration and lookup.
pub fn operations() -> SingletonOps {
    (
        Operation::from_hostcall(
            SingletonRegisterDriver,
            selium_abi::hostcall_contract!(SINGLETON_REGISTER),
        ),
        Operation::from_hostcall(
            SingletonLookupDriver,
            selium_abi::hostcall_contract!(SINGLETON_LOOKUP),
        ),
    )
}
