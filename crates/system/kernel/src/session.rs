use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use thiserror::Error;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{
    drivers::{Capability, session::SessionLifecycleCapability},
    guest_data::GuestError,
    registry::ResourceId,
};

type Result<T, E = SessionError> = std::result::Result<T, E>;

pub struct Session {
    /// The registry ID for this session.
    id: Uuid,
    /// The registry ID for the session that created this one.
    parent: Uuid,
    /// Capabilities that this session is entitled to consume, and which resources it may
    /// consume the capability for.
    entitlements: HashMap<Capability, ResourceScope>,
    /// Public key for this session holder; used for identifying valid payloads.
    _pubkey: [u8; 32],
}

/// The resources accessible by a capability grant.
/// None = "cannot use this capability on any resources",
/// Some = "can only use this capability on the given resources",
/// Any = "can use this capability on any resource"
pub enum ResourceScope {
    None,
    Some(HashSet<ResourceId>),
    Any,
}

#[derive(Error, Debug)]
pub enum SessionError {
    #[error("invalid payload signature")]
    InvalidSignature,
    #[error("session not authorised to perform this action")]
    Unauthorised,
    #[error("attempted to create a session with entitlements beyond its own")]
    EntitlementScope,
    #[error("attempted to revoke a resource from 'Any' scope")]
    RevokeOnAny,
}

impl Session {
    /// This is a highly privileged kernel function and should only be used by the
    /// runtime to bootstrap the first guest session. This should *never* be exposed to
    /// the userland. For userland session creation, use [`Self::create`] instead.
    ///
    /// Note that we don't accept any entitlement resource restrictions as they won't yet
    /// exist. Best practice is to send every capability enabled in the current kernel.
    pub fn bootstrap(entitlements: Vec<Capability>, pubkey: [u8; 32]) -> Self {
        let entitlements =
            HashMap::from_iter(entitlements.into_iter().map(|id| (id, ResourceScope::Any)));

        Self {
            id: Uuid::new_v4(),
            parent: Uuid::nil(),
            entitlements,
            _pubkey: pubkey,
        }
    }

    /// Create a new session, which will be linked to this one. Note that a session
    /// cannot create a new session with privileges beyond its own.
    ///
    /// Note that sessions are mutable, so the privileges rule is only valid at creation
    /// time. It is perfectly possible (and valid) for a session to have its scope
    /// reduced subsequently, making the owning session _less than_ the child session.
    pub fn create(
        &self,
        entitlements: HashMap<Capability, ResourceScope>,
        pubkey: [u8; 32],
    ) -> Result<Self> {
        let mut entitlements = entitlements;

        for (cap, child_scope) in entitlements.iter_mut() {
            let parent_scope = self
                .entitlements
                .get(cap)
                .ok_or(SessionError::EntitlementScope)?;

            if !parent_scope.contains(child_scope) {
                return Err(SessionError::EntitlementScope)?;
            }
        }

        Ok(Self {
            id: Uuid::new_v4(),
            parent: self.id,
            entitlements,
            _pubkey: pubkey,
        })
    }

    /// Authenticate a payload against this session's public key. If successful, the
    /// payload is an authentic payload for this session and can be trusted. Otherwise
    /// this payload is counterfit, meaning either that one or both of session Id and
    /// request payload have been forged.
    pub fn authenticate(&self, _payload: &[u8], _signature: &[u8]) -> bool {
        let success = true; // ...do auth here

        if success {
            debug!(session = %self.id, status = "success", "authenticate");
        } else {
            warn!(session = %self.id, status = "fail", "authenticate");
        }

        // success
        todo!()
    }

    /// Authorise the requested action against the set of entitlements for this session.
    /// If successful, the action can safely be executed for the given resource.
    /// Otherwise the action is outside the permission scope and should not be executed.
    pub fn authorise(&self, capability: Capability, resource_id: ResourceId) -> bool {
        let success = match self.entitlements.get(&capability) {
            Some(ResourceScope::Any) => true,
            Some(ResourceScope::Some(ids)) => ids.contains(&resource_id),
            _ => false,
        };

        if success {
            debug!(session = %self.id, capability = ?capability, resource = resource_id, status = "success", "authorise");
        } else {
            warn!(session = %self.id, capability = ?capability, resource = resource_id, status = "fail", "authorise");
        }

        success
    }

    fn upsert_entitlement(&mut self, entitlement: Capability) {
        self.entitlements
            .insert(entitlement, ResourceScope::Some(HashSet::new()));
    }

    fn remove_entitlement(&mut self, entitlement: Capability) -> Result<(), SessionError> {
        self.entitlements
            .remove(&entitlement)
            .map(|_| ())
            .ok_or(SessionError::EntitlementScope)
    }

    pub(crate) fn grant_resource(&mut self, entitlement: Capability, resource: ResourceId) -> bool {
        match self.entitlements.get_mut(&entitlement) {
            Some(ResourceScope::Some(scope)) => scope.insert(resource),
            Some(scope) if matches!(scope, ResourceScope::None) => {
                let mut set = HashSet::new();
                set.insert(resource);
                *scope = ResourceScope::Some(set);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn revoke_resource(
        &mut self,
        entitlement: Capability,
        resource: ResourceId,
    ) -> Result<bool, SessionError> {
        match self.entitlements.get_mut(&entitlement) {
            Some(scope) if matches!(scope, ResourceScope::Some(_)) => {
                let set = scope.get_set_mut().unwrap();
                let r = set.remove(&resource);
                if set.is_empty() {
                    *scope = ResourceScope::None;
                }
                Ok(r)
            }
            Some(ResourceScope::Any) => Err(SessionError::RevokeOnAny),
            _ => Ok(false),
        }
    }

    fn ensure_removable(&self) -> Result<(), SessionError> {
        if self.parent.is_nil() {
            Err(SessionError::Unauthorised)
        } else {
            Ok(())
        }
    }
}

impl From<SessionError> for GuestError {
    fn from(value: SessionError) -> Self {
        GuestError::Subsystem(value.to_string())
    }
}

impl ResourceScope {
    fn contains(&self, other: &ResourceScope) -> bool {
        match (self, other) {
            (ResourceScope::Any, _) => true,
            (ResourceScope::Some(_), ResourceScope::None) => true,
            (ResourceScope::Some(self_ids), ResourceScope::Some(other_ids)) => {
                other_ids.iter().all(|id| self_ids.contains(id))
            }
            (ResourceScope::None, ResourceScope::None) => true,
            _ => false,
        }
    }

    fn get_set_mut(&mut self) -> Option<&mut HashSet<ResourceId>> {
        match self {
            Self::Any | Self::None => None,
            Self::Some(set) => Some(set),
        }
    }
}

impl From<SessionError> for i32 {
    fn from(value: SessionError) -> Self {
        match value {
            SessionError::InvalidSignature => -110,
            SessionError::Unauthorised => -111,
            SessionError::EntitlementScope => -112,
            SessionError::RevokeOnAny => -113,
        }
    }
}

/// Concrete driver for session lifecycle capability.
#[derive(Clone)]
pub struct SessionLifecycleDriver;

impl SessionLifecycleDriver {
    /// Create a new session lifecycle driver instance
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl SessionLifecycleCapability for SessionLifecycleDriver {
    type Error = SessionError;

    fn create(&self, me: &Session, pubkey: [u8; 32]) -> Result<Session, Self::Error> {
        me.create(HashMap::new(), pubkey)
    }

    fn add_entitlement(
        &self,
        target: &mut Session,
        entitlement: Capability,
    ) -> Result<(), Self::Error> {
        target.upsert_entitlement(entitlement);
        Ok(())
    }

    fn rm_entitlement(
        &self,
        target: &mut Session,
        entitlement: Capability,
    ) -> Result<(), Self::Error> {
        target.remove_entitlement(entitlement)
    }

    fn add_resource(
        &self,
        target: &mut Session,
        entitlement: Capability,
        resource: ResourceId,
    ) -> Result<bool, Self::Error> {
        Ok(target.grant_resource(entitlement, resource))
    }

    fn rm_resource(
        &self,
        target: &mut Session,
        entitlement: Capability,
        resource: ResourceId,
    ) -> Result<bool, Self::Error> {
        target.revoke_resource(entitlement, resource)
    }

    fn remove(&self, target: &Session) -> Result<(), Self::Error> {
        target.ensure_removable()
    }
}
