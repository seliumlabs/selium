use std::{convert::TryFrom, future::ready, sync::Arc};

use wasmtime::Caller;

use crate::{
    drivers::Capability,
    guest_data::{GuestError, GuestResult},
    operation::{Contract, Operation},
    registry::{InstanceRegistry, ResourceId, ResourceType},
    session::Session,
};
use selium_abi::{SessionCreate, SessionEntitlement, SessionRemove, SessionResource};

type SessionOps<C> = (
    Arc<Operation<SessionCreateDriver<C>>>,
    Arc<Operation<SessionRemoveDriver<C>>>,
    Arc<Operation<SessionAddEntitlementDriver<C>>>,
    Arc<Operation<SessionRemoveEntitlementDriver<C>>>,
    Arc<Operation<SessionAddResourceDriver<C>>>,
    Arc<Operation<SessionRemoveResourceDriver<C>>>,
);

/// Capability responsible for session lifecycles.
pub trait SessionLifecycleCapability {
    type Error: Into<GuestError>;

    /// Create a new session with no entitlements
    fn create(&self, parent: &Session, pubkey: [u8; 32]) -> Result<Session, Self::Error>;
    /// Add an entitlement to the given session
    fn add_entitlement(
        &self,
        target: &mut Session,
        entitlement: Capability,
    ) -> Result<(), Self::Error>;
    /// Remove an entitlement from the given session
    fn rm_entitlement(
        &self,
        target: &mut Session,
        entitlement: Capability,
    ) -> Result<(), Self::Error>;
    /// Add a resource to an entitlement
    fn add_resource(
        &self,
        target: &mut Session,
        entitlement: Capability,
        resource: ResourceId,
    ) -> Result<bool, Self::Error>;
    /// Remove a resource from an entitlement
    fn rm_resource(
        &self,
        target: &mut Session,
        entitlement: Capability,
        resource: ResourceId,
    ) -> Result<bool, Self::Error>;
    /// Delete a session
    fn remove(&self, target: &Session) -> Result<(), Self::Error>;
}

impl<T> SessionLifecycleCapability for Arc<T>
where
    T: SessionLifecycleCapability,
{
    type Error = T::Error;

    fn create(&self, parent: &Session, pubkey: [u8; 32]) -> Result<Session, Self::Error> {
        self.as_ref().create(parent, pubkey)
    }

    fn add_entitlement(
        &self,
        target: &mut Session,
        entitlement: Capability,
    ) -> Result<(), Self::Error> {
        self.as_ref().add_entitlement(target, entitlement)
    }

    fn rm_entitlement(
        &self,
        target: &mut Session,
        entitlement: Capability,
    ) -> Result<(), Self::Error> {
        self.as_ref().rm_entitlement(target, entitlement)
    }

    fn add_resource(
        &self,
        target: &mut Session,
        entitlement: Capability,
        resource: ResourceId,
    ) -> Result<bool, Self::Error> {
        self.as_ref().add_resource(target, entitlement, resource)
    }

    fn rm_resource(
        &self,
        target: &mut Session,
        entitlement: Capability,
        resource: ResourceId,
    ) -> Result<bool, Self::Error> {
        self.as_ref().rm_resource(target, entitlement, resource)
    }

    fn remove(&self, target: &Session) -> Result<(), Self::Error> {
        self.as_ref().remove(target)
    }
}

pub struct SessionCreateDriver<Impl>(Impl);
pub struct SessionAddEntitlementDriver<Impl>(Impl);
pub struct SessionRemoveEntitlementDriver<Impl>(Impl);
pub struct SessionAddResourceDriver<Impl>(Impl);
pub struct SessionRemoveResourceDriver<Impl>(Impl);
pub struct SessionRemoveDriver<Impl>(Impl);

impl<Impl> Contract for SessionCreateDriver<Impl>
where
    Impl: SessionLifecycleCapability + Clone + Send + 'static,
{
    type Input = SessionCreate;
    type Output = u32;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let SessionCreate { session_id, pubkey } = input;

        let result = (|| -> GuestResult<u32> {
            let parent_slot = session_id as usize;
            let new_session = match caller
                .data()
                .with::<Session, _>(parent_slot, |session| inner.clone().create(session, pubkey))
            {
                Some(Ok(session)) => session,
                Some(Err(err)) => return Err(err.into()),
                None => return Err(GuestError::NotFound),
            };

            let slot = {
                caller
                    .data_mut()
                    .insert(new_session, None, ResourceType::Session)
                    .map_err(GuestError::from)?
            };

            {
                let granted = caller
                    .data()
                    .with::<Session, _>(parent_slot, |session| {
                        session.grant_resource(Capability::SessionLifecycle, slot)
                    })
                    .ok_or(GuestError::NotFound)?;

                if !granted {
                    return Err(GuestError::PermissionDenied);
                }
            }

            let handle = u32::try_from(slot).map_err(|_| GuestError::InvalidArgument)?;
            Ok(handle)
        })();

        ready(result)
    }
}

impl<Impl> Contract for SessionAddEntitlementDriver<Impl>
where
    Impl: SessionLifecycleCapability + Clone + Send + 'static,
{
    type Input = SessionEntitlement;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let SessionEntitlement {
            session_id,
            target_id,
            capability,
        } = input;

        let result = (|| -> GuestResult<()> {
            let session_slot = session_id as usize;
            let target_slot = target_id as usize;

            let authorised = caller
                .data()
                .with::<Session, _>(session_slot, |parent| {
                    parent.authorise(Capability::SessionLifecycle, target_slot)
                })
                .ok_or(GuestError::NotFound)?;

            if !authorised {
                return Err(GuestError::PermissionDenied);
            }

            match caller
                .data_mut()
                .with::<Session, _>(target_slot, move |target| {
                    inner.clone().add_entitlement(target, capability)
                }) {
                Some(Ok(())) => Ok(()),
                Some(Err(err)) => Err(err.into()),
                None => Err(GuestError::NotFound),
            }
        })();

        ready(result)
    }
}

impl<Impl> Contract for SessionRemoveEntitlementDriver<Impl>
where
    Impl: SessionLifecycleCapability + Clone + Send + 'static,
{
    type Input = SessionEntitlement;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let SessionEntitlement {
            session_id,
            target_id,
            capability,
        } = input;

        let result = (|| -> GuestResult<()> {
            let session_slot = session_id as usize;
            let target_slot = target_id as usize;

            let authorised = caller
                .data()
                .with::<Session, _>(session_slot, |parent| {
                    parent.authorise(Capability::SessionLifecycle, target_slot)
                })
                .ok_or(GuestError::NotFound)?;

            if !authorised {
                return Err(GuestError::PermissionDenied);
            }

            match caller
                .data_mut()
                .with::<Session, _>(target_slot, move |target| {
                    inner.clone().rm_entitlement(target, capability)
                }) {
                Some(Ok(())) => Ok(()),
                Some(Err(err)) => Err(err.into()),
                None => Err(GuestError::NotFound),
            }
        })();

        ready(result)
    }
}

impl<Impl> Contract for SessionAddResourceDriver<Impl>
where
    Impl: SessionLifecycleCapability + Clone + Send + 'static,
{
    type Input = SessionResource;
    type Output = u32;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let SessionResource {
            session_id,
            target_id,
            capability,
            resource_id,
        } = input;

        let result = (|| -> GuestResult<u32> {
            let session_slot = session_id as usize;
            let target_slot = target_id as usize;
            let resource_slot =
                ResourceId::try_from(resource_id).map_err(|_| GuestError::InvalidArgument)?;

            let authorised = caller
                .data()
                .with::<Session, _>(session_slot, |parent| {
                    parent.authorise(Capability::SessionLifecycle, target_slot)
                })
                .ok_or(GuestError::NotFound)?;

            if !authorised {
                return Err(GuestError::PermissionDenied);
            }

            match caller
                .data_mut()
                .with::<Session, _>(target_slot, move |target| {
                    inner
                        .clone()
                        .add_resource(target, capability, resource_slot)
                }) {
                Some(Ok(true)) => Ok(1),
                Some(Ok(false)) => Ok(0),
                Some(Err(err)) => Err(err.into()),
                None => Err(GuestError::NotFound),
            }
        })();

        ready(result)
    }
}

impl<Impl> Contract for SessionRemoveResourceDriver<Impl>
where
    Impl: SessionLifecycleCapability + Clone + Send + 'static,
{
    type Input = SessionResource;
    type Output = u32;

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let SessionResource {
            session_id,
            target_id,
            capability,
            resource_id,
        } = input;

        let result = (|| -> GuestResult<u32> {
            let session_slot = session_id as usize;
            let target_slot = target_id as usize;
            let resource_slot =
                ResourceId::try_from(resource_id).map_err(|_| GuestError::InvalidArgument)?;

            let authorised = caller
                .data()
                .with::<Session, _>(session_slot, |parent| {
                    parent.authorise(Capability::SessionLifecycle, target_slot)
                })
                .ok_or(GuestError::NotFound)?;

            if !authorised {
                return Err(GuestError::PermissionDenied);
            }

            match caller
                .data_mut()
                .with::<Session, _>(target_slot, move |target| {
                    inner.clone().rm_resource(target, capability, resource_slot)
                }) {
                Some(Ok(removed)) => Ok(if removed { 1 } else { 0 }),
                Some(Err(err)) => Err(err.into()),
                None => Err(GuestError::NotFound),
            }
        })();

        ready(result)
    }
}

impl<Impl> Contract for SessionRemoveDriver<Impl>
where
    Impl: SessionLifecycleCapability + Clone + Send + 'static,
{
    type Input = SessionRemove;
    type Output = ();

    fn to_future(
        &self,
        caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let inner = self.0.clone();
        let SessionRemove {
            session_id,
            target_id,
        } = input;

        let result = (|| -> GuestResult<()> {
            let session_slot = session_id as usize;
            let target_slot = target_id as usize;

            let authorised = caller
                .data()
                .with::<Session, _>(session_slot, |parent| {
                    parent.authorise(Capability::SessionLifecycle, target_slot)
                })
                .ok_or(GuestError::NotFound)?;

            if !authorised {
                return Err(GuestError::PermissionDenied);
            }

            if let Some(Err(err)) = caller
                .data()
                .with::<Session, _>(target_slot, |target| inner.clone().remove(target))
            {
                return Err(err.into());
            }

            caller.data_mut().remove::<Session>(target_slot);

            match caller.data().with::<Session, _>(session_slot, |session| {
                session.revoke_resource(Capability::SessionLifecycle, target_slot)
            }) {
                Some(Ok(_)) => Ok(()),
                Some(Err(err)) => Err(err.into()),
                None => Err(GuestError::NotFound),
            }
        })();

        ready(result)
    }
}

pub fn operations<C>(cap: C) -> SessionOps<C>
where
    C: SessionLifecycleCapability + Clone + Send + 'static,
{
    (
        Operation::from_hostcall(
            SessionCreateDriver(cap.clone()),
            selium_abi::hostcall_contract!(SESSION_CREATE),
        ),
        Operation::from_hostcall(
            SessionRemoveDriver(cap.clone()),
            selium_abi::hostcall_contract!(SESSION_REMOVE),
        ),
        Operation::from_hostcall(
            SessionAddEntitlementDriver(cap.clone()),
            selium_abi::hostcall_contract!(SESSION_ADD_ENTITLEMENT),
        ),
        Operation::from_hostcall(
            SessionRemoveEntitlementDriver(cap.clone()),
            selium_abi::hostcall_contract!(SESSION_RM_ENTITLEMENT),
        ),
        Operation::from_hostcall(
            SessionAddResourceDriver(cap.clone()),
            selium_abi::hostcall_contract!(SESSION_ADD_RESOURCE),
        ),
        Operation::from_hostcall(
            SessionRemoveResourceDriver(cap),
            selium_abi::hostcall_contract!(SESSION_RM_RESOURCE),
        ),
    )
}
