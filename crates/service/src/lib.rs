//! Selium fabric service message types and their FlatBuffers codecs.
//!
//! This crate is the single authority for the fabric **service** message
//! types — discovery, control-plane, and scheduler requests/responses,
//! desired-state records, resource and interface metadata, and pipe-control
//! frames — together with their `.fbs` schemas, generated bindings, and
//! `FlatMsg`/`HasSchema` implementations. It depends on `selium-abi` for the
//! shared capability/resource vocabulary and the hostcall contract.

use flatbuffers::{FlatBufferBuilder, InvalidFlatbuffer};
use selium_abi::ResourceClass;
use selium_guest_macros::schema;
use thiserror::Error;

pub mod codec;
#[allow(warnings)]
#[rustfmt::skip]
pub mod fbs;
pub mod log;

// Allow generated schema bindings to refer to this crate by name.
extern crate self as selium_service;

/// Termination code for a channel the bridge could not resolve or attach.
pub const TERMINATE_ATTACH_FAILED: u32 = 2;
/// Termination code for a malformed or missing client handshake.
pub const TERMINATE_BAD_HANDSHAKE: u32 = 1;

/// Decode hook for non-schema field types that map onto a Flatbuffers scalar
/// or string field (selected with `#[schema(codec)]`), where decoding may be
/// strict and fail.
pub trait FieldDecoder: Sized {
    /// Decode the field from its `string` accessor.
    fn decode_field(value: Option<&str>) -> Result<Self, InvalidFlatbuffer>;
}

/// Helper for encoding schema fields into Flatbuffers-ready values.
pub trait FieldEncoder {
    /// Output type written into Flatbuffers args or vectors.
    type Output<'bldr>;

    /// Encode the field for Flatbuffers builders.
    fn encode_field<'bldr, A: flatbuffers::Allocator + 'bldr>(
        &self,
        builder: &mut FlatBufferBuilder<'bldr, A>,
    ) -> Self::Output<'bldr>;
}

/// Flatbuffers-backed message that can be transmitted over an endpoint.
pub trait FlatMsg: Sized {
    /// Encode the owned value into Flatbuffer bytes.
    fn encode(value: &Self) -> Vec<u8>;
    /// Decode the owned value from Flatbuffer bytes.
    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer>;
}

/// Marker trait linking a Rust type to a Flatbuffers schema.
pub trait HasSchema {
    /// Static schema descriptor used for port metadata.
    const SCHEMA: SchemaDescriptor;
}

/// Helper for converting Flatbuffer string accessors into owned `String`s.
pub trait StringFieldValue {
    /// Convert the accessor into an owned `String`.
    fn into_owned(self) -> String;
}

/// Error type for encoding/framing operations.
#[derive(Debug, Error)]
pub enum EncodingError {
    /// ABI framing error.
    #[error("framing error: {0:?}")]
    Framing(selium_abi::AbiError),
    /// Flatbuffers decode error.
    #[error("flatbuffer decode error: {0}")]
    Decode(flatbuffers::InvalidFlatbuffer),
}

/// Static descriptor describing the schema carried by an endpoint.
#[derive(Clone, Copy, Debug)]
pub struct SchemaDescriptor {
    /// Fully qualified schema name (used for human-friendly diagnostics).
    pub fqname: &'static str,
    /// 16-byte content hash identifying the schema.
    pub hash: [u8; 16],
}

/// Metadata describing a resource interface.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/discovery.fbs",
    ty = "selium.discovery.InterfaceMetadata",
    binding = "selium_service::fbs::selium::discovery::InterfaceMetadata"
)]
pub struct InterfaceMetadata {
    /// Interface name.
    pub name: String,
    /// Method names exposed by the interface.
    pub methods: Vec<String>,
}

/// A classification key/value label pair.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/discovery.fbs",
    ty = "selium.discovery.Label",
    binding = "selium_service::fbs::selium::discovery::Label"
)]
pub struct Label {
    /// Label key.
    pub key: String,
    /// Label value.
    pub value: String,
}

/// A single advisory domain-to-tenant mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/discovery.fbs",
    ty = "selium.discovery.DomainEntry",
    binding = "selium_service::fbs::selium::discovery::DomainEntry"
)]
pub struct DomainEntry {
    /// Domain (e.g. `example.com`).
    pub domain: String,
    /// Tenant the domain maps to (e.g. `acme`).
    pub tenant: String,
}

/// Target resource returned by discovery resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/discovery.fbs",
    ty = "selium.discovery.ResourceTarget",
    binding = "selium_service::fbs::selium::discovery::ResourceTarget"
)]
pub struct ResourceTarget {
    /// URI of the resource.
    pub uri: String,
    /// Host id where the resource resides.
    pub host_id: String,
    /// Resource identifier.
    pub resource_id: u64,
    /// Optional interface metadata.
    pub interface: Option<InterfaceMetadata>,
    /// Optional tenant identifier for multi-tenant isolation.
    pub tenant: Option<String>,
    /// Resource class identifying the typed segment of the target's URI.
    #[schema(codec)]
    pub class: ResourceClass,
    /// Classification key/value label pairs.
    pub labels: Vec<Label>,
}

/// Request sent to the discovery service.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/discovery.fbs",
    ty = "selium.discovery.DiscoveryRequest",
    binding = "selium_service::fbs::selium::discovery::DiscoveryRequest"
)]
pub enum DiscoveryRequest {
    /// Resolve a URI (typed, alias, root, or external name) to a resource target.
    #[field("uri")]
    Resolve(String),
    /// List the targets matching a prefix/wildcard query (`sel://<tenant>/<type>/*`).
    #[field("uri")]
    ResolvePrefix(String),
    /// Return every target in the caller's tenant matching a label pair.
    ResolveLabels {
        /// Label key to match.
        key: String,
        /// Label value to match.
        value: String,
    },
    /// Register a URI→target mapping.
    Register {
        /// URI to register.
        uri: String,
        /// Target resource to map the URI to.
        target: ResourceTarget,
        /// Owning process, populated by Tier-1 runtime registrations so the
        /// store can validate Tier-2 ownership without parsing the URI.
        owner: Option<u64>,
        /// Marks the registration as the tenant's root service, so the bare
        /// domain (apex) resolves to it.
        root_service: bool,
    },
    /// Remove a URI→target mapping.
    Revoke {
        /// URI to revoke.
        uri: String,
    },
    /// Revoke every route owned by a process (published by the runtime when
    /// the process exits). Owner-keyed revocation lives in discovery, not in a
    /// runtime-maintained side map.
    RevokeByOwner {
        /// Process whose owner-keyed registrations are revoked.
        process_id: u64,
    },
    /// Seed an out-of-band domain→tenant mapping (Tier-1 publish only).
    SeedDomain {
        /// Domain to map (e.g. `example.com`).
        domain: String,
        /// Tenant the domain maps to (e.g. `acme`).
        tenant: String,
    },
    /// Request the provisioned domain→tenant table. Used by connectors to
    /// obtain a copy for local wire-name resolution.
    ListDomains,
}

/// Response from the discovery service.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/discovery.fbs",
    ty = "selium.discovery.DiscoveryResponse",
    binding = "selium_service::fbs::selium::discovery::DiscoveryResponse"
)]
pub enum DiscoveryResponse {
    /// The requested URI was found.
    #[field("target")]
    Found(ResourceTarget),
    /// A query returned multiple matching targets (prefix enumeration,
    /// label queries). Preserves order.
    #[field("targets")]
    Resolved(Vec<ResourceTarget>),
    /// The requested URI was not found.
    NotFound,
    /// The URI was successfully registered.
    Registered,
    /// The URI was successfully revoked.
    Revoked,
    /// The caller is not authorised to register the given target.
    Forbidden,
    /// The provisioned domain→tenant table, returned for [`DiscoveryRequest::ListDomains`].
    #[field("domains")]
    Domains(Vec<DomainEntry>),
}

/// A deployment's desired state as recorded by the control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/control.fbs",
    ty = "selium.control.Deployment",
    binding = "selium_service::fbs::selium::control::Deployment"
)]
pub struct Deployment {
    /// Workload identifier.
    pub workload_id: String,
    /// Desired replica count.
    pub replicas: u32,
    /// Module reference: a blob-store manifest name or blob identity.
    pub module: String,
}

/// A pipeline binding between workload endpoints, recorded as desired state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/control.fbs",
    ty = "selium.control.PipelineBinding",
    binding = "selium_service::fbs::selium::control::PipelineBinding"
)]
pub struct PipelineBinding {
    /// Binding name.
    pub name: String,
    /// Source side of the binding.
    pub from: String,
    /// Destination side of the binding.
    pub to: String,
}

/// A control-plane desired-state record appended to the durable log.
///
/// The control plane's durable log is the store of record for accepted
/// intent; the deployment and pipeline projections are rebuilt from it on
/// replay. Every record carries the tenant it belongs to, so replay
/// partitions the projection by tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/control.fbs",
    ty = "selium.control.DesiredStateRecord",
    binding = "selium_service::fbs::selium::control::DesiredStateRecord"
)]
pub enum DesiredStateRecord {
    /// A deployment desired-state write.
    Deployment {
        /// Deployment desired state.
        #[field("deployment")]
        deployment: Deployment,
        /// Tenant the record belongs to.
        tenant: String,
    },
    /// A pipeline-binding desired-state write.
    PipelineBinding {
        /// Pipeline binding.
        #[field("pipeline")]
        pipeline: PipelineBinding,
        /// Tenant the record belongs to.
        tenant: String,
    },
    /// A workload stop: a tombstone removing the deployment from the
    /// projection, so replay does not resurrect a stopped workload.
    Stop {
        /// Workload identifier.
        workload_id: String,
        /// Tenant the record belongs to.
        tenant: String,
    },
}

/// A target returned by a control-plane resolve. The resolve projection of a
/// discovered resource: enough to attach (resource id, host) without the full
/// `ResourceTarget` taxonomy (class, tenant, interface, labels), which stays in
/// discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/control.fbs",
    ty = "selium.control.ResolvedTarget",
    binding = "selium_service::fbs::selium::control::ResolvedTarget"
)]
pub struct ResolvedTarget {
    /// URI of the resource.
    pub uri: String,
    /// Host id where the resource resides.
    pub host_id: String,
    /// Resource identifier.
    pub resource_id: u64,
}

/// Outcome of a single delegated interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/control.fbs",
    ty = "selium.control.DelegationStatus",
    binding = "selium_service::fbs::selium::control::DelegationStatus"
)]
pub struct DelegationStatus {
    /// Delegated step name (e.g. `scheduler` or `discovery`).
    pub step: String,
    /// Whether the delegated interaction was applied (not deferred/stubbed).
    pub applied: bool,
    /// Step context, human-readable but typed on the wire.
    pub context: String,
}

/// Request sent to the control-plane service.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/control.fbs",
    ty = "selium.control.ControlRequest",
    binding = "selium_service::fbs::selium::control::ControlRequest"
)]
pub enum ControlRequest {
    /// Record a deployment's desired state (delegating placement to the scheduler).
    Deploy {
        /// Workload identifier.
        workload_id: String,
        /// Desired replica count.
        replicas: u32,
        /// Module reference (manifest name or blob identity).
        module: String,
    },
    /// Scale a workload's desired state.
    Scale {
        /// Workload identifier.
        workload_id: String,
        /// New desired replica count.
        replicas: u32,
    },
    /// Stop a workload.
    Stop {
        /// Workload identifier.
        workload_id: String,
    },
    /// Resolve a URI through discovery.
    Resolve {
        /// URI to resolve.
        uri: String,
    },
    /// Upload module bytes to the blob store and record a manifest.
    Upload {
        /// Manifest name for the stored module.
        manifest: String,
        /// Module bytes to store.
        bytes: Vec<u8>,
    },
    /// Read the last accepted desired state for a workload.
    Status {
        /// Workload identifier.
        workload_id: String,
    },
}

/// Response from the control-plane service.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/control.fbs",
    ty = "selium.control.ControlResponse",
    binding = "selium_service::fbs::selium::control::ControlResponse"
)]
pub enum ControlResponse {
    /// A module was uploaded and stored under its manifest name.
    Uploaded {
        /// Manifest name for the stored module.
        manifest: String,
    },
    /// A desired-state request was accepted and recorded.
    Accepted {
        /// Workload identifier.
        workload_id: String,
        /// Recorded replica count.
        replicas: u32,
        /// Recorded module reference.
        module: String,
        /// Outcome of the primary delegated interaction.
        delegated: DelegationStatus,
    },
    /// The last accepted desired state for the requested workload.
    Status {
        /// The deployment if one is recorded, else `None`.
        deployment: Option<Deployment>,
    },
    /// A resolve request's outcome.
    Resolved {
        /// The resolved target, or `None` when the URI was not found.
        target: Option<ResolvedTarget>,
    },
    /// A typed failure naming the failed step and its context.
    Error {
        /// Step that failed (e.g. `scheduler`, `discovery`, `storage`).
        step: String,
        /// Context describing the failure.
        context: String,
    },
}

/// Request sent to the scheduler service.
///
/// Moved from the superseded external-api guest stub (which deferred these
/// types to `selium-abi` pending the scheduler guest crate).
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/scheduler.fbs",
    ty = "selium.scheduler.SchedulerRequest",
    binding = "selium_service::fbs::selium::scheduler::SchedulerRequest"
)]
pub enum SchedulerRequest {
    /// Ask the scheduler to place a workload.
    Place {
        /// Workload identifier.
        workload_id: String,
        /// Desired replica count.
        replicas: u32,
    },
    /// Ask the scheduler to scale a workload.
    Scale {
        /// Workload identifier.
        workload_id: String,
        /// New desired replica count.
        replicas: u32,
    },
    /// Ask the scheduler to stop a workload.
    Stop {
        /// Workload identifier.
        workload_id: String,
    },
}

/// Response from the scheduler service.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/scheduler.fbs",
    ty = "selium.scheduler.SchedulerResponse",
    binding = "selium_service::fbs::selium::scheduler::SchedulerResponse"
)]
pub enum SchedulerResponse {
    /// The scheduler applied the request.
    Applied,
    /// The scheduler accepted the intent but is not yet online; the request
    /// is recorded as desired state and reconciled once it lands.
    Deferred {
        /// Why application is deferred.
        reason: String,
    },
    /// The scheduler refused the request.
    Rejected {
        /// Rejection reason.
        reason: String,
    },
}

/// Request sent to the identity service.
///
/// The identity guest serves the platform's sole mint authority. The two
/// tiers — operator (tenant create/rotate/revoke) and tenant (issue that
/// tenant's user certificates, manage its principals) — are distinguished by
/// the identity guest from the caller's process tenant, never by a field in
/// this message. Non-ABI payloads are byte vectors: `spki_der` is a DER
/// SubjectPublicKeyInfo, `fingerprint` is the 32-byte SHA-256 leaf SPKI
/// fingerprint, and `grants` is a rkyv-encoded baseline grant set.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/identity.fbs",
    ty = "selium.identity.IdentityRequest",
    binding = "selium_service::fbs::selium::identity::IdentityRequest"
)]
pub enum IdentityRequest {
    /// Onboard a tenant: mint its CA and publish its trust anchor.
    MintTenantCa {
        /// Tenant whose CA is minted.
        tenant: String,
    },
    /// Rotate a tenant's CA: mint a successor and republish the anchor.
    RotateTenantCa {
        /// Tenant whose CA is rotated.
        tenant: String,
    },
    /// Revoke a tenant: remove its anchor and delete its CA key.
    RevokeTenant {
        /// Tenant whose CA is revoked.
        tenant: String,
    },
    /// Issue a short-TTL user leaf certificate from a client SPKI.
    IssueUserCert {
        /// Tenant whose CA signs the leaf.
        tenant: String,
        /// DER-encoded SubjectPublicKeyInfo of the client leaf key.
        spki_der: Vec<u8>,
    },
    /// Record the baseline grant set for a principal's fingerprint.
    SetPrincipalGrants {
        /// Tenant the principal belongs to.
        tenant: String,
        /// SHA-256 fingerprint of the principal's leaf SPKI.
        fingerprint: Vec<u8>,
        /// rkyv-encoded baseline grant set.
        grants: Vec<u8>,
    },
    /// Remove a principal's baseline grants.
    RemovePrincipal {
        /// Tenant the principal belongs to.
        tenant: String,
        /// SHA-256 fingerprint of the principal's leaf SPKI.
        fingerprint: Vec<u8>,
    },
}

/// Response from the identity service.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/identity.fbs",
    ty = "selium.identity.IdentityResponse",
    binding = "selium_service::fbs::selium::identity::IdentityResponse"
)]
pub enum IdentityResponse {
    /// A tenant CA was minted and its anchor published.
    TenantRecorded {
        /// The recorded tenant.
        tenant: String,
    },
    /// A tenant CA was rotated and its anchor republished.
    Rotated {
        /// The rotated tenant.
        tenant: String,
    },
    /// A tenant CA was revoked.
    Revoked {
        /// The revoked tenant.
        tenant: String,
    },
    /// A user leaf certificate was issued.
    UserCertIssued {
        /// DER-encoded leaf certificate.
        certificate_der: Vec<u8>,
    },
    /// A principal's baseline grants were recorded.
    PrincipalRecorded {
        /// The recorded principal's fingerprint.
        fingerprint: Vec<u8>,
    },
    /// A principal's baseline grants were removed.
    PrincipalRemoved {
        /// The removed principal's fingerprint.
        fingerprint: Vec<u8>,
    },
    /// A typed failure naming the failed step and its context.
    Error {
        /// Step that failed.
        step: String,
        /// Context describing the failure.
        context: String,
    },
}

/// A per-tenant metering bucket for one sampling interval, published by the
/// bookkeeper entrypoint to a shared-memory topic and merged by the
/// accountant entrypoint. Counter dimensions (`cpu_instructions`, `bandwidth_bytes`)
/// carry the interval's delta; gauge dimensions (`memory_bytes`,
/// `storage_bytes`) carry the current reading.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/accountant.fbs",
    ty = "selium.accountant.MeteringBucket",
    binding = "selium_service::fbs::selium::accountant::MeteringBucket"
)]
pub struct MeteringBucket {
    /// Tenant whose usage the bucket aggregates.
    pub tenant: String,
    /// CPU instruction delta for the interval (executed WebAssembly
    /// instructions).
    pub cpu_instructions: u64,
    /// Current memory usage in bytes.
    pub memory_bytes: u64,
    /// Current storage usage in bytes.
    pub storage_bytes: u64,
    /// Bandwidth delta in bytes.
    pub bandwidth_bytes: u64,
    /// Wall-clock publish time in unix seconds, stamped by the bookkeeper:
    /// the accountant buckets the bucket into its billing window by this
    /// stamp, not by its own receive-time clock.
    pub published_unix_s: u64,
}

/// Per-dimension ceiling values for a tenant's paid plan or opt-in overage
/// budget, authored by the operator through [`AccountantControl`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/accountant.fbs",
    ty = "selium.accountant.TenantPlan",
    binding = "selium_service::fbs::selium::accountant::TenantPlan"
)]
pub struct TenantPlan {
    /// CPU ceiling in instructions per minute.
    pub cpu_instructions: u64,
    /// Memory ceiling in bytes.
    pub memory_bytes: u64,
    /// Storage ceiling in bytes.
    pub storage_bytes: u64,
    /// Bandwidth ceiling in bytes per minute.
    pub bandwidth_bytes: u64,
}

/// Operator/billing control request served by the accountant: authoring the
/// plan (soft ceiling) and overage budget (hard ceiling = plan + overage), and
/// driving the rare billing-state transitions (delinquency/restoration).
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/accountant.fbs",
    ty = "selium.accountant.AccountantControl",
    binding = "selium_service::fbs::selium::accountant::AccountantControl"
)]
pub enum AccountantControl {
    /// Author the tenant's paid plan ceilings.
    SetPlan {
        /// Tenant whose plan is authored.
        tenant: String,
        /// Per-dimension soft ceilings.
        plan: TenantPlan,
    },
    /// Author the tenant's opt-in overage budget.
    SetOverage {
        /// Tenant whose overage budget is authored.
        tenant: String,
        /// Per-dimension hard-ceiling additions over the plan.
        overage: TenantPlan,
    },
    /// Mark the tenant delinquent: narrow its grants to nothing and zero its
    /// quotas.
    MarkDelinquent {
        /// Tenant to suspend.
        tenant: String,
    },
    /// Restore a delinquent tenant to good standing.
    MarkRestored {
        /// Tenant to restore.
        tenant: String,
    },
    /// Author the tenant's per-tenant process-count ceiling (default 100;
    /// operators raise it per tenant on request).
    SetProcessQuota {
        /// Tenant whose process ceiling is authored.
        tenant: String,
        /// Process-count ceiling.
        processes: u64,
    },
}

/// Response from the accountant's operator/billing control surface.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/accountant.fbs",
    ty = "selium.accountant.AccountantControlResponse",
    binding = "selium_service::fbs::selium::accountant::AccountantControlResponse"
)]
pub enum AccountantControlResponse {
    /// The control was applied and the tenant's enforcement state re-authored.
    Updated {
        /// Tenant whose enforcement state was updated.
        tenant: String,
    },
    /// The control was refused or failed.
    Error {
        /// Failure context.
        context: String,
    },
}

/// Typed per-stream control frames shared between the external client and the
/// bridge channel.
///
/// Carried as a normal frame with tag 0 before data relay begins. The
/// handshake is deterministic: the bridge replies with exactly one control
/// frame — [`PipeControl::Accepted`] once the channel is resolved and
/// attached, or [`PipeControl::Terminate`] (followed by stream teardown) on
/// refusal. Data frames are relayed verbatim and are never decoded as control
/// frames.
#[derive(Debug, Clone, PartialEq, Eq)]
#[schema(
    path = "schemas/pipe_control.fbs",
    ty = "selium.pipe.PipeControl",
    binding = "selium_service::fbs::selium::pipe::PipeControl"
)]
pub enum PipeControl {
    /// A client request to bridge the given channel URI.
    Handshake {
        /// Discovery URI of the fabric channel to bridge.
        uri: String,
    },
    /// A success reply: the named channel is resolved and attached, and the
    /// data relay begins immediately after this frame.
    Accepted,
    /// A terminal failure reply; the stream closes after it.
    Terminate {
        /// Machine-readable termination code.
        code: u32,
    },
}

impl From<selium_abi::AbiError> for EncodingError {
    fn from(error: selium_abi::AbiError) -> Self {
        Self::Framing(error)
    }
}

impl From<flatbuffers::InvalidFlatbuffer> for EncodingError {
    fn from(error: flatbuffers::InvalidFlatbuffer) -> Self {
        Self::Decode(error)
    }
}

impl DesiredStateRecord {
    /// Returns the tenant this record belongs to.
    pub fn tenant(&self) -> &str {
        match self {
            Self::Deployment { tenant, .. }
            | Self::PipelineBinding { tenant, .. }
            | Self::Stop { tenant, .. } => tenant,
        }
    }
}

impl FlatMsg for () {
    fn encode(_value: &Self) -> Vec<u8> {
        Vec::new()
    }

    fn decode(_bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(())
    }
}

impl HasSchema for () {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "empty_tuple",
        hash: [0; 16],
    };
}

impl FlatMsg for u32 {
    fn encode(value: &Self) -> Vec<u8> {
        value.to_le_bytes().into()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(u32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_e| InvalidFlatbuffer::ApparentSizeTooLarge)?,
        ))
    }
}

impl HasSchema for u32 {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "unsigned_thirty_two_bit_int",
        hash: [0, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3],
    };
}

impl FlatMsg for i32 {
    fn encode(value: &Self) -> Vec<u8> {
        value.to_le_bytes().into()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(i32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_e| InvalidFlatbuffer::ApparentSizeTooLarge)?,
        ))
    }
}

impl HasSchema for i32 {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "signed_thirty_two_bit_int",
        hash: [1, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3],
    };
}

impl FlatMsg for u64 {
    fn encode(value: &Self) -> Vec<u8> {
        value.to_le_bytes().into()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(u64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_e| InvalidFlatbuffer::ApparentSizeTooLarge)?,
        ))
    }
}

impl HasSchema for u64 {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "unsigned_sixty_four_bit_int",
        hash: [0, 6, 4, 6, 4, 6, 4, 6, 4, 6, 4, 6, 4, 6, 4, 6],
    };
}

impl FlatMsg for String {
    fn encode(value: &Self) -> Vec<u8> {
        value.as_bytes().to_owned()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(str::from_utf8(bytes)
            .map_err(|e| InvalidFlatbuffer::Utf8Error {
                error: e,
                range: 0..bytes.len(),
                error_trace: Default::default(),
            })?
            .to_owned())
    }
}

impl HasSchema for String {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "string",
        hash: [1; 16],
    };
}

impl FlatMsg for Vec<u8> {
    fn encode(value: &Self) -> Vec<u8> {
        value.clone()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(bytes.to_vec())
    }
}

impl HasSchema for Vec<u8> {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "byte_vector",
        hash: [2; 16],
    };
}

impl StringFieldValue for &str {
    fn into_owned(self) -> String {
        self.to_string()
    }
}

impl StringFieldValue for Option<&str> {
    fn into_owned(self) -> String {
        self.unwrap_or_default().to_string()
    }
}

/// Encodes a `ResourceClass` as its lowercased typed URI segment string.
impl FieldEncoder for ResourceClass {
    type Output<'bldr> = Option<flatbuffers::WIPOffset<&'bldr str>>;

    fn encode_field<'bldr, A: flatbuffers::Allocator + 'bldr>(
        &self,
        builder: &mut FlatBufferBuilder<'bldr, A>,
    ) -> Self::Output<'bldr> {
        Some(builder.create_string(self.uri_segment()))
    }
}

/// Strictly decodes a `ResourceClass` from its typed URI segment string. The
/// segment vocabulary is closed, so an unknown segment fails the decode rather
/// than silently misclassifying the target.
impl FieldDecoder for ResourceClass {
    fn decode_field(value: Option<&str>) -> Result<Self, InvalidFlatbuffer> {
        match value.and_then(ResourceClass::from_uri_segment) {
            Some(class) => Ok(class),
            None => InvalidFlatbuffer::new_missing_required("known resource class segment"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_round_trips() {
        let bytes = FlatMsg::encode(&());
        let decoded: () = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, ());
    }

    #[test]
    fn u32_round_trips() {
        let bytes = FlatMsg::encode(&42u32);
        let decoded: u32 = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, 42u32);
    }

    #[test]
    fn i32_round_trips() {
        let bytes = FlatMsg::encode(&-42i32);
        let decoded: i32 = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, -42i32);
    }

    #[test]
    fn u64_round_trips() {
        let bytes = FlatMsg::encode(&12345678901234u64);
        let decoded: u64 = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, 12345678901234u64);
    }

    #[test]
    fn string_round_trips() {
        let bytes = FlatMsg::encode(&"hello".to_string());
        let decoded: String = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, "hello".to_string());
    }

    #[test]
    fn vec_u8_round_trips() {
        let bytes = FlatMsg::encode(&vec![1u8, 2, 3]);
        let decoded: Vec<u8> = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, vec![1u8, 2, 3]);
    }

    #[test]
    fn discovery_request_round_trips() {
        let request = DiscoveryRequest::Resolve("sel://tenant/app/api".to_string());
        let bytes = FlatMsg::encode(&request);
        let decoded: DiscoveryRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn discovery_response_not_found_round_trips() {
        let response = DiscoveryResponse::NotFound;
        let bytes = FlatMsg::encode(&response);
        let decoded: DiscoveryResponse = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }

    #[test]
    fn discovery_response_found_round_trips() {
        let response = DiscoveryResponse::Found(ResourceTarget {
            uri: "sel://acme/region/7".to_string(),
            host_id: "host-1".to_string(),
            resource_id: 42,
            interface: Some(InterfaceMetadata {
                name: "MyInterface".to_string(),
                methods: vec!["method_a".to_string(), "method_b".to_string()],
            }),
            tenant: Some("acme".to_string()),
            class: selium_abi::ResourceClass::SharedRegion,
            labels: vec![Label {
                key: "app".to_string(),
                value: "web".to_string(),
            }],
        });
        let bytes = FlatMsg::encode(&response);
        let decoded: DiscoveryResponse = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }

    #[test]
    fn discovery_response_found_without_interface_round_trips() {
        let response = DiscoveryResponse::Found(ResourceTarget {
            uri: "sel://acme/region/7".to_string(),
            host_id: "host-1".to_string(),
            resource_id: 42,
            interface: None,
            tenant: None,
            class: selium_abi::ResourceClass::SharedRegion,
            labels: Vec::new(),
        });
        let bytes = FlatMsg::encode(&response);
        let decoded: DiscoveryResponse = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }

    #[test]
    fn discovery_prefix_query_round_trips() {
        let request = DiscoveryRequest::ResolvePrefix("sel://acme/region/*".to_string());
        let bytes = FlatMsg::encode(&request);
        let decoded: DiscoveryRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn discovery_label_query_round_trips() {
        let request = DiscoveryRequest::ResolveLabels {
            key: "app".to_string(),
            value: "web".to_string(),
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: DiscoveryRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn discovery_multi_target_response_round_trips() {
        let targets = vec![
            ResourceTarget {
                uri: "sel://acme/proc/1".to_string(),
                host_id: String::new(),
                resource_id: 1,
                interface: None,
                tenant: Some("acme".to_string()),
                class: selium_abi::ResourceClass::Process,
                labels: vec![Label {
                    key: "app".to_string(),
                    value: "web".to_string(),
                }],
            },
            ResourceTarget {
                uri: "sel://acme/proc/2".to_string(),
                host_id: String::new(),
                resource_id: 2,
                interface: None,
                tenant: Some("acme".to_string()),
                class: selium_abi::ResourceClass::Process,
                labels: vec![Label {
                    key: "app".to_string(),
                    value: "web".to_string(),
                }],
            },
        ];
        let response = DiscoveryResponse::Resolved(targets);
        let bytes = FlatMsg::encode(&response);
        let decoded: DiscoveryResponse = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }

    /// Strict decode: an unknown resource class segment is a decode error,
    /// not a silent default.
    #[test]
    fn unknown_resource_class_segment_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let uri = builder.create_string("sel://acme/region/7");
        let host_id = builder.create_string("");
        let class = builder.create_string("not-a-class");
        let args = crate::fbs::selium::discovery::ResourceTargetArgs {
            uri: Some(uri),
            host_id: Some(host_id),
            resource_id: 7,
            interface: None,
            tenant: None,
            class: Some(class),
            labels: None,
        };
        let root = crate::fbs::selium::discovery::ResourceTarget::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<ResourceTarget, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "unknown class segment must fail decode");
    }

    /// Strict decode: an unknown request variant tag is a decode error,
    /// not a silently reinterpreted default variant.
    #[test]
    fn unknown_request_variant_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::discovery::DiscoveryRequestArgs {
            variant: 42,
            uri: None,
            key: None,
            value: None,
            target: None,
            root_service: false,
            owner: 0,
            process_id: 0,
            domain: None,
            tenant: None,
        };
        let root = crate::fbs::selium::discovery::DiscoveryRequest::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<DiscoveryRequest, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "unknown variant tag must fail decode");
    }

    /// Strict decode: a Register without a target is a decode error, not a
    /// fabricated default target.
    #[test]
    fn register_without_target_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::discovery::DiscoveryRequestArgs {
            variant: 3,
            uri: None,
            key: None,
            value: None,
            target: None,
            root_service: false,
            owner: 0,
            process_id: 0,
            domain: None,
            tenant: None,
        };
        let root = crate::fbs::selium::discovery::DiscoveryRequest::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<DiscoveryRequest, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "Register without target must fail decode");
    }

    /// Strict decode: an unknown response variant tag is a decode error,
    /// not a silently reinterpreted default variant.
    #[test]
    fn unknown_response_variant_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::discovery::DiscoveryResponseArgs {
            variant: 77,
            target: None,
            targets: None,
            domains: None,
        };
        let root = crate::fbs::selium::discovery::DiscoveryResponse::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<DiscoveryResponse, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "unknown variant tag must fail decode");
    }

    /// Strict decode: a Found response without a target is a decode error,
    /// not a silent NotFound.
    #[test]
    fn found_without_target_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::discovery::DiscoveryResponseArgs {
            variant: 0,
            target: None,
            targets: None,
            domains: None,
        };
        let root = crate::fbs::selium::discovery::DiscoveryResponse::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<DiscoveryResponse, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "Found without target must fail decode");
    }

    #[test]
    fn control_request_deploy_round_trips() {
        let request = ControlRequest::Deploy {
            workload_id: "api".to_string(),
            replicas: 3,
            module: "api/v1".to_string(),
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: ControlRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn control_request_scale_round_trips() {
        let request = ControlRequest::Scale {
            workload_id: "api".to_string(),
            replicas: 5,
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: ControlRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn control_request_upload_round_trips_bytes() {
        let request = ControlRequest::Upload {
            manifest: "api/v1".to_string(),
            bytes: vec![0x00, 0x61, 0x73, 0x6d],
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: ControlRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn control_request_status_round_trips() {
        let request = ControlRequest::Status {
            workload_id: "api".to_string(),
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: ControlRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn control_response_accepted_round_trips() {
        let response = ControlResponse::Accepted {
            workload_id: "api".to_string(),
            replicas: 3,
            module: "api/v1".to_string(),
            delegated: DelegationStatus {
                step: "scheduler".to_string(),
                applied: false,
                context: "scheduler service not yet online".to_string(),
            },
        };
        let bytes = FlatMsg::encode(&response);
        let decoded: ControlResponse = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }

    #[test]
    fn control_response_resolved_round_trips() {
        let response = ControlResponse::Resolved {
            target: Some(ResolvedTarget {
                uri: "sel://acme/bridge".to_string(),
                host_id: "host-a".to_string(),
                resource_id: 42,
            }),
        };
        let bytes = FlatMsg::encode(&response);
        let decoded: ControlResponse = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }

    #[test]
    fn control_response_error_round_trips() {
        let response = ControlResponse::Error {
            step: "discovery".to_string(),
            context: "delegation failed".to_string(),
        };
        let bytes = FlatMsg::encode(&response);
        let decoded: ControlResponse = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }

    #[test]
    fn scheduler_request_place_round_trips() {
        let request = SchedulerRequest::Place {
            workload_id: "api".to_string(),
            replicas: 3,
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: SchedulerRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn scheduler_response_deferred_round_trips() {
        let response = SchedulerResponse::Deferred {
            reason: "scheduler not yet online".to_string(),
        };
        let bytes = FlatMsg::encode(&response);
        let decoded: SchedulerResponse = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, response);
    }

    #[test]
    fn pipe_control_handshake_round_trips() {
        let control = PipeControl::Handshake {
            uri: "sel://acme/lobby".to_string(),
        };
        let bytes = FlatMsg::encode(&control);
        let decoded: PipeControl = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, control);
    }

    #[test]
    fn pipe_control_accepted_round_trips() {
        let control = PipeControl::Accepted;
        let bytes = FlatMsg::encode(&control);
        let decoded: PipeControl = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, control);
    }

    #[test]
    fn pipe_control_terminate_round_trips() {
        let control = PipeControl::Terminate { code: 2 };
        let bytes = FlatMsg::encode(&control);
        let decoded: PipeControl = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, control);
    }

    /// Strict decode: an unknown pipe-control variant tag is a decode error.
    #[test]
    fn unknown_pipe_control_variant_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::pipe::PipeControlArgs {
            variant: 77,
            uri: None,
            code: 0,
        };
        let root = crate::fbs::selium::pipe::PipeControl::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<PipeControl, InvalidFlatbuffer> = FlatMsg::decode(&bytes);
        assert!(result.is_err(), "unknown variant tag must fail decode");
    }

    #[test]
    fn identity_request_mint_tenant_ca_round_trips() {
        let request = IdentityRequest::MintTenantCa {
            tenant: "acme".to_string(),
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: IdentityRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn identity_request_issue_user_cert_round_trips() {
        let request = IdentityRequest::IssueUserCert {
            tenant: "acme".to_string(),
            spki_der: vec![0x30, 0x82, 0x01, 0x02],
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: IdentityRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn identity_request_set_principal_grants_round_trips() {
        let request = IdentityRequest::SetPrincipalGrants {
            tenant: "acme".to_string(),
            fingerprint: vec![0xAB; 32],
            grants: vec![0x01, 0x02, 0x03],
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: IdentityRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn identity_request_remove_principal_round_trips() {
        let request = IdentityRequest::RemovePrincipal {
            tenant: "acme".to_string(),
            fingerprint: vec![0xCD; 32],
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: IdentityRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn identity_response_round_trips() {
        for response in [
            IdentityResponse::TenantRecorded {
                tenant: "acme".to_string(),
            },
            IdentityResponse::Revoked {
                tenant: "acme".to_string(),
            },
            IdentityResponse::UserCertIssued {
                certificate_der: vec![0x30, 0x82, 0x01, 0x00],
            },
            IdentityResponse::PrincipalRecorded {
                fingerprint: vec![0xEF; 32],
            },
            IdentityResponse::Error {
                step: "mint".to_string(),
                context: "tenant CA not found".to_string(),
            },
        ] {
            let bytes = FlatMsg::encode(&response);
            let decoded: IdentityResponse = FlatMsg::decode(&bytes).expect("decode");
            assert_eq!(decoded, response);
        }
    }

    #[test]
    fn metering_bucket_round_trips() {
        let bucket = MeteringBucket {
            tenant: "acme".to_string(),
            cpu_instructions: 1_234_567,
            memory_bytes: 65_536,
            storage_bytes: 4096,
            bandwidth_bytes: 10_000,
            published_unix_s: 1_789_439_040,
        };
        let bytes = FlatMsg::encode(&bucket);
        let decoded: MeteringBucket = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, bucket);
    }

    #[test]
    fn accountant_control_response_round_trips() {
        for response in [
            AccountantControlResponse::Updated {
                tenant: "acme".to_string(),
            },
            AccountantControlResponse::Error {
                context: "unknown tenant".to_string(),
            },
        ] {
            let bytes = FlatMsg::encode(&response);
            let decoded: AccountantControlResponse = FlatMsg::decode(&bytes).expect("decode");
            assert_eq!(decoded, response);
        }
    }

    #[test]
    fn tenant_plan_round_trips() {
        let plan = TenantPlan {
            cpu_instructions: 60_000_000,
            memory_bytes: 1_073_741_824,
            storage_bytes: 10_485_760,
            bandwidth_bytes: 104_857_600,
        };
        let bytes = FlatMsg::encode(&plan);
        let decoded: TenantPlan = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, plan);
    }

    #[test]
    fn accountant_control_round_trips() {
        for control in [
            AccountantControl::SetPlan {
                tenant: "acme".to_string(),
                plan: TenantPlan {
                    cpu_instructions: 0,
                    memory_bytes: 1024,
                    storage_bytes: 512,
                    bandwidth_bytes: 256,
                },
            },
            AccountantControl::SetOverage {
                tenant: "acme".to_string(),
                overage: TenantPlan {
                    cpu_instructions: 0,
                    memory_bytes: 256,
                    storage_bytes: 128,
                    bandwidth_bytes: 64,
                },
            },
            AccountantControl::MarkDelinquent {
                tenant: "acme".to_string(),
            },
            AccountantControl::MarkRestored {
                tenant: "acme".to_string(),
            },
            AccountantControl::SetProcessQuota {
                tenant: "acme".to_string(),
                processes: 250,
            },
        ] {
            let bytes = FlatMsg::encode(&control);
            let decoded: AccountantControl = FlatMsg::decode(&bytes).expect("decode");
            assert_eq!(decoded, control);
        }
    }

    /// Strict decode: an unknown identity request variant tag is a decode
    /// error, not a silently reinterpreted default variant.
    #[test]
    fn unknown_identity_request_variant_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::identity::IdentityRequestArgs {
            variant: 77,
            tenant: None,
            spki_der: None,
            fingerprint: None,
            grants: None,
        };
        let root = crate::fbs::selium::identity::IdentityRequest::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<IdentityRequest, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "unknown variant tag must fail decode");
    }

    /// Strict decode: an unknown control request variant tag is a decode
    /// error, not a silently reinterpreted default variant.
    #[test]
    fn unknown_control_request_variant_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::control::ControlRequestArgs {
            variant: 42,
            workload_id: None,
            replicas: 0,
            module: None,
            uri: None,
            manifest: None,
            bytes: None,
        };
        let root = crate::fbs::selium::control::ControlRequest::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<ControlRequest, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "unknown variant tag must fail decode");
    }

    /// Strict decode: an unknown scheduler response variant tag is a decode
    /// error, not a silently reinterpreted default variant.
    #[test]
    fn unknown_scheduler_response_variant_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::scheduler::SchedulerResponseArgs {
            variant: 77,
            reason: None,
        };
        let root = crate::fbs::selium::scheduler::SchedulerResponse::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<SchedulerResponse, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "unknown variant tag must fail decode");
    }

    /// The Tier-1-only `Register.owner` round-trips over the FlatBuffers wire
    /// (sentinel 0 = None on the wire).
    #[test]
    fn discovery_register_owner_round_trips() {
        let request = DiscoveryRequest::Register {
            uri: "sel://acme/region/7".to_string(),
            target: ResourceTarget {
                uri: "sel://acme/region/7".to_string(),
                host_id: String::new(),
                resource_id: 7,
                interface: None,
                tenant: Some("acme".to_string()),
                class: ResourceClass::SharedRegion,
                labels: Vec::new(),
            },
            owner: Some(42),
            root_service: false,
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: DiscoveryRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn discovery_register_without_owner_round_trips() {
        let request = DiscoveryRequest::Register {
            uri: "sel://acme/region/7".to_string(),
            target: ResourceTarget {
                uri: "sel://acme/region/7".to_string(),
                host_id: String::new(),
                resource_id: 7,
                interface: None,
                tenant: Some("acme".to_string()),
                class: ResourceClass::SharedRegion,
                labels: Vec::new(),
            },
            owner: None,
            root_service: false,
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: DiscoveryRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn discovery_revoke_by_owner_round_trips() {
        let request = DiscoveryRequest::RevokeByOwner { process_id: 99 };
        let bytes = FlatMsg::encode(&request);
        let decoded: DiscoveryRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn discovery_seed_domain_round_trips() {
        let request = DiscoveryRequest::SeedDomain {
            domain: "example.com".to_string(),
            tenant: "acme".to_string(),
        };
        let bytes = FlatMsg::encode(&request);
        let decoded: DiscoveryRequest = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, request);
    }

    #[test]
    fn desired_state_record_deployment_round_trips() {
        let record = DesiredStateRecord::Deployment {
            deployment: Deployment {
                workload_id: "api".to_string(),
                replicas: 3,
                module: "api/v1".to_string(),
            },
            tenant: "acme".to_string(),
        };
        let bytes = FlatMsg::encode(&record);
        let decoded: DesiredStateRecord = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn desired_state_record_pipeline_binding_round_trips() {
        let record = DesiredStateRecord::PipelineBinding {
            pipeline: PipelineBinding {
                name: "api-to-db".to_string(),
                from: "api".to_string(),
                to: "db".to_string(),
            },
            tenant: "acme".to_string(),
        };
        let bytes = FlatMsg::encode(&record);
        let decoded: DesiredStateRecord = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn desired_state_record_stop_round_trips() {
        let record = DesiredStateRecord::Stop {
            workload_id: "api".to_string(),
            tenant: "acme".to_string(),
        };
        let bytes = FlatMsg::encode(&record);
        let decoded: DesiredStateRecord = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, record);
    }

    #[test]
    fn pipeline_binding_round_trips() {
        let binding = PipelineBinding {
            name: "api-to-db".to_string(),
            from: "api".to_string(),
            to: "db".to_string(),
        };
        let bytes = FlatMsg::encode(&binding);
        let decoded: PipelineBinding = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(decoded, binding);
    }

    /// Strict decode: an unknown desired-state variant tag is a decode error.
    #[test]
    fn unknown_desired_state_variant_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::control::DesiredStateRecordArgs {
            variant: 77,
            deployment: None,
            pipeline: None,
            workload_id: None,
            tenant: None,
        };
        let root = crate::fbs::selium::control::DesiredStateRecord::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<DesiredStateRecord, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(result.is_err(), "unknown variant tag must fail decode");
    }

    /// Strict decode: a Deployment record without a deployment is a decode error.
    #[test]
    fn desired_state_deployment_without_payload_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::control::DesiredStateRecordArgs {
            variant: 0,
            deployment: None,
            pipeline: None,
            workload_id: None,
            tenant: None,
        };
        let root = crate::fbs::selium::control::DesiredStateRecord::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<DesiredStateRecord, InvalidFlatbuffer> =
            FlatMsg::decode(&bytes);
        assert!(
            result.is_err(),
            "missing deployment payload must fail decode"
        );
    }

    // --- Data-enum macro attribute coverage (task 3.5) ---

    /// Exercises `#[tag]`, `#[field]`, `#[schema(skip)]`, and mixed unit/data
    /// variants against the scheduler request table.
    #[derive(Debug, Clone, PartialEq)]
    #[schema(
        path = "schemas/scheduler.fbs",
        ty = "selium.scheduler.SchedulerRequest",
        binding = "selium_service::fbs::selium::scheduler::SchedulerRequest"
    )]
    enum AttrEnum {
        Locate {
            #[field("workload_id")]
            name: String,
            #[schema(skip)]
            note: Option<u32>,
        },
        Ping,
        Scale {
            workload_id: String,
            replicas: u32,
        },
        #[tag(7)]
        Done {
            workload_id: String,
        },
    }

    /// Exercises a required nested message field and its strict decode.
    #[derive(Debug, Clone, PartialEq)]
    #[schema(
        path = "schemas/control.fbs",
        ty = "selium.control.DesiredStateRecord",
        binding = "selium_service::fbs::selium::control::DesiredStateRecord"
    )]
    enum NestedEnum {
        Replace { deployment: Deployment },
        Remove { workload_id: String },
    }

    #[test]
    fn data_enum_field_rename_and_skip() {
        let original = AttrEnum::Locate {
            name: "api".to_string(),
            note: Some(99),
        };
        let decoded: AttrEnum = FlatMsg::decode(&FlatMsg::encode(&original)).expect("decode");
        assert_eq!(
            decoded,
            AttrEnum::Locate {
                name: "api".to_string(),
                note: None,
            },
            "renamed field read back; skipped field defaulted"
        );
    }

    #[test]
    fn data_enum_unit_variant_round_trips_alongside_data_variants() {
        let value = AttrEnum::Ping;
        let decoded: AttrEnum = FlatMsg::decode(&FlatMsg::encode(&value)).expect("decode");
        assert_eq!(decoded, value);
    }

    #[test]
    fn data_enum_tag_override_round_trips() {
        let value = AttrEnum::Done {
            workload_id: "web".to_string(),
        };
        let decoded: AttrEnum = FlatMsg::decode(&FlatMsg::encode(&value)).expect("decode");
        assert_eq!(decoded, value);
    }

    #[test]
    fn data_enum_scalar_and_string_round_trip() {
        let value = AttrEnum::Scale {
            workload_id: "db".to_string(),
            replicas: 3,
        };
        let decoded: AttrEnum = FlatMsg::decode(&FlatMsg::encode(&value)).expect("decode");
        assert_eq!(decoded, value);
    }

    #[test]
    fn data_enum_nested_field_round_trips() {
        let deployment = Deployment {
            workload_id: "api".to_string(),
            replicas: 3,
            module: "api/v1".to_string(),
        };
        let value = NestedEnum::Replace {
            deployment: deployment.clone(),
        };
        let decoded: NestedEnum = FlatMsg::decode(&FlatMsg::encode(&value)).expect("decode");
        assert_eq!(decoded, NestedEnum::Replace { deployment });
    }

    #[test]
    fn data_enum_missing_required_nested_field_is_decode_error() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let args = crate::fbs::selium::control::DesiredStateRecordArgs {
            variant: 0,
            deployment: None,
            pipeline: None,
            workload_id: None,
            tenant: None,
        };
        let root = crate::fbs::selium::control::DesiredStateRecord::create(&mut builder, &args);
        builder.finish(root, None);
        let bytes = builder.finished_data().to_vec();
        let result: ::std::result::Result<NestedEnum, InvalidFlatbuffer> = FlatMsg::decode(&bytes);
        assert!(
            result.is_err(),
            "missing required nested field must fail decode"
        );
    }
}
