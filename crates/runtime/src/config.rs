use std::collections::HashSet;

use selium_abi::{CapabilityGrant, EntrypointMetadata, ProcessId};

/// A single entrypoint argument, in declaration order.
///
/// Integer arguments are carried by value; pointer arguments carry a byte
/// payload that the runtime copies into the guest's linear memory before
/// invoking the entrypoint, passing the `(address, length)` pair instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemGuestArg {
    /// A u64-valued argument passed through as a single `i64` slot.
    Integer(u64),
    /// A pointer argument passed as two `i64` slots: `(address, length)`.
    Pointer(Vec<u8>),
}

/// Condition used to decide when a bootstrapped system guest is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessCondition {
    /// Guest is considered ready immediately after start.
    Immediate,
    /// Guest is ready once the activity log contains the supplied text.
    ActivityLogContains(String),
}

/// Configuration for a system guest that should be bootstrapped by the runtime.
#[derive(Debug, Clone)]
pub struct SystemGuestDescriptor {
    /// Stable system guest name.
    pub name: String,
    /// Module id used to register and start the guest.
    pub module_id: String,
    /// WebAssembly module bytes.
    pub module_bytes: Vec<u8>,
    /// Entrypoint export to invoke.
    pub entrypoint: String,
    /// Entrypoint arguments, in declaration order.
    pub arguments: Vec<SystemGuestArg>,
    /// Capability grants assigned to the guest process.
    pub grants: Vec<CapabilityGrant>,
    /// Names of system guests that must become ready first.
    pub dependencies: Vec<String>,
    /// Readiness condition for this guest.
    pub readiness: ReadinessCondition,
    /// Tenant identity for this guest. `None` means "platform tenant".
    /// Children spawned by this guest inherit this tenant.
    pub tenant: Option<String>,
    /// Serving role this guest declares: the internal route URI it registers
    /// itself (e.g. `sel:///dns/resolve` or `sel://acme/bridge`). When set,
    /// readiness additionally requires the registration to be observable in
    /// discovery (recorded by the discovery service), replacing the runtime's
    /// old well-known-URI provisioning. `mark_ready()` stays a bare signal.
    pub serving_role: Option<String>,
    /// Protocol handler names this guest serves (e.g. `sel-http` for
    /// `selium-connector-http`). The runtime records these so serve-side
    /// guests can pin their listeners to the handler process via
    /// `ResolveProtocolHandler`; the registry is revoked on teardown.
    pub handlers: Vec<String>,
}

/// Runtime bootstrap configuration.
#[derive(Debug, Clone, Default)]
pub struct RuntimeConfig {
    /// System guests to start during bootstrap.
    pub system_guests: Vec<SystemGuestDescriptor>,
    /// When true, the runtime creates the discovery pub/sub feed ring and
    /// discovery listener, and wires them into the discovery system guest.
    pub start_discovery: bool,
    /// Out-of-band domain→tenant mapping, seeded into the discovery service
    /// at startup (advisory: routes and scopes names, never authenticates).
    pub domain_table: Vec<(String, String)>,
}

/// Guest successfully started during bootstrap.
#[derive(Debug, Clone)]
pub struct BootstrappedGuest {
    /// System guest name.
    pub name: String,
    /// Process id assigned to the guest.
    pub process_id: ProcessId,
}

/// Report returned after bootstrapping system guests.
#[derive(Debug, Clone, Default)]
pub struct BootstrapReport {
    /// Guests started successfully, in bootstrap order.
    pub guests: Vec<BootstrappedGuest>,
}

/// Capability authority persisted for a process.
#[derive(Debug, Clone)]
pub struct ProcessAuthority {
    /// Grants assigned to the process.
    pub grants: Vec<CapabilityGrant>,
    /// Tenant identity assigned at spawn (inherited from parent or explicit
    /// host assignment for system guests). `None` means "platform tenant".
    pub tenant: Option<String>,
    /// Parent process id, if spawned by another process. `None` for
    /// bootstrapped system guests. Used by the `Children` selector.
    pub parent: Option<ProcessId>,
    /// Queue ids returned to this process by successful discovery Resolve
    /// calls. Provides an authorisation basis for cross-process
    /// `HostQueueAttach` without requiring an `ExplicitResource` grant.
    pub resolved_queue_ids: HashSet<u64>,
    /// Region ids returned to this process by successful discovery Resolve
    /// calls. Provides an authorisation basis for `AttachRegion` on a region
    /// the process did not allocate (e.g. an identity guest's live tables).
    pub resolved_region_ids: HashSet<u64>,
}

impl SystemGuestDescriptor {
    /// Builds a system guest descriptor from generated entrypoint metadata.
    pub fn from_entrypoint_metadata(
        name: impl Into<String>,
        module_id: impl Into<String>,
        module_bytes: Vec<u8>,
        metadata: EntrypointMetadata,
        grants: Vec<CapabilityGrant>,
    ) -> Self {
        Self {
            name: name.into(),
            module_id: module_id.into(),
            module_bytes,
            entrypoint: metadata.name,
            arguments: Vec::new(),
            grants,
            dependencies: Vec::new(),
            readiness: ReadinessCondition::Immediate,
            tenant: None,
            serving_role: None,
            handlers: Vec::new(),
        }
    }

    /// Sets the discovery handle (host queue shared_id) for this guest.
    ///
    /// The discovery handle is prepended as the first entrypoint argument, so
    /// guests whose leading parameter is a `Context` receive it ahead of any
    /// other declared arguments. The `#[entrypoint]` macro constructs the
    /// context for `Context`-leading entrypoints via `Context::from_raw`;
    /// entrypoints declaring an untyped leading `u64` receive the raw handle
    /// directly.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Create a host queue for discovery.
    /// let discovery_queue = kernel.create_host_queue();
    /// let discovery_shared_id = discovery_queue.shared_id;
    ///
    /// // Configure the discovery guest with the shared_id.
    /// let mut discovery_guest = SystemGuestDescriptor::from_entrypoint_metadata(
    ///     "discovery", "discovery-module", module_bytes, metadata, grants,
    /// );
    /// discovery_guest.set_discovery_handle(discovery_shared_id);
    ///
    /// // Configure application guests with the same shared_id.
    /// let mut app_guest = SystemGuestDescriptor::from_entrypoint_metadata(
    ///     "my-app", "app-module", app_bytes, app_metadata, app_grants,
    /// );
    /// app_guest.set_discovery_handle(discovery_shared_id);
    /// app_guest.dependencies.push("discovery".to_string());
    /// ```
    pub fn set_discovery_handle(&mut self, shared_id: u64) {
        self.arguments.insert(0, SystemGuestArg::Integer(shared_id));
    }

    /// Sets both the discovery feed ring region id and the discovery RPC
    /// listener shared id for the discovery system guest itself.
    ///
    /// The feed ring id is passed as the first entrypoint argument and the
    /// listener shared id as the second.
    pub fn set_discovery_feed_and_handle(&mut self, feed_region_id: u64, listener_shared_id: u64) {
        self.arguments = vec![
            SystemGuestArg::Integer(feed_region_id),
            SystemGuestArg::Integer(listener_shared_id),
        ];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_argument_carries_u64_values() {
        assert_eq!(SystemGuestArg::Integer(7), SystemGuestArg::Integer(7));
        assert_ne!(SystemGuestArg::Integer(7), SystemGuestArg::Integer(8));
        assert_ne!(SystemGuestArg::Integer(7), SystemGuestArg::Pointer(vec![]));
    }

    #[test]
    fn pointer_argument_carries_payload() {
        let arg = SystemGuestArg::Pointer(b"udp://127.0.0.1:53".to_vec());
        match arg {
            SystemGuestArg::Pointer(bytes) => assert_eq!(bytes, b"udp://127.0.0.1:53"),
            other => panic!("expected pointer argument, got {other:?}"),
        }
    }

    #[test]
    fn empty_arguments_is_default() {
        let descriptor = SystemGuestDescriptor {
            name: "guest".to_string(),
            module_id: "m".to_string(),
            module_bytes: vec![0, 97, 115, 109],
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: Vec::new(),
            dependencies: Vec::new(),
            readiness: ReadinessCondition::Immediate,
            tenant: None,
            serving_role: None,
            handlers: Vec::new(),
        };
        assert!(descriptor.arguments.is_empty());
    }
}
