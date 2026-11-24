//! Discovery system guest.
//!
//! The store is a fed index over one deterministic URI taxonomy:
//! `sel://<tenant>/<type>/<id>` (typed), `sel://<tenant>/<path…>` (named
//! service routes), and leaf aliases. External wire names are projections of
//! this space: `resolve_wire_name` strips the tenant domain (longest match
//! against the advisory domain table, defaulting to the synthetic tenant
//! label) and reverses the remaining labels into a path. Everything is
//! tenant-scoped; the empty tenant is the reserved root namespace.
//!
//! Owner-registered routes carry a lease (see [`LEASE_TTL_MS`]): an owner
//! renews it through `DiscoveryRequest::Renew`, and the lease sweep revokes a
//! lapsed registration as if its owner had revoked it.
//!
//! # Authority versus projection
//!
//! Admission decisions read the **host's** authoritative record for
//! `(class, id)` (existence, owner, class, mint-time tenant) through the
//! `ProcessOwnsResource` hostcall. The registry store is a projection: it can
//! lag a resource the caller has just created, and it never authorises a
//! registration by itself. Canonical typed URIs are derived keys, so they may
//! resolve to `NotFound` until their owner publishes (or the projection
//! catches up); that miss is transient, not a permission decision.
//!
//! Resolution is likewise **not** an authorisation basis on its own. A
//! resolved shared resource becomes an attach basis only through the host's
//! owner-declared `DeclareAttachableByResolution` path, which the runtime
//! enforces; discovery merely records the resolve for a resource whose owner
//! already declared it attachable.
//!
//! The runtime→discovery feed is revocation-only: `ProcessGone { process_id }`
//! removes owner-keyed records (process node, named routes, aliases, health)
//! and `ResourceFreed { class, id }` removes identity-keyed typed records when
//! the host actually frees the resource. Domains stay on the identity-guest
//! `SetDomain` path.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex},
};

use anyhow::Context as _;
use selium_abi::{Capability, ProcessId, ResourceAuthority, ResourceClass, ResourceKind, uri};
use selium_guest::{AttachDeclarationScope, InterfaceMetadata, entrypoint, pattern_interface};
use selium_service::{DiscoveryRequest, DiscoveryResponse, DomainEntry, ResourceTarget};
use selium_shm::{Channel, ChannelBackpressure, transport::ShmTransport};
use selium_wire::{
    LiveTable,
    framed::{FramedRead, FramedWrite},
    pubsub::{Publisher, Subscriber},
};

/// The route-change live table: key = registered URI, value = target.
pub type RouteTable = LiveTable<String, ResourceTarget, ShmTransport>;

pub const DISCOVERY_EXCHANGE: &str = "selium.discovery.resolve";
pub const INTERFACE_METADATA_TABLE: &str = "selium.discovery.interfaces";
/// Lease lifetime: a guest registration lapses if its owner does not renew it
/// within this window. Shared with the SDK's renewal task, which renews at half
/// this interval.
pub const LEASE_TTL_MS: u64 = selium_service::REGISTRATION_LEASE_TTL_MS;
pub const REGISTRATION_LOG: &str = "selium.discovery.registrations";
/// The root-namespace route the discovery route-change table is served on.
pub const ROUTE_TABLE_ROUTE: &str = "sel:///discovery-routes";
pub const URI_LIVE_TABLE: &str = "selium.discovery.uri-table";

#[pattern_interface]
pub trait DiscoveryControl {
    fn register(target: ResourceTarget);
    fn remove(uri: String);
    fn resolve_exact(uri: String);
    fn resolve_prefix(prefix: String);
}

/// A guest registration's lease: the owning process and the wall-clock
/// deadline it must renew before.
#[derive(Debug, Clone, Copy)]
pub struct Lease {
    /// Process that owns the registration.
    pub owner: ProcessId,
    /// Wall-clock deadline in milliseconds since the Unix epoch.
    pub expires_at_ms: u64,
}

/// Outcome of classifying a registration against the host authority.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RegistrationAdmission {
    /// The raw form resolved to this internal key and is admitted.
    Admitted(String),
    /// The caller owns the id, but under a different class than it claimed.
    ClassMismatch,
    /// The caller owns no resource with the claimed `(class, id)`.
    Unattested,
    /// The registration is refused for a validation or permission reason.
    Refused,
}

#[derive(Debug, Clone, Default)]
pub struct DiscoveryStore {
    /// Exact-key registrations: typed canonical URIs, root well-known URIs,
    /// and named service routes (`sel://<tenant>/<path…>`). An alias is not
    /// stored here: it resolves only through its canonical typed target.
    registrations: BTreeMap<String, ResourceTarget>,
    /// Lease per owner-registered route: liveness is observable registration.
    /// Host-authored (Tier-1) registrations are not leased; they are revoked
    /// on process exit by the runtime.
    leases: HashMap<String, Lease>,
    /// Leaf aliases: alias URI → canonical typed URI.
    aliases: HashMap<String, String>,
    /// Reverse alias index: canonical typed URI → alias URIs, for reverse
    /// revocation when a target is revoked.
    alias_backrefs: HashMap<String, Vec<String>>,
    /// Label index: `(key, value)` → canonical typed URIs.
    label_index: HashMap<(String, String), BTreeSet<String>>,
    /// Advisory domain→tenant table, provisioned out-of-band (Tier-1 seed).
    /// Routes and scopes names; never authenticates.
    domains: uri::DomainTable,
    /// Owning process per registered route key (typed URIs, named-service
    /// routes, aliases, and translated wire-name registrations). Used for
    /// owner-keyed revocation on process exit.
    route_owners: HashMap<String, ProcessId>,
    /// Root-service designation per tenant: the bare domain (apex) resolves
    /// to this internal URI.
    root_services: HashMap<String, String>,
}

impl DiscoveryStore {
    /// Seeds the advisory domain→tenant table (out-of-band provisioning).
    pub fn seed_domain(&mut self, domain: &str, tenant: &str) {
        self.domains.seed(domain, tenant);
    }

    /// Returns whether the domain table maps `domain`.
    pub fn domain_tenant(&self, domain: &str) -> Option<&str> {
        self.domains.get(domain)
    }

    /// Records or refreshes a guest registration's lease.
    pub fn set_lease(&mut self, uri: &str, owner: ProcessId, now_ms: u64) {
        self.leases.insert(
            uri.to_string(),
            Lease {
                owner,
                expires_at_ms: now_ms.saturating_add(LEASE_TTL_MS),
            },
        );
    }

    /// Renews a guest registration's lease if `owner` holds it. Returns
    /// whether the lease was renewed.
    pub fn renew_lease(&mut self, uri: &str, owner: ProcessId, now_ms: u64) -> bool {
        match self.leases.get_mut(uri) {
            Some(lease) if lease.owner == owner => {
                lease.expires_at_ms = now_ms.saturating_add(LEASE_TTL_MS);
                true
            }
            _ => false,
        }
    }

    /// Expires every lapsed lease, revoking the registration as if its owner
    /// had revoked it. Watchers observe the resulting revocation.
    pub fn expire_leases(&mut self, now_ms: u64) {
        let expired: Vec<String> = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.expires_at_ms <= now_ms)
            .map(|(uri, _)| uri.clone())
            .collect();
        for uri in expired {
            if self.aliases.contains_key(&uri) {
                self.revoke_alias(&uri);
            } else {
                self.revoke_key(&uri);
            }
        }
    }

    /// Sets or clears a guest's health record for `name` under `tenant`.
    ///
    /// Health is published state, not a resource route: it is not lease-expired
    /// (a lapsing lease would mark every healthy long-running guest unhealthy
    /// until its owner renewed it again) and is not subject to the typed-resource
    /// ownership check. Liveness is carried by the route leases, and the record
    /// itself is revoked when its owner's process exits.
    pub fn set_health(
        &mut self,
        tenant: Option<&str>,
        caller: ProcessId,
        name: &str,
        healthy: bool,
    ) {
        let uri = health_uri(tenant, name);
        if healthy {
            let target = ResourceTarget {
                uri: uri.clone(),
                host_id: String::new(),
                resource_id: caller,
                interface: None,
                tenant: tenant.map(str::to_string),
                class: selium_abi::ResourceClass::Process,
                labels: Vec::new(),
            };
            self.registrations.insert(uri.clone(), target);
            self.route_owners.insert(uri.clone(), caller);
        } else {
            self.revoke_key(&uri);
        }
    }

    /// Stores a registration under its exact key and maintains the label
    /// index for typed targets.
    ///
    /// The owner is the caller that published it. Ownership itself is never
    /// inferred from this store: admission asks the host authority.
    fn store(&mut self, target: ResourceTarget, owner: Option<ProcessId>) {
        if let Some(process_id) = owner {
            self.route_owners.insert(target.uri.clone(), process_id);
        }
        // Remove any prior label associations for this URI before indexing the
        // replacement: retargeting must not leave labels from the old target
        // resolvable to the new one.
        self.remove_from_label_index(&target.uri);
        if uri::parse_typed(&target.uri).is_some() {
            for label in &target.labels {
                self.label_index
                    .entry((label.key.clone(), label.value.clone()))
                    .or_default()
                    .insert(target.uri.clone());
            }
        }
        self.registrations.insert(target.uri.clone(), target);
    }

    /// Removes `uri` from every label reverse index, dropping empty entries.
    fn remove_from_label_index(&mut self, uri: &str) {
        let mut empty_labels = Vec::new();
        for ((label_key, label_value), uris) in self.label_index.iter_mut() {
            uris.remove(uri);
            if uris.is_empty() {
                empty_labels.push((label_key.clone(), label_value.clone()));
            }
        }
        for label in empty_labels {
            self.label_index.remove(&label);
        }
    }

    /// Removes a registration by exact key, revoking any aliases that resolve
    /// to it and cleaning its label index entries.
    fn revoke_key(&mut self, key: &str) {
        self.registrations.remove(key);
        self.leases.remove(key);
        // Reverse alias revocation: revoking a target revokes its aliases
        // (and, for single-segment routes registered under those alias keys,
        // their exact-key entries too).
        if let Some(aliases) = self.alias_backrefs.remove(key) {
            for alias in aliases {
                self.revoke_alias(&alias);
            }
        }
        // Drop label index entries referencing the removed target.
        self.remove_from_label_index(key);
        // Clean up the root-service designation if it pointed here.
        self.root_services.retain(|_, uri| uri != key);
        self.route_owners.remove(key);
    }

    /// Removes an alias registration (guest/Tier-2 alias, or a Tier-1
    /// revoke-by-alias-key). Returns the alias's canonical key, if any.
    ///
    /// A single-segment named-service route is registered both as an exact
    /// key (the caller's serving target) and as an alias (the cascade
    /// backref), so revoking it must remove both.
    fn revoke_alias(&mut self, alias: &str) {
        if let Some(canonical) = self.aliases.remove(alias)
            && let Some(backrefs) = self.alias_backrefs.get_mut(&canonical)
        {
            backrefs.retain(|a| a != alias);
            if backrefs.is_empty() {
                self.alias_backrefs.remove(&canonical);
            }
        }
        self.registrations.remove(alias);
        self.leases.remove(alias);
        self.route_owners.remove(alias);
        self.root_services.retain(|_, uri| uri != alias);
    }

    /// Revokes every owner-keyed record belonging to a process: its process
    /// node, named routes, aliases, and health records. Identity-keyed typed
    /// records (regions, queues, volumes) are removed only by
    /// `ResourceFreed`, so a co-owner exiting never deletes a record another
    /// owner still needs.
    pub fn revoke_by_owner(&mut self, process_id: ProcessId) {
        let owned_keys: Vec<String> = self
            .route_owners
            .iter()
            .filter(|(uri, owner)| **owner == process_id && is_owner_keyed(uri))
            .map(|(uri, _)| uri.clone())
            .collect();
        for key in owned_keys {
            if self.aliases.contains_key(&key) {
                self.revoke_alias(&key);
            } else {
                self.revoke_key(&key);
            }
        }
    }

    /// Revokes every registration whose target is the named resource identity
    /// (class + id). The registry recorded the resource's tenant at
    /// registration, so a resource minted under a serving tenant is revoked
    /// under the tenant it was minted for — the runtime needs no tenant side
    /// map.
    ///
    /// Aliases are collected by their canonical target as well as through the
    /// registration cascade, because an alias may have been admitted before its
    /// canonical record was projected: no `registrations` entry then exists for
    /// [`revoke_key`](Self::revoke_key) to cascade from.
    pub fn revoke_resource(&mut self, class: &str, id: u64) {
        let mut keys: Vec<String> = self
            .registrations
            .iter()
            .filter(|(_, target)| target.resource_id == id && target.class.uri_segment() == class)
            .map(|(uri, _)| uri.clone())
            .collect();
        keys.extend(
            self.aliases
                .iter()
                .filter(|(_, canonical)| {
                    uri::parse_typed(canonical).is_some_and(|(_, target_class, target_id)| {
                        target_id == id && target_class.uri_segment() == class
                    })
                })
                .map(|(alias, _)| alias.clone()),
        );
        for key in keys {
            if self.aliases.contains_key(&key) {
                self.revoke_alias(&key);
            } else {
                self.revoke_key(&key);
            }
        }
    }

    /// Applies a volatile Tier-1 event from the runtime feed.
    ///
    /// Tier-1 authority comes from the *transport* (the runtime feed), never
    /// from the URI string. The feed carries revocations only.
    fn apply_tier1_event(&mut self, request: DiscoveryRequest) {
        match request {
            DiscoveryRequest::ProcessGone { process_id } => {
                self.revoke_by_owner(process_id);
            }
            DiscoveryRequest::ResourceFreed { class, id } => {
                self.revoke_resource(&class, id);
            }
            // Query and Tier-2 variants never arrive over the feed.
            DiscoveryRequest::Register { .. }
            | DiscoveryRequest::Revoke { .. }
            | DiscoveryRequest::Resolve(_)
            | DiscoveryRequest::ResolvePrefix(_)
            | DiscoveryRequest::ResolveLabels { .. }
            | DiscoveryRequest::Renew { .. }
            | DiscoveryRequest::SetHealth { .. }
            | DiscoveryRequest::SetDomain { .. }
            | DiscoveryRequest::ListDomains => {}
        }
    }

    /// Resolves an exact URI (typed, named-service route, leaf alias, or
    /// external wire name) with optional tenant scoping.
    ///
    /// An external wire name is translated through the unified resolver: the
    /// tenant domain is stripped (longest domain-table match, falling back to
    /// the synthetic tenant label), the remaining labels are reversed into a
    /// path, and that internal path is resolved. The bare domain (apex)
    /// resolves through the tenant's designated root service.
    pub fn resolve_exact(&self, uri: &str, caller_tenant: Option<&str>) -> Option<ResourceTarget> {
        match self.resolve_internal(uri, caller_tenant) {
            Some(target) => Some(target),
            None => {
                // External wire name: translate to an internal path.
                if uri::parse_sel(uri).is_some() {
                    return None;
                }
                let (tenant, path) = uri::resolve_wire_name(uri, &self.domains)?;
                if !tenant_admits(caller_tenant, Some(&tenant)) {
                    return None;
                }
                if path.is_empty() {
                    let root_service = self.root_services.get(&tenant)?;
                    return self.resolve_internal(root_service, caller_tenant);
                }
                let internal = format!("{}{}/{}", uri::SEL_PREFIX, tenant, path.join("/"));
                self.resolve_internal(&internal, caller_tenant)
            }
        }
    }

    /// Resolves an internal `sel://` URI: exact registration first, then leaf
    /// aliases, with tenant scoping.
    fn resolve_internal(&self, uri: &str, caller_tenant: Option<&str>) -> Option<ResourceTarget> {
        if let Some(target) = self.registrations.get(uri) {
            return tenant_admits(caller_tenant, target.tenant.as_deref()).then(|| target.clone());
        }
        if let Some(canonical) = self.aliases.get(uri)
            && let Some(target) = self.registrations.get(canonical)
        {
            return tenant_admits(caller_tenant, target.tenant.as_deref()).then(|| target.clone());
        }
        None
    }

    /// Resolves a prefix/`*` enumeration query (`sel://<tenant>/<type>/*` or
    /// `sel://<tenant>/*`) with tenant scoping.
    pub fn resolve_prefix(&self, prefix: &str, caller_tenant: Option<&str>) -> Vec<ResourceTarget> {
        let Some((tenant, path)) = uri::parse_sel(prefix) else {
            return Vec::new();
        };
        let Some(base) = uri::wildcard_prefix(path) else {
            return Vec::new();
        };
        let mut results = self
            .registrations
            .iter()
            .filter_map(|(key, target)| {
                let (target_tenant, target_path) = uri::parse_sel(key)?;
                if target_tenant != tenant {
                    return None;
                }
                if !tenant_admits(caller_tenant, target.tenant.as_deref()) {
                    return None;
                }
                if base.is_empty() {
                    // Enumerate every typed target in the tenant.
                    uri::parse_typed(key)?;
                } else {
                    let first_segment = target_path.split('/').next().unwrap_or("");
                    if first_segment != base {
                        return None;
                    }
                }
                Some(target.clone())
            })
            .collect::<Vec<_>>();
        // Deterministic order from the BTreeMap iteration.
        results.sort_by(|a, b| a.uri.cmp(&b.uri));
        results
    }

    /// Answers a label query: every typed target in the caller's tenant whose
    /// labels match `(key, value)`.
    pub fn resolve_labels(
        &self,
        key: &str,
        value: &str,
        caller_tenant: Option<&str>,
    ) -> Vec<ResourceTarget> {
        let mut results = Vec::new();
        if let Some(uris) = self.label_index.get(&(key.to_string(), value.to_string())) {
            for uri in uris {
                if let Some(target) = self.registrations.get(uri)
                    && tenant_admits(caller_tenant, target.tenant.as_deref())
                {
                    results.push(target.clone());
                }
            }
        }
        results
    }

    /// Translates and validates a registration's URI into its internal
    /// `sel://` form, answering from the host authority rather than the
    /// projected store.
    ///
    /// `authority` is the host's mint-time answer for the claimed target;
    /// `claim_held` says whether the caller holds an exact `RouteClaim` for
    /// the URI being registered. Typed URIs are derived keys: the classified
    /// `(tenant, class, id)` must match both the authority's answer and the
    /// claimed target. Named records must live in the tenant the host minted
    /// the target under; a root name additionally requires its claim.
    ///
    /// Returns `None` when the registration is not admitted. Callers that must
    /// tell a permission failure from an unattested alias target use
    /// [`classify_registration`](Self::classify_registration).
    pub fn registration_uri(
        &self,
        raw: &str,
        caller_tenant: Option<&str>,
        authority: &ResourceAuthority,
        claim_held: bool,
        resource_id: u64,
        class: &selium_abi::ResourceClass,
    ) -> Option<String> {
        match self.classify_registration(
            raw,
            caller_tenant,
            authority,
            claim_held,
            resource_id,
            class,
        ) {
            RegistrationAdmission::Admitted(uri) => Some(uri),
            RegistrationAdmission::ClassMismatch
            | RegistrationAdmission::Unattested
            | RegistrationAdmission::Refused => None,
        }
    }

    /// Classifies a registration attempt against the host authority.
    ///
    /// A class mismatch is reported separately from an unattested identity: the
    /// authority knows the caller owns the id, just not under the class the
    /// registration claims, so the answer is a permission failure rather than a
    /// missing target.
    fn classify_registration(
        &self,
        raw: &str,
        caller_tenant: Option<&str>,
        authority: &ResourceAuthority,
        claim_held: bool,
        resource_id: u64,
        class: &selium_abi::ResourceClass,
    ) -> RegistrationAdmission {
        if !authority.owned {
            return if authority.other_classes.is_empty() {
                RegistrationAdmission::Unattested
            } else {
                RegistrationAdmission::ClassMismatch
            };
        }
        let mint_tenant = authority.tenant.as_deref().unwrap_or("");
        let uri_str = match uri::parse_sel(raw) {
            Some(_) => raw.to_string(),
            None => {
                let Some((domain_tenant, path)) = uri::resolve_wire_name(raw, &self.domains) else {
                    return RegistrationAdmission::Refused;
                };
                if path.is_empty() {
                    // A bare domain designates no service of its own.
                    return RegistrationAdmission::Refused;
                }
                format!("{}{}/{}", uri::SEL_PREFIX, domain_tenant, path.join("/"))
            }
        };
        let Some((tenant, path)) = uri::parse_sel(&uri_str) else {
            return RegistrationAdmission::Refused;
        };

        // Canonical typed URI: identity-checked against the host authority.
        if let Some((typed_tenant, typed_class, typed_id)) = uri::parse_typed(&uri_str) {
            if typed_class != *class || typed_id != resource_id || typed_tenant != mint_tenant {
                return RegistrationAdmission::Refused;
            }
            return RegistrationAdmission::Admitted(uri_str);
        }

        let Some(first) = path.split('/').next() else {
            return RegistrationAdmission::Refused;
        };
        if first.is_empty() || uri::is_class_segment(first) {
            return RegistrationAdmission::Refused;
        }
        if first == "health" {
            // The health namespace is reserved: health records are published
            // through `SetHealth` under their owner's own name, so a guest
            // cannot shadow or forge another guest's health record.
            return RegistrationAdmission::Refused;
        }
        // Named records live in the tenant the target was minted under; a
        // caller cannot alias a resource into another tenant.
        if tenant != mint_tenant {
            return RegistrationAdmission::Refused;
        }
        if !tenant_admits(caller_tenant, Some(tenant)) {
            return RegistrationAdmission::Refused;
        }
        // Reserved root names are exclusive host-issued claims, including the
        // protocol-handler namespace.
        if tenant.is_empty() && !claim_held {
            return RegistrationAdmission::Refused;
        }
        if first == "handler" && !claim_held {
            return RegistrationAdmission::Refused;
        }
        RegistrationAdmission::Admitted(uri_str)
    }

    /// The internal key a raw registration form resolves to, without admission
    /// checks.
    ///
    /// Renewal keys off this so a lease taken out under a projected internal
    /// URI is renewed by the same raw form the owner registered with; the
    /// owner check in [`renew_lease`](Self::renew_lease) remains the gate.
    fn registration_key(&self, raw: &str) -> Option<String> {
        match uri::parse_sel(raw) {
            Some(_) => Some(raw.to_string()),
            None => {
                let (domain_tenant, path) = uri::resolve_wire_name(raw, &self.domains)?;
                if path.is_empty() {
                    return None;
                }
                Some(format!(
                    "{}{}/{}",
                    uri::SEL_PREFIX,
                    domain_tenant,
                    path.join("/")
                ))
            }
        }
    }

    /// The `(class, id)` a revocation names, if the registration exists.
    ///
    /// Aliases name their canonical target; named routes and typed records
    /// carry the target directly. Used only to scope the host authority read
    /// before the revocation is admitted.
    fn revocation_target(&self, internal: &str) -> Option<(selium_abi::ResourceClass, u64)> {
        if let Some(canonical) = self.aliases.get(internal) {
            let (_, class, id) = uri::parse_typed(canonical)?;
            return Some((class, id));
        }
        self.registrations
            .get(internal)
            .map(|target| (target.class.clone(), target.resource_id))
    }

    /// Renews a lease named by any registration form (an internal `sel://` URI
    /// or a wire name), keyed exactly as [`registration_uri`](Self::registration_uri)
    /// keyed the original registration.
    pub fn renew_lease_raw(&mut self, raw: &str, owner: ProcessId, now_ms: u64) -> bool {
        match self.registration_key(raw) {
            Some(key) => self.renew_lease(&key, owner, now_ms),
            None => false,
        }
    }

    /// Applies a guest registration with the authority-based validation chain:
    /// wire-name translation, typed-key derivation, tenant/class authority,
    /// claim gating, and exclusive live claims.
    pub fn apply_register(
        &mut self,
        caller: ProcessId,
        caller_tenant: Option<&str>,
        target: ResourceTarget,
        authority: &ResourceAuthority,
        claim_held: bool,
        root_service: bool,
    ) -> DiscoveryResponse {
        let uri_str = match self.classify_registration(
            &target.uri,
            caller_tenant,
            authority,
            claim_held,
            target.resource_id,
            &target.class,
        ) {
            RegistrationAdmission::Admitted(uri) => uri,
            // The caller owns the id under another class: a permission
            // failure, not a missing target.
            RegistrationAdmission::ClassMismatch | RegistrationAdmission::Refused => {
                return DiscoveryResponse::Forbidden;
            }
            // An unattested leaf alias names a target the authority does not
            // recognise: nothing is stored and resolution reports NotFound. A
            // typed or multi-segment form is an outright permission failure.
            RegistrationAdmission::Unattested => {
                let leaf_alias = uri::parse_sel(&target.uri)
                    .map(|(_, path)| !path.is_empty() && !path.contains('/'))
                    .unwrap_or_else(|| {
                        uri::resolve_wire_name(&target.uri, &self.domains)
                            .is_some_and(|(_, path)| path.len() == 1)
                    });
                return if leaf_alias {
                    DiscoveryResponse::NotFound
                } else {
                    DiscoveryResponse::Forbidden
                };
            }
        };
        let mint_tenant = authority.tenant.as_deref().unwrap_or("");
        let (tenant, path) = uri::parse_sel(&uri_str).expect("registration_uri is a sel uri");
        let is_typed = uri::parse_typed(&uri_str).is_some();

        // Exclusive live claims: one live registration per *claimed* well-known
        // name. The current owner may replace its own registration. Typed
        // records are identity-keyed rather than claimed names, so co-owners
        // publish the same canonical key without conflict; unclaimed named
        // records keep last-writer-wins.
        if claim_held
            && !is_typed
            && (self.registrations.contains_key(&uri_str) || self.aliases.contains_key(&uri_str))
            && self
                .route_owners
                .get(&uri_str)
                .is_some_and(|owner| *owner != caller)
        {
            return DiscoveryResponse::Conflict;
        }

        if !is_typed && !path.contains('/') {
            // A leaf alias resolves only through its canonical typed target,
            // which may not have been projected yet. Storing the mapping (and
            // not an exact registration) is what makes the transient
            // `NotFound` behaviour correct.
            let canonical =
                uri::resource_uri(mint_tenant, target.class.clone(), target.resource_id);
            // Retargeting an alias moves it: remove the back-reference from
            // its previous canonical target so revoking that target can no
            // longer revoke an alias now pointing elsewhere.
            if let Some(previous) = self.aliases.insert(uri_str.clone(), canonical.clone())
                && previous != canonical
                && let Some(backrefs) = self.alias_backrefs.get_mut(&previous)
            {
                backrefs.retain(|alias| alias != &uri_str);
                if backrefs.is_empty() {
                    self.alias_backrefs.remove(&previous);
                }
            }
            let backrefs = self.alias_backrefs.entry(canonical).or_default();
            if !backrefs.contains(&uri_str) {
                backrefs.push(uri_str.clone());
            }
            self.route_owners.insert(uri_str.clone(), caller);
            if root_service {
                self.root_services.insert(tenant.to_string(), uri_str);
            }
            return DiscoveryResponse::Registered;
        }

        // Named-service route (multi-segment) or canonical typed record: store
        // the caller's full target under the derived key.
        let mut target = target;
        target.uri = uri_str.clone();
        target.tenant = (!mint_tenant.is_empty()).then(|| mint_tenant.to_string());
        self.store(target, Some(caller));
        if root_service {
            self.root_services.insert(tenant.to_string(), uri_str);
        }
        DiscoveryResponse::Registered
    }

    /// Applies a guest revocation. Guests may revoke only the registrations
    /// they own, in the tenant the target was minted under; typed URIs are
    /// host-attested and revoked when the resource is freed or its owner
    /// exits. The target's ownership is confirmed against the host authority,
    /// never the projected store. Unknown keys report `NotFound`.
    pub fn apply_revoke(
        &mut self,
        caller: ProcessId,
        caller_tenant: Option<&str>,
        authority: &ResourceAuthority,
        uri: &str,
    ) -> DiscoveryResponse {
        // Translate an external wire name to the internal route it projects
        // to, so a guest can revoke by wire name.
        if uri::parse_sel(uri).is_none() {
            return match uri::resolve_wire_name(uri, &self.domains) {
                Some((tenant, path)) if !path.is_empty() => {
                    let internal = format!("{}{}/{}", uri::SEL_PREFIX, tenant, path.join("/"));
                    self.apply_revoke(caller, caller_tenant, authority, &internal)
                }
                _ => DiscoveryResponse::NotFound,
            };
        }

        if uri::parse_typed(uri).is_some() {
            // Typed URIs are host-attested; only `ResourceFreed` revokes them.
            return DiscoveryResponse::Forbidden;
        }
        if uri::is_root_uri(uri) {
            // Root names are host-issued claims, not guest-owned custom
            // registrations: only `ProcessGone` or lease expiry removes them.
            return DiscoveryResponse::Forbidden;
        }
        let Some((tenant, _path)) = uri::parse_sel(uri) else {
            return DiscoveryResponse::NotFound;
        };
        if !tenant_admits(caller_tenant, Some(tenant)) {
            return DiscoveryResponse::Forbidden;
        }
        let is_alias = self.aliases.contains_key(uri);
        if !is_alias && !self.registrations.contains_key(uri) {
            return DiscoveryResponse::NotFound;
        }
        let mint_tenant = authority.tenant.as_deref().unwrap_or("");
        if !authority.owned || tenant != mint_tenant {
            return DiscoveryResponse::Forbidden;
        }
        if is_alias {
            if self.route_owners.get(uri).copied() != Some(caller) {
                return DiscoveryResponse::Forbidden;
            }
            self.revoke_alias(uri);
            return DiscoveryResponse::Revoked;
        }
        let Some(target) = self.registrations.get(uri).cloned() else {
            return DiscoveryResponse::NotFound;
        };
        if self.route_owners.get(uri).copied() != Some(caller) {
            return DiscoveryResponse::Forbidden;
        }
        if self
            .registration_uri(
                uri,
                caller_tenant,
                authority,
                true,
                target.resource_id,
                &target.class,
            )
            .is_none()
        {
            return DiscoveryResponse::Forbidden;
        }
        self.revoke_key(uri);
        DiscoveryResponse::Revoked
    }

    /// Returns the expected claims that have no live registration.
    ///
    /// Expected claims are the host's record of what each process *should*
    /// publish; discovery is the projection that says what it *did* publish.
    /// A difference is drift, not an accounting fact: the process remains in
    /// the host's process table, ownership sets, metering, and quota.
    pub fn unpublished_claims(&self, expected: &[String]) -> Vec<String> {
        expected
            .iter()
            .filter(|uri| {
                !self.registrations.contains_key(*uri) && !self.aliases.contains_key(*uri)
            })
            .cloned()
            .collect()
    }

    /// Returns the provisioned domain→tenant entries for a `ListDomains`
    /// request.
    pub fn domain_entries(&self) -> Vec<DomainEntry> {
        self.domains
            .entries()
            .map(|(domain, tenant)| DomainEntry {
                domain: domain.to_string(),
                tenant: tenant.to_string(),
            })
            .collect()
    }

    pub fn ingest_interface_metadata(&mut self, uri: &str, metadata: InterfaceMetadata) -> bool {
        let Some(target) = self.registrations.get_mut(uri) else {
            return false;
        };
        target.interface = Some(metadata);
        true
    }
}

pub fn interface_metadata() -> InterfaceMetadata {
    discoverycontrol_pattern_metadata()
}

/// Maps an authority read to admission: a failed read fails closed with
/// `Forbidden`, never falling back to the projected store.
fn admission_authority(
    authority: selium_guest::Result<ResourceAuthority>,
) -> Option<ResourceAuthority> {
    authority.ok()
}

fn attach_feed_subscriber(
    feed_region_id: u64,
) -> selium_guest::Result<Subscriber<DiscoveryRequest, ShmTransport>> {
    let channel = Channel::attach(feed_region_id)
        .map_err(|error| selium_guest::GuestError::Host(error.to_string()))?;
    let transport = ShmTransport::new(&channel, &channel)
        .map_err(|error| selium_guest::GuestError::Host(error.to_string()))?;
    let framed = FramedRead::new(transport);
    // Disable overwrite detection: the discovery feed is volatile and the guest
    // reads whatever is currently available, accepting that events may be lost.
    Ok(Subscriber::new(framed, None))
}

/// Builds the route-change live table over its own ring, returning the ring id
/// and the table.
fn create_route_table() -> selium_wire::Result<(u64, RouteTable)> {
    let channel = Channel::create_with_backpressure(
        256 * 1024,
        ChannelBackpressure::Drop,
        ResourceKind::LiveTable,
    )?;
    let region_id = channel.region_id();
    let write = ShmTransport::new(&channel, &channel)?;
    let read = ShmTransport::new(&channel, &channel)?;
    let publisher = Publisher::new(FramedWrite::new(write));
    let subscriber = Subscriber::new(FramedRead::new(read), None);
    let authority = selium_wire::TableWriterAuthority::claim("discovery.routes")?;
    let table = LiveTable::new(authority, publisher, subscriber)?;
    Ok((region_id, table))
}

/// Publishes an owned listener/region directly into the registry. Discovery
/// cannot call its own RPC listener from its entrypoint without self-joining
/// its reactor, so it performs the same canonical-then-named admission steps
/// locally, still mediated by host ownership and the exact route claim.
fn publish_owned_route(
    store: &mut DiscoveryStore,
    class: ResourceClass,
    resource_id: u64,
    route: &str,
) -> Result<(), String> {
    let process_id = selium_guest::process_id();
    let authority = selium_guest::resource_authority(process_id, class.clone(), resource_id)
        .map_err(|error| format!("authority query failed for {route}: {error}"))?;
    if !authority.owned {
        return Err(format!(
            "host authority does not assign {class:?} {resource_id} to discovery"
        ));
    }
    let canonical = ResourceTarget {
        uri: uri::resource_uri(
            authority.tenant.as_deref().unwrap_or_default(),
            class.clone(),
            resource_id,
        ),
        host_id: String::new(),
        resource_id,
        interface: None,
        tenant: authority.tenant.clone(),
        class: class.clone(),
        labels: Vec::new(),
    };
    let response = store.apply_register(process_id, None, canonical, &authority, false, false);
    if !matches!(response, DiscoveryResponse::Registered) {
        return Err(format!("canonical publication refused: {response:?}"));
    }

    let claim_held = selium_guest::process_capability_for_uri(
        process_id,
        Capability::RouteClaim,
        Some(route.to_string()),
    )
    .map_err(|error| format!("route claim query failed for {route}: {error}"))?;
    let target = ResourceTarget {
        uri: route.to_string(),
        host_id: String::new(),
        resource_id,
        interface: None,
        tenant: None,
        class,
        labels: Vec::new(),
    };
    let response = store.apply_register(process_id, None, target, &authority, claim_held, false);
    if !matches!(response, DiscoveryResponse::Registered) {
        return Err(format!("named route publication refused: {response:?}"));
    }
    Ok(())
}

/// Response used when the caller's tenant scope could not be verified
/// (fail-closed): reads disclose nothing, writes are refused. A tenant
/// *absence* (verified `None`, i.e. a root/system principal) is legitimate
/// and scoped normally — only a failed lookup takes this path.
fn denied_response(request: &DiscoveryRequest) -> DiscoveryResponse {
    match request {
        DiscoveryRequest::Resolve(_) => DiscoveryResponse::NotFound,
        DiscoveryRequest::ResolvePrefix(_) | DiscoveryRequest::ResolveLabels { .. } => {
            DiscoveryResponse::Resolved(Vec::new())
        }
        DiscoveryRequest::ListDomains => DiscoveryResponse::Domains(Vec::new()),
        DiscoveryRequest::Register { .. }
        | DiscoveryRequest::Revoke { .. }
        | DiscoveryRequest::ProcessGone { .. }
        | DiscoveryRequest::ResourceFreed { .. }
        | DiscoveryRequest::Renew { .. }
        | DiscoveryRequest::SetHealth { .. }
        | DiscoveryRequest::SetDomain { .. } => DiscoveryResponse::Forbidden,
    }
}

#[entrypoint]
async fn discovery_main(feed_region_id: u64, listener_shared_id: u64) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    selium_guest::info!(guest = "selium-discovery", "system guest booting");

    let feed_subscriber = attach_feed_subscriber(feed_region_id)
        .with_context(|| "failed to attach discovery feed subscriber")?;

    let listener = selium_guest::ResourceListener::attach(listener_shared_id)
        .with_context(|| "failed to attach discovery listener")?;

    selium_guest::info!(
        feed_region_id,
        shared_id = listener.descriptor().shared_id,
        "discovery feed and listener attached"
    );

    let store = Arc::new(Mutex::new(DiscoveryStore::default()));

    // Publish the RPC listener itself. Replacement clients learn the new queue
    // from this route rather than retaining the bootstrap handle forever.
    selium_guest::declare_attachable_by_resolution(
        ResourceClass::HostQueue,
        listener_shared_id,
        AttachDeclarationScope::Any,
    )
    .with_context(|| "failed to declare the discovery listener attachable")?;
    {
        let mut store = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        publish_owned_route(
            &mut store,
            ResourceClass::HostQueue,
            listener_shared_id,
            selium_guest::DISCOVERY_ROUTE,
        )
        .map_err(|error| anyhow::anyhow!(error))?;
    }

    // The route-change table: consumers watch it to converge on providers.
    let (route_region, route_table) =
        create_route_table().with_context(|| "failed to create discovery route table")?;
    let route_table = Arc::new(route_table);
    {
        let process_id = selium_guest::process_id();
        let mut store = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let authority =
            selium_guest::resource_authority(process_id, ResourceClass::SharedRegion, route_region)
                .unwrap_or(selium_abi::ResourceAuthority {
                    owned: false,
                    tenant: None,
                    other_classes: Vec::new(),
                });
        // Publish the canonical typed record first, then the root route it
        // backs, exactly as `Context::serve` does for a guest route.
        let canonical = ResourceTarget {
            uri: uri::resource_uri(
                authority.tenant.as_deref().unwrap_or_default(),
                ResourceClass::SharedRegion,
                route_region,
            ),
            host_id: String::new(),
            resource_id: route_region,
            interface: None,
            tenant: authority.tenant.clone(),
            class: ResourceClass::SharedRegion,
            labels: Vec::new(),
        };
        if !matches!(
            store.apply_register(process_id, None, canonical, &authority, false, false),
            DiscoveryResponse::Registered
        ) {
            selium_guest::warn!("discovery route table canonical publication failed");
        }
        let route_target = ResourceTarget {
            uri: ROUTE_TABLE_ROUTE.to_string(),
            host_id: String::new(),
            resource_id: route_region,
            interface: None,
            tenant: None,
            class: ResourceClass::SharedRegion,
            labels: Vec::new(),
        };
        let claim_held = selium_guest::process_capability_for_uri(
            process_id,
            Capability::RouteClaim,
            Some(ROUTE_TABLE_ROUTE.to_string()),
        )
        .unwrap_or(false);
        if !matches!(
            store.apply_register(
                process_id,
                None,
                route_target,
                &authority,
                claim_held,
                false,
            ),
            DiscoveryResponse::Registered
        ) {
            selium_guest::warn!("discovery route table route publication failed");
        }
    }
    // Declare the table's region attachable so consumers can resolve and
    // attach it.
    selium_guest::declare_attachable_by_resolution(
        ResourceClass::SharedRegion,
        route_region,
        AttachDeclarationScope::Any,
    )
    .with_context(|| "failed to declare the route table attachable")?;
    {
        let store = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(error) = sync_route_table(&store, &route_table) {
            selium_guest::warn!("discovery route table initial sync failed: {error}");
        }
    }

    // Spawn the feed processing loop.
    selium_guest::spawn(feed_loop(
        store.clone(),
        route_table.clone(),
        feed_subscriber,
    ));
    // Spawn the lease sweep loop.
    selium_guest::spawn(lease_sweep_loop(store.clone(), route_table.clone()));

    // Discovery publishes no health record of its own: it is the registry, its
    // entrypoint takes raw infrastructure handles rather than a `Context`, and
    // publishing would mean an RPC from this guest to its own listener — a
    // self-join the SDK does not model (it deadlocks the guest's reactor
    // against its own accept loop). Health is published state and an absent
    // record is valid: peers observe discovery by the routes it serves.

    // Accept incoming RPC connections forever.
    loop {
        let incoming = match listener.recv().await {
            Ok(connection) => connection,
            Err(error) => {
                selium_guest::warn!("discovery accept failed: {error}");
                continue;
            }
        };

        let connection_region_id = incoming.shared_id;
        let connection =
            match selium_shm::rpc::accept::<DiscoveryRequest, DiscoveryResponse>(incoming.into()) {
                Ok(c) => c,
                Err(error) => {
                    selium_guest::warn!("discovery rpc accept failed: {error}");
                    continue;
                }
            };

        let store = store.clone();
        let route_table = route_table.clone();
        selium_guest::spawn(handler(
            store,
            route_table,
            connection_region_id,
            connection,
        ));
    }
}

async fn feed_loop(
    store: Arc<Mutex<DiscoveryStore>>,
    route_table: Arc<RouteTable>,
    mut subscriber: Subscriber<DiscoveryRequest, ShmTransport>,
) {
    loop {
        // Park on the feed rather than yielding. The feed's writer is the
        // *host* (it publishes Tier-1 lifecycle events), and a yield only
        // re-arms this task locally — it tells nobody that the feed has data.
        // Parking registers interest in the region with the host, so the host's
        // own publish wakes this task; without it a Tier-1 event sits unread
        // until something unrelated re-enters the guest.
        match subscriber.recv_with_tag().await {
            Ok((request, _tag)) => {
                let mut store = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                store.apply_tier1_event(request);
                if let Err(error) = sync_route_table(&store, &route_table) {
                    selium_guest::warn!("discovery route table sync failed: {error}");
                }
            }
            Err(error) => {
                selium_guest::warn!("discovery feed read failed: {error}");
                break;
            }
        }
    }
}

async fn handler(
    store: Arc<Mutex<DiscoveryStore>>,
    route_table: Arc<RouteTable>,
    connection_region_id: u64,
    mut conn: selium_shm::rpc::RpcConnection<DiscoveryRequest, DiscoveryResponse>,
) {
    let client_process_id = conn.client_process_id();
    // The caller's tenant is read from the runtime's persisted process
    // authority, scoping every operation to the calling process's own
    // tenant. Fail-closed: if the lookup itself fails, requests are denied
    // rather than silently treated as unscoped.
    let caller_scope = selium_guest::process_tenant(client_process_id);
    let scope_verified = caller_scope.is_ok();
    let caller_tenant = caller_scope.unwrap_or_default();
    if !scope_verified {
        selium_guest::warn!(
            "failed to resolve caller tenant for process {client_process_id}; denying requests"
        );
    }
    // A delivered rendezvous region transfers to discovery, so this handler
    // owns its client's connection region and releases it when the session
    // ends. (Releasing a departed client's region while the handler is parked
    // would unmap memory the parked read is using; that needs a parked-read
    // interrupt and is recorded as a known unknown.)
    loop {
        let request = match conn.recv().await {
            Ok(request) => request,
            Err(selium_shm::rpc::RpcError::ConnectionClosed) => break,
            Err(error) => {
                selium_guest::warn!("discovery recv failed: {error}");
                break;
            }
        };
        {
            let response = {
                let mut store = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let response = match request.payload() {
                    Ok(payload) if !scope_verified => denied_response(&payload),
                    Ok(payload) => match payload {
                        DiscoveryRequest::Resolve(uri) => {
                            match store.resolve_exact(&uri, caller_tenant.as_deref()) {
                                Some(target) => {
                                    // A resolved shared resource (region or
                                    // queue) gains an authorisation basis
                                    // for the resolving client, but only
                                    // when its owner declared it attachable
                                    // by resolution: the runtime enforces
                                    // the declaration's scope and refuses an
                                    // undeclared resource. No guest identity
                                    // is privileged for recording.
                                    if matches!(
                                        target.class,
                                        selium_abi::ResourceClass::SharedRegion
                                            | selium_abi::ResourceClass::HostQueue
                                    ) && let Err(error) =
                                        selium_guest::record_resolved_resource_for(
                                            target.class.clone(),
                                            target.resource_id,
                                            client_process_id,
                                        )
                                    {
                                        selium_guest::warn!(
                                            "resolve authorisation record failed: {error}"
                                        );
                                    }
                                    DiscoveryResponse::Found(target)
                                }
                                None => DiscoveryResponse::NotFound,
                            }
                        }
                        DiscoveryRequest::ResolvePrefix(prefix) => {
                            let targets = store.resolve_prefix(&prefix, caller_tenant.as_deref());
                            DiscoveryResponse::Resolved(targets)
                        }
                        DiscoveryRequest::ResolveLabels { key, value } => {
                            let targets =
                                store.resolve_labels(&key, &value, caller_tenant.as_deref());
                            DiscoveryResponse::Resolved(targets)
                        }
                        DiscoveryRequest::Register {
                            target,
                            root_service,
                            ..
                        } => {
                            // Ownership and the mint tenant are host facts.
                            // Admission asks the authority synchronously and
                            // never falls back to the projected store.
                            match admission_authority(selium_guest::resource_authority(
                                client_process_id,
                                target.class.clone(),
                                target.resource_id,
                            )) {
                                Some(authority) => {
                                    let tentative = store.registration_key(&target.uri);
                                    let claim_held = tentative.as_deref().is_some_and(|uri| {
                                        selium_guest::process_capability_for_uri(
                                            client_process_id,
                                            Capability::RouteClaim,
                                            Some(uri.to_string()),
                                        )
                                        .unwrap_or(false)
                                    });
                                    let response = store.apply_register(
                                        client_process_id,
                                        caller_tenant.as_deref(),
                                        target,
                                        &authority,
                                        claim_held,
                                        root_service,
                                    );
                                    if matches!(response, DiscoveryResponse::Registered)
                                        && let Some(uri) = tentative
                                    {
                                        if uri.contains("/handler/") {
                                            selium_guest::info!(
                                                "discovery: registered handler route {uri} for process {client_process_id}"
                                            );
                                        }
                                        let now_ms = selium_guest::time::now()
                                            .map(|nanos| nanos / 1_000_000)
                                            .unwrap_or(0);
                                        store.set_lease(&uri, client_process_id, now_ms);
                                    } else if let Some(uri) = &tentative
                                        && uri.contains("/handler/")
                                    {
                                        selium_guest::warn!(
                                            "discovery: handler route {uri} for process {client_process_id} refused: {response:?}"
                                        );
                                    }
                                    response
                                }
                                None => {
                                    // Fail closed: the projection never
                                    // substitutes for an authority answer.
                                    selium_guest::warn!(
                                        "discovery authority read failed for process {client_process_id}"
                                    );
                                    DiscoveryResponse::Forbidden
                                }
                            }
                        }
                        DiscoveryRequest::Renew { uri } => {
                            let now_ms = selium_guest::time::now()
                                .map(|nanos| nanos / 1_000_000)
                                .unwrap_or(0);
                            // Keyed exactly as the original registration was,
                            // so a wire-name registration renews the lease
                            // taken out under its projected internal URI.
                            if store.renew_lease_raw(&uri, client_process_id, now_ms) {
                                DiscoveryResponse::Registered
                            } else {
                                DiscoveryResponse::Forbidden
                            }
                        }
                        DiscoveryRequest::SetHealth { name, healthy } => {
                            store.set_health(
                                caller_tenant.as_deref(),
                                client_process_id,
                                &name,
                                healthy,
                            );
                            if healthy {
                                DiscoveryResponse::Registered
                            } else {
                                DiscoveryResponse::Revoked
                            }
                        }
                        DiscoveryRequest::Revoke { uri } => {
                            let internal =
                                store.registration_key(&uri).unwrap_or_else(|| uri.clone());
                            let authority = match store.revocation_target(&internal) {
                                Some((class, id)) => {
                                    selium_guest::resource_authority(client_process_id, class, id)
                                        .unwrap_or(selium_abi::ResourceAuthority {
                                            owned: false,
                                            tenant: None,
                                            other_classes: Vec::new(),
                                        })
                                }
                                None => selium_abi::ResourceAuthority {
                                    owned: false,
                                    tenant: None,
                                    other_classes: Vec::new(),
                                },
                            };
                            store.apply_revoke(
                                client_process_id,
                                caller_tenant.as_deref(),
                                &authority,
                                &uri,
                            )
                        }
                        DiscoveryRequest::ListDomains => {
                            DiscoveryResponse::Domains(store.domain_entries())
                        }
                        DiscoveryRequest::SetDomain { domain, tenant } => {
                            // The advisory domain mapping is authored by the
                            // bootstrap-provisioned `DomainWrite` authority
                            // (the identity guest), not by any guest identity.
                            let allowed = selium_guest::process_capability(
                                client_process_id,
                                Capability::DomainWrite,
                            )
                            .unwrap_or(false);
                            if allowed {
                                store.seed_domain(&domain, &tenant);
                                DiscoveryResponse::Registered
                            } else {
                                DiscoveryResponse::Forbidden
                            }
                        }
                        // Tier-1 revocation operations never arrive over
                        // RPC.
                        DiscoveryRequest::ProcessGone { .. }
                        | DiscoveryRequest::ResourceFreed { .. } => DiscoveryResponse::Forbidden,
                    },
                    Err(error) => {
                        selium_guest::warn!("discovery payload decode failed: {error}");
                        continue;
                    }
                };
                if let Err(error) = sync_route_table(&store, &route_table) {
                    selium_guest::warn!("discovery route table sync failed: {error}");
                }
                response
            };
            if let Err(error) = request.reply(response).await {
                selium_guest::warn!("discovery reply failed: {error}");
                break;
            }
        }
    }

    // The handler is done with the connection. Only a close confirmed by the
    // host-set peer-departed flag is final: a writer-count dip during setup
    // also surfaces as `ConnectionClosed` and must not free a live session.
    // With a confirmed close, drop the session, then release the region
    // discovery owns on the client's behalf.
    let peer_closed = conn.peer_closed_flag().unwrap_or(false);
    drop(conn);
    if peer_closed && let Err(error) = selium_shm::free_region(connection_region_id) {
        selium_guest::warn!(
            "discovery failed to release connection region {connection_region_id}: {error}"
        );
    }
}

/// The tenant-scoped health route for `name`.
fn health_uri(tenant: Option<&str>, name: &str) -> String {
    format!(
        "{}{}/health/{}",
        uri::SEL_PREFIX,
        tenant.unwrap_or_default(),
        name
    )
}

/// Returns whether a registration key is owner-keyed (removed when its owner
/// exits) rather than identity-keyed (removed only when the host frees the
/// resource).
///
/// Named routes, aliases, health records, and a process's own node are
/// owner-keyed. Every other typed record is identity-keyed, which is what
/// preserves a co-owned resource's record when one owner exits.
fn is_owner_keyed(key: &str) -> bool {
    match uri::parse_typed(key) {
        Some((_, ResourceClass::Process, _)) => true,
        Some(_) => false,
        None => true,
    }
}

/// Periodically expires lapsed guest-registration leases, revoking the routes
/// and publishing the resulting route changes.
async fn lease_sweep_loop(store: Arc<Mutex<DiscoveryStore>>, route_table: Arc<RouteTable>) {
    const SWEEP_INTERVAL_NANOS: u64 = 2_000_000_000;
    let mut ticks: u64 = 0;
    let mut reported_drift: BTreeMap<ProcessId, BTreeSet<String>> = BTreeMap::new();
    let mut missing_since: BTreeMap<ProcessId, u64> = BTreeMap::new();
    loop {
        // The sweep *interval* is measured on the monotonic clock, the same
        // domain `Timer` compares its deadline against. Wall-clock
        // nanoseconds since the epoch (`selium_guest::time::now`) are a
        // different domain entirely — feeding one into `Instant::from_nanos`
        // puts the deadline ~56 years in the future and the sweep never ticks.
        // Wall-clock time is still the right domain for *lease expiry*, below:
        // a lease is stored against the clock every participant reads.
        let deadline = match selium_guest::Instant::now() {
            Ok(now) => now
                .checked_add(std::time::Duration::from_nanos(SWEEP_INTERVAL_NANOS))
                .unwrap_or(selium_guest::Instant::MAX),
            Err(error) => {
                selium_guest::warn!("discovery lease sweep clock failed: {error}");
                selium_guest::yield_now().await;
                continue;
            }
        };
        if let Err(error) = selium_guest::Timer::new(deadline).await {
            selium_guest::warn!("discovery lease sweep sleep failed: {error}");
            continue;
        }

        let now_ms = selium_guest::time::now()
            .map(|nanos| nanos / 1_000_000)
            .unwrap_or(0);
        // A first-tick marker: the sweep runs on a timer, so "it fired" is
        // otherwise unobservable from outside, and an integration test cannot
        // tell a ticking sweep from one parked on a deadline that never
        // arrives. Marking only the *first* tick keeps the probe free — a
        // 0.5 Hz heartbeat writes a ring frame and wakes the host on every
        // beat, which is enough to perturb the spines sharing this host.
        if ticks == 0 {
            selium_guest::info!("discovery lease sweep tick");
        }
        ticks += 1;
        // Reconcile each process's expected claims against what it actually
        // published. A claim with no live registration is drift; it is logged
        // (once per newly observed gap) rather than treated as an accounting
        // fact, and discovery remains a projection, never the authority.
        reconcile_claim_drift(&store, &mut reported_drift, &mut missing_since, ticks);
        let mut store = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store.expire_leases(now_ms);
        if let Err(error) = sync_route_table(&store, &route_table) {
            selium_guest::warn!("discovery route table sync failed: {error}");
        }
    }
}

/// Reports expected claims that have no live registration.
///
/// The host is the authority for what a process *should* publish (its
/// descriptor grants); discovery only knows what it *did* publish. A gap that
/// persists past the boot grace window is drift: the process stays accounted
/// for by the host, and each newly observed gap is logged so an operator can
/// act on it without discovery pretending to be the accounting authority.
fn reconcile_claim_drift(
    store: &Arc<Mutex<DiscoveryStore>>,
    reported: &mut BTreeMap<ProcessId, BTreeSet<String>>,
    missing_since: &mut BTreeMap<ProcessId, u64>,
    tick: u64,
) {
    // A claim is routinely unpublished for the first moments of a process's
    // life (owner publication is not a feed barrier). Only a gap that persists
    // well past boot is reported as drift.
    const DRIFT_GRACE_TICKS: u64 = 15;

    let Ok(processes) = selium_guest::process_list() else {
        return;
    };
    for &process_id in &processes {
        let expected = match selium_guest::expected_claims(process_id) {
            Ok(expected) => expected,
            // The process disappeared between the list and this read; a dead
            // process is not drift.
            Err(_) => continue,
        };
        if expected.is_empty() {
            reported.remove(&process_id);
            missing_since.remove(&process_id);
            continue;
        }
        let missing: BTreeSet<String> = {
            let store = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            store.unpublished_claims(&expected).into_iter().collect()
        };
        if missing.is_empty() {
            reported.remove(&process_id);
            missing_since.remove(&process_id);
            continue;
        }
        let since = *missing_since.entry(process_id).or_insert(tick);
        if tick.saturating_sub(since) < DRIFT_GRACE_TICKS {
            continue;
        }
        {
            let already = reported.entry(process_id).or_default();
            for uri in missing.difference(already) {
                selium_guest::warn!(
                    "discovery claim drift: process {process_id} holds a claim for {uri} with no live registration"
                );
            }
        }
        reported.insert(process_id, missing);
    }
    reported.retain(|process_id, _| processes.contains(process_id));
    missing_since.retain(|process_id, _| processes.contains(process_id));
}

/// Projects the store's registrations onto the route table, publishing only
/// real changes: a set for every new or changed route, a delete for every
/// removed route. The table's per-entry version is the route's generation.
fn sync_route_table(store: &DiscoveryStore, table: &RouteTable) -> selium_wire::Result<()> {
    let mut current: std::collections::HashMap<String, ResourceTarget> = store
        .registrations
        .iter()
        .map(|(uri, target)| (uri.clone(), target.clone()))
        .collect();
    // An alias is only resolvable while its canonical target is projected, so
    // it enters the route table at that point (and leaves when the target is
    // revoked).
    for (alias, canonical) in &store.aliases {
        if let Some(target) = store.registrations.get(canonical) {
            current.insert(alias.clone(), target.clone());
        }
    }
    for (uri, target) in &current {
        if table.get(uri)?.as_ref() != Some(target) {
            table.set(uri.clone(), target.clone())?;
        }
    }
    for (uri, record) in table.scan(usize::MAX)? {
        if record.value.is_some() && !current.contains_key(uri.as_str()) {
            table.delete(uri)?;
        }
    }
    Ok(())
}

/// Returns whether a caller's tenant admits a target's tenant. The check is
/// skipped when either side is absent (backward-compatible with root/system
/// registrations and untracked tenants).
fn tenant_admits(caller: Option<&str>, target: Option<&str>) -> bool {
    match (caller, target) {
        (Some(caller), Some(target)) => caller == target,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_service::Label;

    fn target(
        uri: &str,
        resource_id: u64,
        tenant: Option<&str>,
        class: selium_abi::ResourceClass,
    ) -> ResourceTarget {
        ResourceTarget {
            uri: uri.to_string(),
            host_id: "host-a".to_string(),
            resource_id,
            interface: None,
            tenant: tenant.map(str::to_string),
            class,
            labels: Vec::new(),
        }
    }

    fn region(tenant: &str, id: u64) -> ResourceTarget {
        target(
            &uri::resource_uri(tenant, selium_abi::ResourceClass::SharedRegion, id),
            id,
            (!tenant.is_empty()).then_some(tenant),
            selium_abi::ResourceClass::SharedRegion,
        )
    }

    /// A host-authority answer for a target the caller owns under `tenant`.
    fn authority_for(tenant: &str) -> ResourceAuthority {
        ResourceAuthority {
            owned: true,
            tenant: Some(tenant.to_string()),
            other_classes: Vec::new(),
        }
    }

    /// Admits a registration against a permissive authority derived from the
    /// target's own tenant: convenience for tests that are not exercising the
    /// authority answer itself.
    fn admit(
        store: &mut DiscoveryStore,
        caller: ProcessId,
        caller_tenant: Option<&str>,
        target: ResourceTarget,
        claim_held: bool,
        root_service: bool,
    ) -> DiscoveryResponse {
        let mint = target.tenant.clone().unwrap_or_default();
        store.apply_register(
            caller,
            caller_tenant,
            target,
            &authority_for(&mint),
            claim_held,
            root_service,
        )
    }

    /// Revokes as the owner of the registration's target, with the authority
    /// answer derived from the store's recorded target.
    fn revoke(
        store: &mut DiscoveryStore,
        caller: ProcessId,
        caller_tenant: Option<&str>,
        uri: &str,
    ) -> DiscoveryResponse {
        let internal = store
            .registration_key(uri)
            .unwrap_or_else(|| uri.to_string());
        let authority = match store.revocation_target(&internal) {
            Some((_, id)) => {
                let tenant = store
                    .registrations
                    .values()
                    .find(|target| target.resource_id == id)
                    .and_then(|target| target.tenant.clone())
                    .unwrap_or_default();
                authority_for(&tenant)
            }
            None => ResourceAuthority {
                owned: false,
                tenant: None,
                other_classes: Vec::new(),
            },
        };
        store.apply_revoke(caller, caller_tenant, &authority, uri)
    }

    fn queue(tenant: &str, id: u64) -> ResourceTarget {
        target(
            &uri::resource_uri(tenant, selium_abi::ResourceClass::HostQueue, id),
            id,
            (!tenant.is_empty()).then_some(tenant),
            selium_abi::ResourceClass::HostQueue,
        )
    }

    fn process(tenant: &str, id: u64, labels: Vec<(&str, &str)>) -> ResourceTarget {
        let mut t = target(
            &uri::resource_uri(tenant, selium_abi::ResourceClass::Process, id),
            id,
            (!tenant.is_empty()).then_some(tenant),
            selium_abi::ResourceClass::Process,
        );
        t.labels = labels
            .into_iter()
            .map(|(k, v)| Label {
                key: k.to_string(),
                value: v.to_string(),
            })
            .collect();
        t
    }

    // scenario: discovery-registration/deterministic-tenant-scoped-uri-schema/internal-address-is-typed-and-tenant-scoped
    // scenario: discovery-registration/tenant-scoped-process-uri-resolution/cross-tenant-guest-cannot-resolve-process-uri
    #[test]
    fn typed_uri_resolves_within_tenant() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        assert_eq!(
            store.resolve_exact("sel://acme/region/7", Some("acme")),
            Some(region("acme", 7))
        );
        assert_eq!(
            store.resolve_exact("sel://acme/region/7", Some("beta")),
            None
        );
        assert_eq!(
            store.resolve_exact("sel://acme/region/8", Some("acme")),
            None
        );
    }

    #[test]
    fn root_uri_resolves_for_system_callers() {
        let mut store = DiscoveryStore::default();
        let dnchecked = target(
            "sel:///dns/resolve",
            12,
            None,
            selium_abi::ResourceClass::HostQueue,
        );
        store.store(dnchecked, None);

        assert!(store.resolve_exact("sel:///dns/resolve", None).is_some());
        assert!(
            store
                .resolve_exact("sel:///dns/resolve", Some("acme"))
                .is_some()
        );
    }

    // scenario: discovery-registration/leaf-aliases/alias-resolves-to-a-typed-target
    // scenario: guest-context/serve-based-route-registration/guest-registers-its-own-route
    // scenario: discovery-registration/serve-based-route-registration/guest-registers-its-own-route
    #[test]
    fn single_segment_route_resolves_to_the_serving_target() {
        let mut store = DiscoveryStore::default();
        store.store(process("acme", 123, vec![]), Some(123));
        let route = target(
            "sel://acme/proxy",
            123,
            Some("acme"),
            selium_abi::ResourceClass::Process,
        );
        admit(&mut store, 123, Some("acme"), route.clone(), false, false);

        // A single-segment route is a leaf alias: it resolves only through the
        // canonical typed target it names, so resolution returns that target.
        assert_eq!(
            store.resolve_exact("sel://acme/proxy", Some("acme")),
            Some(process("acme", 123, vec![]))
        );
    }

    // scenario: addressing/single-serve-declaration/resolution-returns-the-registered-route-target
    // scenario: discovery-registration/serve-based-route-registration/canonical-record-is-published-first
    #[test]
    fn single_segment_route_preserves_interface_metadata() {
        // A single-segment serve (e.g. `HttpServeStream::bind(ctx, "api")`)
        // carries an interface marker on its target. `serve` publishes that
        // same target as the canonical typed record before the alias, so the
        // marker survives resolution through the canonical target.
        let mut store = DiscoveryStore::default();
        let mut canonical = queue("acme", 7);
        canonical.interface = Some(InterfaceMetadata {
            name: "selium.http/stream".to_string(),
            methods: Vec::new(),
        });
        store.store(canonical.clone(), Some(42));
        let route = target(
            "sel://acme/api",
            7,
            Some("acme"),
            selium_abi::ResourceClass::HostQueue,
        );
        admit(&mut store, 42, Some("acme"), route, false, false);

        let resolved = store
            .resolve_exact("api.acme", Some("acme"))
            .expect("route resolves");
        assert_eq!(
            resolved.interface.expect("interface marker survives"),
            canonical.interface.expect("canonical marker")
        );
    }

    // scenario: discovery-registration/leaf-aliases/revoking-a-target-revokes-its-aliases
    #[test]
    fn revoking_a_target_revokes_its_aliases() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 456), Some(42));
        admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel://acme/proxy",
                456,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            false,
            false,
        );

        store.revoke_key("sel://acme/region/456");
        assert!(
            store
                .resolve_exact("sel://acme/region/456", Some("acme"))
                .is_none()
        );
        assert!(
            store
                .resolve_exact("sel://acme/proxy", Some("acme"))
                .is_none()
        );
    }

    // scenario: discovery-registration/root-tenant-namespace/guest-cannot-register-the-root-namespace
    #[test]
    fn guest_cannot_register_in_root_namespace() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        let response = admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel:///discovery",
                7,
                None,
                selium_abi::ResourceClass::SharedRegion,
            ),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert!(
            store
                .resolve_exact("sel:///discovery", Some("acme"))
                .is_none()
        );
    }

    #[test]
    fn guest_cannot_mint_typed_uris() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        let response = admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel://acme/region/999",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
    }

    // scenario: discovery-bootstrap/tier-2-guest-registration/unowned-resource-registration-is-forbidden
    // scenario: discovery-registration/guest-custom-uri-validation/guest-registration-rejected-for-unowned-resource
    #[test]
    fn guest_registration_requires_ownership() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        // Process 99 does not own resource 7: the multi-segment named
        // registration is refused, and nothing is stored.
        let response = store.apply_register(
            99,
            Some("acme"),
            target(
                "sel://acme/bridge/x",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            &ResourceAuthority {
                owned: false,
                tenant: None,
                other_classes: Vec::new(),
            },
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert!(
            store
                .resolve_exact("sel://acme/bridge/x", Some("acme"))
                .is_none()
        );
    }

    #[test]
    fn guest_alias_must_live_under_own_tenant() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        let response = admit(
            &mut store,
            42,
            Some("beta"),
            target(
                "sel://acme/proxy",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
    }

    #[test]
    fn alias_cannot_shadow_a_class_noun() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        // `region` is a reserved class noun; parse_alias rejects it, so it is
        // treated as a typed URI attempt and refused.
        let response = admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel://acme/region",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
    }

    // scenario: discovery-registration/replacement-maintains-secondary-indexes/labels-are-removed-on-retargeting
    #[test]
    fn retargeting_a_label_removes_the_old_index_entry() {
        let mut store = DiscoveryStore::default();
        store.store(process("acme", 123, vec![("app", "old")]), Some(123));
        assert_eq!(store.resolve_labels("app", "old", Some("acme")).len(), 1);

        store.store(process("acme", 123, vec![("app", "new")]), Some(123));
        assert!(
            store.resolve_labels("app", "old", Some("acme")).is_empty(),
            "a replaced label must disappear from the reverse index"
        );
        assert_eq!(store.resolve_labels("app", "new", Some("acme")).len(), 1);
    }

    // scenario: discovery-registration/replacement-maintains-secondary-indexes/replacement-failure-leaves-prior-indexes-coherent
    #[test]
    fn a_rejected_replacement_leaves_the_previous_registration_and_indexes_intact() {
        let mut store = DiscoveryStore::default();
        let original = process("acme", 123, vec![("app", "old")]);
        let original_uri = original.uri.clone();
        store.store(original, Some(123));
        assert_eq!(store.resolve_labels("app", "old", Some("acme")).len(), 1);

        // A caller the host does not authorise for the resource attempts to
        // replace the same URI with different labels.
        let response = store.apply_register(
            999,
            Some("acme"),
            process("acme", 123, vec![("app", "new")]),
            &ResourceAuthority {
                owned: false,
                tenant: None,
                other_classes: Vec::new(),
            },
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));

        // Neither half applied: the primary mapping and every secondary index
        // still describe the old registration.
        assert_eq!(store.resolve_labels("app", "old", Some("acme")).len(), 1);
        assert!(
            store.resolve_labels("app", "new", Some("acme")).is_empty(),
            "a rejected replacement must not index its new labels"
        );
        assert!(store.resolve_exact(&original_uri, Some("acme")).is_some());
    }

    // scenario: discovery-registration/discovery-uri-registration/register-overwrites-existing-uri
    // scenario: discovery-registration/replacement-maintains-secondary-indexes/alias-back-reference-follows-replacement
    #[test]
    fn alias_backreference_follows_retargeting() {
        let mut store = DiscoveryStore::default();
        let first = uri::resource_uri("acme", selium_abi::ResourceClass::SharedRegion, 7);
        let second = uri::resource_uri("acme", selium_abi::ResourceClass::SharedRegion, 8);
        store.store(region("acme", 7), Some(42));
        store.store(region("acme", 8), Some(42));

        for id in [7u64, 8u64] {
            let response = admit(
                &mut store,
                42,
                Some("acme"),
                target(
                    "sel://acme/alias",
                    id,
                    Some("acme"),
                    selium_abi::ResourceClass::SharedRegion,
                ),
                false,
                false,
            );
            assert!(matches!(response, DiscoveryResponse::Registered));
        }

        // The alias now points at resource 8; revoking 7 must not remove it.
        store.revoke_key(&first);
        assert!(
            store.resolve_exact("alias.acme", Some("acme")).is_some(),
            "revoking the previous target must not revoke a retargeted alias"
        );
        store.revoke_key(&second);
        assert!(
            store.resolve_exact("alias.acme", Some("acme")).is_none(),
            "revoking the current target revokes the alias"
        );
    }

    // scenario: discovery-registration/classification-labels/label-query-returns-matching-targets
    #[test]
    fn label_query_returns_matching_targets_only() {
        let mut store = DiscoveryStore::default();
        store.store(process("acme", 123, vec![("app", "web")]), Some(123));
        store.store(process("acme", 124, vec![("app", "web")]), Some(124));
        store.store(process("acme", 125, vec![("app", "worker")]), Some(125));

        let matches = store.resolve_labels("app", "web", Some("acme"));
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().all(|t| {
            t.labels.contains(&Label {
                key: "app".to_string(),
                value: "web".to_string(),
            })
        }));
    }

    #[test]
    fn label_query_is_tenant_scoped() {
        let mut store = DiscoveryStore::default();
        store.store(process("acme", 123, vec![("app", "web")]), Some(123));
        store.store(process("beta", 124, vec![("app", "web")]), Some(124));

        assert_eq!(store.resolve_labels("app", "web", Some("acme")).len(), 1);
        assert_eq!(store.resolve_labels("app", "web", Some("beta")).len(), 1);
    }

    // scenario: discovery-registration/prefix-and-enumeration-queries/prefix-lists-a-tenant-s-resources
    // scenario: discovery-registration/prefix-and-enumeration-queries/cross-tenant-enumeration-denied
    #[test]
    fn prefix_enumeration_lists_a_tenants_resources() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 1), Some(42));
        store.store(region("acme", 2), Some(42));
        store.store(region("beta", 3), Some(43));

        let results = store.resolve_prefix("sel://acme/region/*", Some("acme"));
        assert_eq!(results.len(), 2);

        // Cross-tenant enumeration is denied.
        assert!(
            store
                .resolve_prefix("sel://acme/region/*", Some("beta"))
                .is_empty()
        );
    }

    #[test]
    fn prefix_enumeration_with_bare_star() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 1), Some(42));
        store.store(process("acme", 123, vec![]), Some(123));

        let results = store.resolve_prefix("sel://acme/*", Some("acme"));
        assert_eq!(results.len(), 2);
    }

    // scenario: discovery-registration/external-name-resolution-through-addressing/wire-name-resolves-to-a-tenant-route
    // scenario: addressing/path-label-reversal/bridge-route-projects-to-a-wire-name
    #[test]
    fn wire_name_resolves_through_addressing() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        // A named-service route: the tenant's queue under the path `bridge`.
        let bridge = target(
            "sel://acme/bridge",
            7,
            Some("acme"),
            selium_abi::ResourceClass::HostQueue,
        );
        assert!(matches!(
            admit(&mut store, 42, Some("acme"), bridge.clone(), false, false),
            DiscoveryResponse::Registered
        ));

        // The synthetic label resolves without any table entry, mapping
        // `bridge.acme` onto the internal `sel://acme/bridge` alias, which
        // resolves through its canonical typed target.
        assert_eq!(
            store.resolve_exact("bridge.acme", Some("acme")),
            store.resolve_exact("sel://acme/bridge", Some("acme"))
        );
        assert_eq!(
            store.resolve_exact("bridge.acme", Some("acme")),
            Some(queue("acme", 7))
        );
        // Unknown traffic does not contact the synthetic namespace.
        assert_eq!(store.resolve_exact("bridge.beta", Some("acme")), None);
    }

    // scenario: discovery-registration/external-name-resolution-through-addressing/registered-domain-wire-name-resolves
    // scenario: discovery-registration/advisory-domain-scoping/tenant-registers-within-its-domain
    // scenario: discovery-registration/serve-based-route-registration/route-registration-derives-wire-name-from-the-domain-table
    // scenario: guest-context/serve-based-route-registration/route-registration-derives-wire-name-from-the-domain-table
    // scenario: addressing/advisory-domain-to-tenant-table/provisioned-domain-routes-to-its-tenant
    #[test]
    fn registered_domain_wire_name_resolves() {
        let mut store = DiscoveryStore::default();
        store.seed_domain("example.com", "acme");
        store.store(queue("acme", 7), Some(42));
        let bridge = target(
            "sel://acme/bridge",
            7,
            Some("acme"),
            selium_abi::ResourceClass::HostQueue,
        );
        admit(&mut store, 42, Some("acme"), bridge.clone(), false, false);

        // `example.com -> acme` is provisioned, so `bridge.example.com`
        // reverses into path `["bridge"]` within tenant `acme`, resolving
        // through the canonical typed target.
        assert_eq!(
            store.resolve_exact("bridge.example.com", Some("acme")),
            Some(queue("acme", 7))
        );
    }

    #[test]
    fn domain_table_covers_unknown_and_provisioned_cases() {
        let mut store = DiscoveryStore::default();
        assert_eq!(store.domain_tenant("example.com"), None);

        store.seed_domain("example.com", "acme");
        assert_eq!(store.domain_tenant("example.com"), Some("acme"));
        assert_eq!(store.domain_tenant("example.org"), None);
    }

    #[test]
    fn tier1_process_gone_revokes_guest_routes() {
        // A process exit arrives as a Tier-1 ProcessGone event; the store
        // revokes every owner-keyed record, with no runtime side map.
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel://acme/bridge",
                7,
                Some("acme"),
                selium_abi::ResourceClass::HostQueue,
            ),
            false,
            false,
        );
        assert!(store.resolve_exact("bridge.acme", Some("acme")).is_some());

        store.apply_tier1_event(DiscoveryRequest::ProcessGone { process_id: 42 });
        assert_eq!(store.resolve_exact("bridge.acme", Some("acme")), None);
    }

    #[test]
    fn tier1_resource_freed_revokes_by_identity() {
        // A resource free arrives as a Tier-1 ResourceFreed event; the store
        // revokes the registration recorded for that resource identity.
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        store.store(queue("beta", 9), Some(99));
        assert!(
            store
                .resolve_exact("sel://acme/queue/7", Some("acme"))
                .is_some()
        );
        assert!(
            store
                .resolve_exact("sel://beta/queue/9", Some("beta"))
                .is_some()
        );

        store.apply_tier1_event(DiscoveryRequest::ResourceFreed {
            class: "queue".to_string(),
            id: 7,
        });
        assert_eq!(
            store.resolve_exact("sel://acme/queue/7", Some("acme")),
            None
        );
        assert!(
            store
                .resolve_exact("sel://beta/queue/9", Some("beta"))
                .is_some(),
            "only the named resource identity is revoked"
        );
    }

    // scenario: discovery-registration/advisory-domain-scoping/foreign-domain-registration-refused
    #[test]
    fn foreign_domain_registration_is_refused() {
        let mut store = DiscoveryStore::default();
        store.seed_domain("example.com", "acme");
        store.store(queue("beta", 7), Some(99));

        // Tenant `beta` attempts to register a name under `example.com`, a
        // domain owned by `acme`: the derived tenant does not admit the
        // caller, so the registration is refused.
        let response = admit(
            &mut store,
            99,
            Some("beta"),
            target(
                "bridge.example.com",
                7,
                Some("beta"),
                selium_abi::ResourceClass::HostQueue,
            ),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert_eq!(
            store.resolve_exact("bridge.example.com", Some("acme")),
            None
        );
    }

    /// Admission succeeds from the host authority even when the projected
    /// store has never seen the resource: the canonical typed URI is a derived
    /// key, not a row that must have arrived.
    // scenario: discovery-registration/host-authoritative-admission/admission-succeeds-before-the-feed-catches-up
    // scenario: guest-context/canonical-typed-publication/owner-publishes-a-canonical-record
    #[test]
    fn admission_succeeds_before_the_projection_catches_up() {
        let mut store = DiscoveryStore::default();
        let response = admit(
            &mut store,
            42,
            Some("acme"),
            region("acme", 7),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Registered));
        assert_eq!(
            store.resolve_exact("sel://acme/region/7", Some("acme")),
            Some(region("acme", 7))
        );
    }

    /// A projected record the authority does not attest cannot authorise a
    /// registration by itself.
    // scenario: discovery-registration/host-authoritative-admission/projection-cannot-authorise
    #[test]
    fn the_projection_cannot_authorise_a_registration() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));
        let response = store.apply_register(
            99,
            Some("acme"),
            target(
                "sel://acme/bridge/x",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            &ResourceAuthority {
                owned: false,
                tenant: None,
                other_classes: Vec::new(),
            },
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert_eq!(
            store.resolve_exact("sel://acme/bridge/x", Some("acme")),
            None
        );
    }

    // scenario: guest-context/canonical-typed-publication/cross-tenant-canonical-publication-refused
    #[test]
    fn cross_tenant_canonical_publication_is_refused() {
        let mut store = DiscoveryStore::default();
        // Region 7 was minted under tenant "beta"; the caller claims "acme".
        let response = store.apply_register(
            42,
            Some("acme"),
            region("acme", 7),
            &authority_for("beta"),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
    }

    #[test]
    fn an_unowned_named_registration_is_refused() {
        let mut store = DiscoveryStore::default();
        let response = store.apply_register(
            42,
            Some("acme"),
            target(
                "sel://acme/bridge/x",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            &ResourceAuthority {
                owned: false,
                tenant: None,
                other_classes: Vec::new(),
            },
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
    }

    // scenario: discovery-registration/discovery-uri-registration/caller-revokes-a-uri
    // scenario: discovery-registration/guest-custom-uri-validation/guest-revokes-their-own-custom-uri
    // scenario: discovery-registration/guest-revocation-authorisation/guest-revokes-own-external-name
    #[test]
    fn guest_revokes_their_own_custom_uri() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        let route = target(
            "sel://acme/bridge",
            7,
            Some("acme"),
            selium_abi::ResourceClass::HostQueue,
        );
        admit(&mut store, 42, Some("acme"), route, false, false);

        // Revocation accepts the wire name and translates it to the internal
        // route it projects onto.
        let response = revoke(&mut store, 42, Some("acme"), "bridge.acme");
        assert!(matches!(response, DiscoveryResponse::Revoked));
        assert_eq!(store.resolve_exact("bridge.acme", Some("acme")), None);
    }

    #[test]
    fn guest_cannot_revoke_root_namespace() {
        let mut store = DiscoveryStore::default();
        let response = revoke(&mut store, 42, Some("acme"), "sel:///dns/resolve");
        assert!(matches!(response, DiscoveryResponse::Forbidden));
    }

    // scenario: discovery-registration/guest-revocation-authorisation/guest-cannot-revoke-a-typed-uri
    #[test]
    fn guest_cannot_revoke_typed_uris() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        // Typed URIs are runtime-minted; only the Tier-1 feed revokes them.
        let response = revoke(&mut store, 42, Some("acme"), "sel://acme/region/7");
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert!(
            store
                .resolve_exact("sel://acme/region/7", Some("acme"))
                .is_some()
        );
    }

    // scenario: discovery-registration/guest-revocation-authorisation/guest-cannot-revoke-another-tenant-s-alias
    #[test]
    fn guest_cannot_revoke_another_tenants_alias() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));
        let registered = admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel://acme/proxy",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            false,
            false,
        );
        assert!(matches!(registered, DiscoveryResponse::Registered));

        // A caller whose verified tenant is not the alias's tenant is denied.
        let response = revoke(&mut store, 42, Some("beta"), "sel://acme/proxy");
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert!(
            store
                .resolve_exact("sel://acme/proxy", Some("acme"))
                .is_some()
        );
    }

    // scenario: discovery-registration/guest-revocation-authorisation/guest-cannot-revoke-a-registration-it-does-not-own
    #[test]
    fn guest_cannot_revoke_an_alias_it_does_not_own() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));
        admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel://acme/proxy",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            false,
            false,
        );

        // Process 99 does not own resource 7, so it may not revoke the alias.
        let response = revoke(&mut store, 99, Some("acme"), "sel://acme/proxy");
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert!(
            store
                .resolve_exact("sel://acme/proxy", Some("acme"))
                .is_some()
        );
    }

    #[test]
    fn guest_cannot_revoke_a_route_it_does_not_own() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        let bridge = target(
            "sel://acme/bridge",
            7,
            Some("acme"),
            selium_abi::ResourceClass::HostQueue,
        );
        admit(&mut store, 42, Some("acme"), bridge.clone(), false, false);

        let response = revoke(&mut store, 99, Some("acme"), "bridge.acme");
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert_eq!(
            store.resolve_exact("bridge.acme", Some("acme")),
            Some(queue("acme", 7))
        );
    }

    // scenario: discovery-registration/guest-revocation-authorisation/revoking-an-unknown-key-returns-notfound
    #[test]
    fn revoking_an_unknown_uri_returns_not_found() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        let response = revoke(&mut store, 42, Some("acme"), "sel://acme/nowhere");
        assert!(matches!(response, DiscoveryResponse::NotFound));
    }

    // scenario: discovery-registration/guest-custom-uri-validation/guest-registration-rejected-for-class-mismatch
    #[test]
    fn guest_alias_class_must_match_the_owned_resource() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));

        // Process 42 owns resource 7 as a SharedRegion; a canonical URI that
        // classifies it as a Process node must not pass, even for the owner.
        let response = store.apply_register(
            42,
            Some("acme"),
            target(
                "sel://acme/region/7",
                7,
                Some("acme"),
                selium_abi::ResourceClass::Process,
            ),
            &authority_for("acme"),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
    }

    #[test]
    fn guest_alias_cannot_cross_the_mint_tenant() {
        let mut store = DiscoveryStore::default();
        // A delegated allocation: the runtime minted region 7 for tenant
        // "beta" on behalf of process 42, which owns it.
        store.store(region("beta", 7), Some(42));

        // An alias under "acme" claims an authority answer minted under
        // "beta": the tenant mismatch is refused.
        let response = store.apply_register(
            42,
            Some("acme"),
            target(
                "sel://acme/proxy",
                7,
                Some("acme"),
                selium_abi::ResourceClass::SharedRegion,
            ),
            &authority_for("beta"),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert_eq!(store.resolve_exact("sel://acme/proxy", Some("acme")), None);
    }

    // scenario: discovery-registration/tenant-scoped-process-uri-resolution/failed-tenant-lookup-fails-closed
    #[test]
    fn denied_response_fails_closed_per_variant() {
        use selium_service::ResourceTarget;

        let resolve = DiscoveryRequest::Resolve("sel://acme/region/7".to_string());
        assert!(matches!(
            denied_response(&resolve),
            DiscoveryResponse::NotFound
        ));

        let prefix = DiscoveryRequest::ResolvePrefix("sel://acme/region/*".to_string());
        assert!(matches!(
            denied_response(&prefix),
            DiscoveryResponse::Resolved(ref targets) if targets.is_empty()
        ));

        let labels = DiscoveryRequest::ResolveLabels {
            key: "app".to_string(),
            value: "web".to_string(),
        };
        assert!(matches!(
            denied_response(&labels),
            DiscoveryResponse::Resolved(ref targets) if targets.is_empty()
        ));

        let register = DiscoveryRequest::Register {
            uri: "sel://acme/proxy".to_string(),
            target: ResourceTarget {
                uri: "sel://acme/proxy".to_string(),
                host_id: String::new(),
                resource_id: 7,
                interface: None,
                tenant: Some("acme".to_string()),
                class: selium_abi::ResourceClass::SharedRegion,
                labels: Vec::new(),
            },
            owner: None,
            root_service: false,
        };
        assert!(matches!(
            denied_response(&register),
            DiscoveryResponse::Forbidden
        ));

        let revoke = DiscoveryRequest::Revoke {
            uri: "https://acme.com/path".to_string(),
        };
        assert!(matches!(
            denied_response(&revoke),
            DiscoveryResponse::Forbidden
        ));
    }

    // scenario: selium-runtime/owner-published-process-nodes/process-node-is-published-by-the-process
    // scenario: discovery-registration/discovery-uri-registration/caller-registers-a-uri
    // scenario: discovery-bootstrap/tier-2-guest-registration/owned-resource-registration-succeeds
    // scenario: discovery-registration/guest-custom-uri-validation/guest-registers-custom-uri-for-owned-resource
    // scenario: discovery-registration/tenant-scoped-process-uri-resolution/same-tenant-guest-resolves-process-uri
    // scenario: discovery-registration/deterministic-tenant-scoped-uri-schema/process-address
    // scenario: discovery-registration/process-node-addressing/a-process-without-resources-is-addressable
    #[test]
    fn end_to_end_discovery_lifecycle() {
        // Task 5.2 store-level golden path: spawn-node → allocate-region →
        // alias → label-query → teardown-revoke.
        let mut store = DiscoveryStore::default();

        // Spawn node: process 123 of tenant "acme" (registered by the runtime).
        store.store(process("acme", 123, vec![("app", "web")]), Some(123));
        assert!(
            store
                .resolve_exact("sel://acme/proc/123", Some("acme"))
                .is_some()
        );

        // Allocate region: runtime mints `sel://acme/region/7`.
        store.store(region("acme", 7), Some(123));
        assert!(
            store
                .resolve_exact("sel://acme/region/7", Some("acme"))
                .is_some()
        );

        // Route: the owning process registers a single-segment route
        // (`sel://acme/cache`) for the region. The route resolves through its
        // canonical typed target, and revoking the typed registration
        // cascades to the route below.
        let route = target(
            "sel://acme/cache",
            7,
            Some("acme"),
            selium_abi::ResourceClass::SharedRegion,
        );
        assert!(matches!(
            admit(&mut store, 123, Some("acme"), route, false, false),
            DiscoveryResponse::Registered
        ));
        assert_eq!(
            store.resolve_exact("sel://acme/cache", Some("acme")),
            Some(region("acme", 7))
        );

        // Label query: the process node carries `app=web`.
        assert_eq!(store.resolve_labels("app", "web", Some("acme")).len(), 1);

        // Teardown: revoking the region also revokes its alias.
        store.revoke_key("sel://acme/region/7");
        assert!(
            store
                .resolve_exact("sel://acme/region/7", Some("acme"))
                .is_none()
        );
        assert!(
            store
                .resolve_exact("sel://acme/cache", Some("acme"))
                .is_none()
        );

        // Revoking the process node makes it unresolvable.
        store.revoke_key("sel://acme/proc/123");
        assert!(
            store
                .resolve_exact("sel://acme/proc/123", Some("acme"))
                .is_none()
        );
    }

    // scenario: addressing/process-keyed-revocation/guest-exit-revokes-its-routes
    // scenario: discovery-registration/discovery-uri-registration/owner-revoked-on-exit
    #[test]
    fn owner_exit_revokes_owned_routes() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel://acme/bridge",
                7,
                Some("acme"),
                selium_abi::ResourceClass::HostQueue,
            ),
            false,
            false,
        );
        assert!(store.resolve_exact("bridge.acme", Some("acme")).is_some());

        // Owner-keyed revocation: the owning process exits and discovery
        // revokes its routes without a runtime-maintained side map.
        store.revoke_by_owner(42);
        assert_eq!(store.resolve_exact("bridge.acme", Some("acme")), None);
        assert_eq!(store.resolve_exact("sel://acme/bridge", Some("acme")), None);
    }

    // scenario: discovery-registration/root-registration-capability-gated/root-registration-requires-capability
    // scenario: discovery-registration/root-registration-capability-gated/capability-holding-guest-registers-in-root
    // scenario: guest-context/root-registration-capability-gated/root-registration-requires-capability
    // scenario: guest-context/root-registration-capability-gated/capability-holding-guest-registers-in-root
    #[test]
    fn root_registration_requires_capability() {
        let mut store = DiscoveryStore::default();
        store.store(queue("", 7), Some(42));

        // Without an exact route claim, a root-namespace route is refused.
        let denied = admit(
            &mut store,
            42,
            None,
            target(
                "sel:///dns/resolve",
                7,
                None,
                selium_abi::ResourceClass::HostQueue,
            ),
            false,
            false,
        );
        assert!(matches!(denied, DiscoveryResponse::Forbidden));

        // With the capability, the same registration is accepted.
        let allowed = admit(
            &mut store,
            42,
            None,
            target(
                "sel:///dns/resolve",
                7,
                None,
                selium_abi::ResourceClass::HostQueue,
            ),
            true,
            false,
        );
        assert!(matches!(allowed, DiscoveryResponse::Registered));
        assert!(store.resolve_exact("sel:///dns/resolve", None).is_some());
    }

    // scenario: addressing/apex-root-service-alias/domain-maps-directly-to-a-service
    #[test]
    fn root_service_designation_resolves_the_apex() {
        let mut store = DiscoveryStore::default();
        store.seed_domain("example.com", "acme");
        store.store(queue("acme", 7), Some(42));
        // The tenant designates `["http", "prod"]` as its root service.
        admit(
            &mut store,
            42,
            Some("acme"),
            target(
                "sel://acme/http/prod",
                7,
                Some("acme"),
                selium_abi::ResourceClass::HostQueue,
            ),
            false,
            true,
        );

        // The bare domain resolves to the designated root service.
        assert_eq!(
            store.resolve_exact("example.com", Some("acme")),
            Some(target(
                "sel://acme/http/prod",
                7,
                Some("acme"),
                selium_abi::ResourceClass::HostQueue,
            ))
        );
    }
    // scenario: addressing/single-canonical-namespace/internal-path-is-canonical
    // scenario: addressing/single-serve-declaration/one-declaration-yields-both-names

    #[test]
    fn wire_name_registration_produces_the_same_route_as_serve() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        // A guest registers a bare wire name; it translates to the internal
        // path and is resolvable both ways.
        let route = target(
            "bridge.acme",
            7,
            Some("acme"),
            selium_abi::ResourceClass::HostQueue,
        );
        let response = admit(&mut store, 42, Some("acme"), route, false, false);
        assert!(matches!(response, DiscoveryResponse::Registered));
        // Both spellings project to the same internal alias key (the wire name
        // is a projection, not a second key), which resolves through the
        // canonical typed target.
        assert_eq!(
            store.resolve_exact("bridge.acme", Some("acme")),
            Some(queue("acme", 7))
        );
        assert_eq!(
            store.resolve_exact("sel://acme/bridge", Some("acme")),
            Some(queue("acme", 7))
        );
    }

    // scenario: route-watch/route-change-generations/replacement-advances-the-generation
    // scenario: discovery-registration/route-change-publication/registration-publishes-an-appearance
    // scenario: discovery-registration/route-change-publication/replacement-advances-the-generation
    // scenario: discovery-registration/route-change-publication/lease-expiry-publishes-a-revocation
    #[test]
    fn route_table_publishes_appear_replace_and_revoke() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
        let (region_id, table) = create_route_table().expect("create route table");

        let mut store = DiscoveryStore::default();
        let uri = "sel://acme/queue/7".to_string();
        store.store(queue("acme", 7), Some(42));
        sync_route_table(&store, &table).expect("sync register");

        let channel = Channel::attach(region_id).expect("attach route table");
        let transport = ShmTransport::new_replay(&channel, &channel).expect("transport");
        let subscriber = Subscriber::new(FramedRead::new(transport), None);
        let view: selium_wire::LiveTableView<String, ResourceTarget, ShmTransport> =
            selium_wire::LiveTableView::new(subscriber).expect("view");

        let appeared = view.get_record(&uri).expect("get").expect("appeared");
        assert_eq!(
            appeared.value.as_ref().map(|target| target.resource_id),
            Some(7)
        );
        let first_version = appeared.version;

        // Replace the same URI with a new target: the generation advances.
        store.store(
            target(&uri, 99, Some("acme"), selium_abi::ResourceClass::HostQueue),
            Some(42),
        );
        sync_route_table(&store, &table).expect("sync replace");
        view.sync().expect("view sync");
        let replaced = view.get_record(&uri).expect("get").expect("replaced");
        assert_eq!(
            replaced.value.as_ref().map(|target| target.resource_id),
            Some(99)
        );
        assert!(
            replaced.version > first_version,
            "replacement advances the generation"
        );

        // Revoke: the table publishes a tombstone.
        store.revoke_key(&uri);
        sync_route_table(&store, &table).expect("sync revoke");
        view.sync().expect("view sync");
        let revoked = view.get_record(&uri).expect("get").expect("tombstone");
        assert!(revoked.value.is_none(), "revocation is a tombstone");
    }

    // scenario: route-watch/registration-leases/renewal-keeps-the-registration-live
    #[test]
    fn lease_renewal_keeps_a_registration_live() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        store.set_lease("sel://acme/queue/7", 42, 1_000);
        assert!(store.renew_lease("sel://acme/queue/7", 42, 1_000 + LEASE_TTL_MS - 1));
        store.expire_leases(1_000 + LEASE_TTL_MS + 5);
        assert!(
            store
                .resolve_exact("sel://acme/queue/7", Some("acme"))
                .is_some(),
            "a renewed lease stays live"
        );
    }

    // scenario: route-watch/registration-leases/lapsed-lease-expires-the-registration
    #[test]
    fn lapsed_lease_expires_the_registration() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        store.set_lease("sel://acme/queue/7", 42, 1_000);
        store.expire_leases(1_000 + LEASE_TTL_MS + 1);
        assert_eq!(
            store.resolve_exact("sel://acme/queue/7", Some("acme")),
            None,
            "a lapsed lease revokes the registration"
        );
    }

    #[test]
    fn lease_renewal_requires_the_owner() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        store.set_lease("sel://acme/queue/7", 42, 1_000);
        assert!(!store.renew_lease("sel://acme/queue/7", 99, 1_000));
    }

    /// A wire-name registration is leased under its projected internal URI, so
    /// renewal has to key the same way registration did: renewing the raw wire
    /// name must find the lease, not miss it and lapse the route.
    #[test]
    fn a_wire_name_registration_renews_under_the_key_it_was_registered_with() {
        let mut store = DiscoveryStore::default();
        // `bridge.acme` projects to `sel://acme/bridge`.
        store.set_lease("sel://acme/bridge", 42, 1_000);
        assert!(
            store.renew_lease_raw("bridge.acme", 42, 1_000),
            "renewing by the raw wire name finds the projected lease"
        );
        store.expire_leases(1_000 + LEASE_TTL_MS + 1);
        assert!(
            !store.renew_lease_raw("bridge.acme", 42, 1_000 + LEASE_TTL_MS + 2),
            "a lease nobody renewed has lapsed"
        );
    }

    #[test]
    fn a_health_record_outlives_the_lease_window_and_dies_with_its_owner() {
        // Health is advisory published state, not a leased route: it must not
        // lapse while its guest is alive and healthy (that would report every
        // long-running guest as unhealthy), but it disappears with the process
        // that wrote it.
        let mut store = DiscoveryStore::default();
        store.set_health(Some("acme"), 7, "api", true);
        store.expire_leases(LEASE_TTL_MS * 3);
        assert!(
            store
                .resolve_exact("sel://acme/health/api", Some("acme"))
                .is_some(),
            "a health record is not lease-expired"
        );

        store.revoke_by_owner(7);
        assert_eq!(
            store.resolve_exact("sel://acme/health/api", Some("acme")),
            None,
            "a health record is revoked when its owner exits"
        );
    }

    /// The health namespace is reserved: a guest cannot shadow another guest's
    /// health record by registering a same-named route of its own.
    #[test]
    fn the_health_namespace_is_reserved_against_guest_registration() {
        let mut store = DiscoveryStore::default();
        store.store(queue("acme", 7), Some(42));
        assert_eq!(
            store.registration_uri(
                "sel://acme/health/api",
                Some("acme"),
                &authority_for("acme"),
                false,
                7,
                &selium_abi::ResourceClass::HostQueue,
            ),
            None,
            "a guest may not register inside the health namespace"
        );
    }

    // scenario: route-watch/health-is-published-state/guest-reports-health-as-state
    #[test]
    fn health_record_sets_resolves_and_clears() {
        // Health is published state: a tenant-scoped record a peer resolves.
        let mut store = DiscoveryStore::default();
        store.set_health(Some("acme"), 7, "api", true);
        assert!(
            store
                .resolve_exact("sel://acme/health/api", Some("acme"))
                .is_some()
        );

        store.set_health(Some("acme"), 7, "api", false);
        assert!(
            store
                .resolve_exact("sel://acme/health/api", Some("acme"))
                .is_none(),
            "an absent health record is not observable"
        );
    }

    // scenario: selium-runtime/protocol-handler-route-claims/handler-guest-publishes-its-claim
    // scenario: route-claims/well-known-names-are-claims/handler-guest-claims-its-scheme
    // scenario: discovery-registration/root-tenant-namespace/system-guest-registers-under-root
    #[test]
    fn a_claimed_handler_route_resolves_and_is_revoked_by_owner() {
        let mut store = DiscoveryStore::default();
        let handler = target(
            "sel:///handler/sel-quic",
            42,
            None,
            selium_abi::ResourceClass::Process,
        );
        assert!(matches!(
            admit(&mut store, 42, None, handler, true, false),
            DiscoveryResponse::Registered
        ));
        let resolved = store
            .resolve_exact("sel:///handler/sel-quic", None)
            .expect("handler resolves");
        assert_eq!(resolved.resource_id, 42);

        store.revoke_by_owner(42);
        assert_eq!(store.resolve_exact("sel:///handler/sel-quic", None), None);
    }

    // scenario: route-claims/well-known-names-are-claims/another-guest-cannot-claim-a-scheme
    // scenario: guest-context/serve-based-route-registration/unclaimed-root-name-is-refused
    #[test]
    fn a_guest_without_a_claim_cannot_register_a_handler_route() {
        // The handler namespace is claim-gated: a root-authorised guest without
        // the exact claim is refused, so the pin stays unforgeable.
        let mut store = DiscoveryStore::default();
        let response = admit(
            &mut store,
            7,
            None,
            target(
                "sel:///handler/sel-quic",
                7,
                None,
                selium_abi::ResourceClass::Process,
            ),
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
    }

    // scenario: route-claims/host-issued-route-claims/claim-holder-publishes-a-reserved-name
    // scenario: route-claims/exclusive-live-claims/second-claimant-refused
    // scenario: route-claims/exclusive-live-claims/claim-released-on-owner-exit
    #[test]
    fn a_second_claimant_gets_conflict_and_ownership_exit_releases_the_name() {
        let mut store = DiscoveryStore::default();
        store.store(queue("", 7), Some(42));
        let name = |id: u64| {
            target(
                "sel:///control",
                id,
                None,
                selium_abi::ResourceClass::HostQueue,
            )
        };
        assert!(matches!(
            admit(&mut store, 42, None, name(7), true, false),
            DiscoveryResponse::Registered
        ));
        assert!(matches!(
            admit(&mut store, 99, None, name(8), true, false),
            DiscoveryResponse::Conflict
        ));
        // The first claimant's registration is intact.
        assert_eq!(
            store
                .resolve_exact("sel:///control", None)
                .map(|target| target.resource_id),
            Some(7)
        );

        // Owner exit releases the claimed name for a later claimant.
        store.apply_tier1_event(DiscoveryRequest::ProcessGone { process_id: 42 });
        assert!(matches!(
            admit(&mut store, 99, None, name(8), true, false),
            DiscoveryResponse::Registered
        ));
    }

    // scenario: discovery-registration/host-authoritative-admission/authority-failure-fails-closed
    #[test]
    fn an_authority_read_failure_fails_closed() {
        let failed = admission_authority(Err(selium_guest::GuestError::Host(
            "authority unavailable".to_string(),
        )));
        assert!(
            failed.is_none(),
            "admission must deny rather than fall back to the projection"
        );

        let answered = admission_authority(Ok(authority_for("acme")));
        assert!(answered.is_some());
    }

    // scenario: discovery-registration/guest-custom-uri-validation/guest-alias-admitted-while-the-projection-lags
    // scenario: discovery-registration/leaf-aliases/alias-resolves-only-through-its-canonical-target
    #[test]
    fn a_leaf_alias_admitted_before_the_canonical_is_projected_resolves_after_it() {
        let mut store = DiscoveryStore::default();
        let alias = target(
            "sel://acme/proxy",
            7,
            Some("acme"),
            selium_abi::ResourceClass::SharedRegion,
        );
        assert!(matches!(
            admit(&mut store, 42, Some("acme"), alias, false, false),
            DiscoveryResponse::Registered
        ));
        // The canonical typed target has not been projected yet: the alias
        // resolves through the canonical and therefore reports NotFound.
        assert_eq!(store.resolve_exact("sel://acme/proxy", Some("acme")), None);

        store.store(region("acme", 7), Some(42));
        assert_eq!(
            store.resolve_exact("sel://acme/proxy", Some("acme")),
            Some(region("acme", 7))
        );
    }

    #[test]
    fn freeing_an_unprojected_target_revokes_its_aliases() {
        let mut store = DiscoveryStore::default();
        let alias = target(
            "sel://acme/proxy",
            7,
            Some("acme"),
            selium_abi::ResourceClass::SharedRegion,
        );
        assert!(matches!(
            admit(&mut store, 42, Some("acme"), alias, false, false),
            DiscoveryResponse::Registered
        ));
        // The canonical record has not been projected, so the cascade has no
        // registration to start from.
        assert!(store.aliases.contains_key("sel://acme/proxy"));

        store.apply_tier1_event(DiscoveryRequest::ResourceFreed {
            class: "region".to_string(),
            id: 7,
        });
        assert!(
            store.aliases.is_empty(),
            "the alias must be revoked with the target it names"
        );

        // A later projection of the same identity must not resurrect it.
        store.store(region("acme", 7), Some(42));
        assert_eq!(store.resolve_exact("sel://acme/proxy", Some("acme")), None);
    }

    #[test]
    fn a_class_mismatch_is_forbidden_not_not_found() {
        // Process 42 owns id 7 as a shared region but claims it is a process
        // node: the authority answers "not owned, but owned as a region", so
        // admission reports a permission failure rather than a missing target.
        let mut store = DiscoveryStore::default();
        let response = store.apply_register(
            42,
            Some("acme"),
            target(
                "sel://acme/proxy",
                7,
                Some("acme"),
                selium_abi::ResourceClass::Process,
            ),
            &ResourceAuthority {
                owned: false,
                tenant: None,
                other_classes: vec![selium_abi::ResourceClass::SharedRegion],
            },
            false,
            false,
        );
        assert!(matches!(response, DiscoveryResponse::Forbidden));
        assert_eq!(store.resolve_exact("sel://acme/proxy", Some("acme")), None);
    }

    #[test]
    fn co_owner_exit_does_not_remove_the_shared_typed_record() {
        let mut store = DiscoveryStore::default();
        store.store(region("acme", 7), Some(42));
        // A second owner also published the same canonical record.
        store
            .route_owners
            .insert("sel://acme/region/7".to_string(), 99);

        // One co-owner exits: identity-keyed typed records survive.
        store.apply_tier1_event(DiscoveryRequest::ProcessGone { process_id: 42 });
        assert!(
            store
                .resolve_exact("sel://acme/region/7", Some("acme"))
                .is_some(),
            "a co-owner exit must not delete the shared record"
        );

        // The host actually frees the resource: now the identity-keyed record
        // is removed.
        store.apply_tier1_event(DiscoveryRequest::ResourceFreed {
            class: "region".to_string(),
            id: 7,
        });
        assert!(
            store
                .resolve_exact("sel://acme/region/7", Some("acme"))
                .is_none()
        );
    }

    // scenario: route-claims/expected-claim-reconciliation/unpublished-claim-is-visible-as-drift
    #[test]
    fn expected_claim_without_a_live_registration_is_reported_as_drift() {
        let store = DiscoveryStore::default();
        let expected = vec![
            "sel:///control".to_string(),
            "sel:///handler/sel-quic".to_string(),
        ];
        assert_eq!(store.unpublished_claims(&expected), expected);
    }

    #[test]
    fn a_published_claim_is_not_reported_as_drift() {
        let mut store = DiscoveryStore::default();
        assert!(matches!(
            admit(
                &mut store,
                42,
                None,
                target(
                    "sel:///control",
                    7,
                    None,
                    selium_abi::ResourceClass::HostQueue,
                ),
                true,
                false,
            ),
            DiscoveryResponse::Registered
        ));
        assert!(
            store
                .unpublished_claims(&["sel:///control".to_string()])
                .is_empty()
        );
    }
}
