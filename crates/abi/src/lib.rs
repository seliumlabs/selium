//! Selium ABI contracts shared by host and guest crates.
//!
//! # Capability Enforcement Matrix
//!
//! Grants are validated against an admission matrix at spawn time. A selector
//! is either **enforced** (evaluated against the runtime-built `ScopeContext`)
//! or **rejected** (the grant fails at spawn with a precise error naming the
//! selector). This prevents the "accept-then-always-deny" trap.
//!
//! | Selector | Status | Scope context field |
//! |---|---|---|
//! | `ResourceClass` | **Enforced** | `resource_class` |
//! | `Locality` | **Enforced** | `locality` |
//! | `ExplicitResource` | **Enforced** | `resource_id` |
//! | `Tenant` | **Enforced** | `tenant` (from process authority) |
//! | `Namespace` | **Enforced** | `tenant` (from process authority) |
//! | `Children` | **Enforced** | requires process-tree access (runtime) |
//! | `Uri` | **Enforced** | `uri` (exact, canonicalised) |
//! | `UriPrefix` | **Enforced*** | `uri` (network endpoints and `sel://`) |
//!
//! \* `UriPrefix` is evaluatable only when the same grant also carries a
//! selector the prefix applies to: a network `ResourceClass` selector
//! (`TcpListener`, `TcpStream`, or `UdpSocket`), or `Volume`. Otherwise the
//! grant is rejected at registration time. For `sel://` URIs the prefix is
//! path-segment aware rather than a plain string prefix.
//!
//! An **empty** selector list means "unrestricted within the capability" and is
//! explicitly accepted. Intersection semantics apply: all selectors in a grant
//! must match for the grant to apply to a given scope context.

use rkyv::{
    Archive, Deserialize, Serialize,
    api::high::{HighDeserializer, HighSerializer, HighValidator},
    rancor::Error as RancorError,
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};
use thiserror::Error;

pub mod client_identity;
/// Layout constants for the guest wake mailbox shared with the host.
///
/// The mailbox carries two wake channels:
///
/// - The **ring** (cooperative guests): the host enqueues task ids into the
///   ring and the guest drains it during a reactor poll. Single-producer
///   (host) / single-consumer (guest) by construction. Both sides access the
///   ring words with atomics; the `FLAG_OFFSET` word is an advisory hint only
///   (pending is derived from `head != tail`).
/// - The **parking-word area** (multithreaded guests): one shared `WAKE_WORD`
///   plus one `u32` parking word per task id. A wake is delivered by bumping
///   the target task's parking word and notifying, so a worker parked on the
///   shared wake word resumes in place — the host never drives the guest's
///   reactor as a unit. See `parking_word_offset`. Both the host and the guest
///   bump parking/wake words with genuine atomic read-modify-writes (the guest
///   never takes the host's memory lock), so the counter is never lost to a
///   torn read-modify-write.
pub mod mailbox {
    /// Byte offset of the ring head word.
    pub const HEAD_OFFSET: usize = 0;
    /// Byte offset of the wake flag word. Advisory only: the producer sets it
    /// on enqueue and the consumer clears it after draining, but wake-delivery
    /// correctness derives from `head != tail` (the flag set can be lost to a
    /// racing flag clear). Retained for ABI-layout stability.
    pub const FLAG_OFFSET: usize = 4;
    /// Byte offset of the ring tail word.
    pub const TAIL_OFFSET: usize = 8;
    /// Byte offset of the ring capacity word.
    pub const CAPACITY_OFFSET: usize = 12;
    /// Byte offset where ring slots begin.
    pub const RING_OFFSET: usize = 16;
    /// Number of task wake slots in the mailbox ring.
    pub const CAPACITY: usize = 32;
    /// Size in bytes of each mailbox slot.
    pub const SLOT_SIZE: usize = 4;
    /// Byte offset of the shared wake word (multithreaded guests). Bumped and
    /// notified whenever any task is enqueued or woken, so a worker parked on
    /// this word resumes to re-check the run queue.
    pub const WAKE_WORD_OFFSET: usize = RING_OFFSET + CAPACITY * SLOT_SIZE;
    /// Byte offset where the per-task parking-word table begins. Each entry is
    /// one `u32` (see [`parking_word_offset`]).
    pub const PARK_WORDS_OFFSET: usize = WAKE_WORD_OFFSET + 4;
    /// Number of per-task parking words. Task ids below this bound have a
    /// host-reachable parking word; ids at or above it fall back to the ring
    /// mailbox wake path.
    pub const PARK_WORDS_CAPACITY: usize = 1024;
    /// Byte offset of the process-stop word. The host sets it to a nonzero
    /// value and notifies the shared wake word to make multithreaded workers
    /// return from the worker entry (process teardown). Kept after the
    /// parking-word table so the ring/wake-word offsets stay stable.
    pub const STOP_OFFSET: usize = PARK_WORDS_OFFSET + PARK_WORDS_CAPACITY * SLOT_SIZE;
    /// Total mailbox byte length.
    pub const BYTE_LEN: usize = STOP_OFFSET + 4;

    /// Returns the byte offset of `task_id`'s parking word within the mailbox,
    /// or `None` when the task id is beyond [`PARK_WORDS_CAPACITY`].
    ///
    /// The word is a `u32` that the host bumps (with an atomic fetch-add) and
    /// notifies to resume a worker parked on the task; the guest's wake path
    /// bumps the same word so both sides observe one consistent counter.
    pub const fn parking_word_offset(task_id: super::TaskId) -> Option<usize> {
        let task_id = task_id as usize;
        if task_id < PARK_WORDS_CAPACITY {
            Some(PARK_WORDS_OFFSET + task_id * SLOT_SIZE)
        } else {
            None
        }
    }
}
pub mod uri;

/// Identifier for a resource handle that is local to one process or host context.
pub type LocalResourceId = u64;
/// Identifier for an asynchronous hostcall operation.
pub type OperationId = u64;
/// Identifier for a Selium process.
pub type ProcessId = u64;
/// Identifier for a resource that may be shared across local handles.
pub type SharedResourceId = u64;
/// Identifier for a guest task waiting on host progress.
pub type TaskId = u32;

/// Packed status code for a dropped hostcall.
pub const HOSTCALL_STATUS_DROPPED: u32 = 4;
/// Packed status code for a failed hostcall.
pub const HOSTCALL_STATUS_FAILED: u32 = 2;
/// Packed status code for an output buffer that is too small.
pub const HOSTCALL_STATUS_OUTPUT_TOO_SMALL: u32 = 3;
/// Packed status code for a pending hostcall.
pub const HOSTCALL_STATUS_PENDING: u32 = 1;
/// Packed status code for a ready hostcall.
pub const HOSTCALL_STATUS_READY: u32 = 0;
/// Maximum byte length of a `HostQueueSend` metadata payload. Senders
/// exceeding this bound are rejected by the runtime before the payload
/// reaches the kernel queue, bounding per-entry queue memory. Generous
/// headroom over the ~45-byte [`client_identity::ClientIdentity`] encoding
/// for future metadata users (e.g. HTTP identity hints).
pub const METADATA_MAX_BYTES: usize = 4096;

/// Maximum encoded byte length of a [`HostcallEnvelope`] accepted from a guest.
///
/// The host rejects a declared envelope length above this ceiling before
/// allocating a decode buffer, so hostile input cannot request an unbounded
/// allocation. The ceiling accommodates inline system-guest modules (hundreds
/// of KiB) with headroom while keeping per-request host memory bounded.
pub const HOSTCALL_ENVELOPE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Greatest sleep duration, in milliseconds, that a guest may request.
///
/// A hundred years is far beyond any legitimate platform timer while leaving
/// comfortable headroom for `Instant` representation on every supported
/// target. Requests above this ceiling (including `u64::MAX`) are rejected
/// with `MalformedPayload` before a deadline or timer is created.
pub const MAX_SLEEP_MILLIS: u64 = 100 * 365 * 24 * 60 * 60 * 1000;

/// Magic value identifying a Selium hostcall request preamble.
pub const HOSTCALL_MAGIC: u32 = 0x5345_4C4D;
/// Current host/guest ABI version for hostcall request encoding.
///
/// Host and guest packages share this version in lockstep during the alpha:
/// a request carrying any other version is rejected before rkyv enum
/// dispatch, so an incompatible layout is never decoded as another variant.
/// Version 2 also makes hostcall creation return the full [`OperationId`];
/// status/length packing remains confined to polling, whose value is a `u32`.
/// Version 3 replaces executable negotiation with the passive custom section
/// below, so no guest code runs before its declared version is validated.
pub const SELIUM_ABI_VERSION: u32 = 3;
/// Custom section containing the guest's ABI version as four little-endian
/// bytes. The runtime reads this declaration from the module bytes without
/// instantiating or executing guest code.
pub const SELIUM_ABI_VERSION_SECTION: &str = "selium_abi_version";
/// Fixed preamble size: `[magic: u32 LE][abi version: u32 LE]`.
pub const HOSTCALL_PREAMBLE_SIZE: usize = 8;

/// Marker trait for values that can be encoded with Selium's rkyv codec.
pub trait RkyvEncode:
    Archive + for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RancorError>>
{
}

/// Metadata describing a guest entrypoint export.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct EntrypointMetadata {
    /// Entrypoint export name.
    pub name: String,
}

/// Capability required to perform a class of host operations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum Capability {
    /// Permission to start, stop, and inspect processes.
    ProcessLifecycle,
    /// Permission to allocate, attach, read, write, and destroy shared memory.
    SharedMemory,
    /// Permission to create, attach, notify, wait on, and close signals.
    Signal,
    /// Permission to use network listeners, sessions, streams, and requests.
    Network,
    /// Permission to use durable volumes.
    Storage,
    /// Permission to manage session lifetime.
    SessionLifecycle,
    /// Permission to read activity log events.
    ActivityRead,
    /// Permission to read metering observations.
    MeteringRead,
    /// Permission to read guest log entries.
    GuestLogRead,
    /// Permission to write guest log entries.
    GuestLogWrite,
    /// Permission to create, attach, send, and receive from host-mediated connection queues.
    HostQueue,
    /// Permission to confer child grants the holder does not itself hold,
    /// subject to a tenant scope. Held only by system guests (e.g. the
    /// per-tenant bridge-server) that spawn children carrying an external
    /// client's grants.
    DelegateGrants,
    /// Permission to publish one exact reserved registry name (`sel:///…`)
    /// under a host-issued, non-delegable claim. The URI the claim covers is
    /// carried by a `ResourceSelector::Uri`; without a matching claim a guest
    /// cannot register that name. Bootstrap-provisioned only: a spawn that
    /// confers it on a child is denied.
    RouteClaim,
    /// Permission to sign tenant CAs and user certificates through the signing
    /// hostcalls (`SignTenantCa`, `SignUserCert`, `RevokeCa`). Bootstrap-
    /// provisioned only: like `DelegateGrants`, it can never be conferred on a
    /// child process. Held solely by the identity system guest.
    MintCertificate,
    /// Permission to author host-held quota counters via the `QuotaSet` and
    /// `QuotaClear` hostcalls. Bootstrap-provisioned only: like
    /// `DelegateGrants` and `MintCertificate`, it can never be conferred on a
    /// child process. Held solely by the accounting system guest.
    QuotaWrite,
    /// Permission to probe whether another process holds a capability via the
    /// `ProcessCapability` hostcall. Bootstrap-provisioned only: like
    /// `DelegateGrants`, `MintCertificate` and `QuotaWrite`, it can never be
    /// conferred on a child process. Held by a registry implementation (e.g.
    /// the discovery system guest) that must gate a request against the
    /// caller's grants.
    ProcessProbe,
    /// Permission to author the advisory domain-to-tenant mapping via the
    /// `SetDomain` discovery request. Bootstrap-provisioned only: like
    /// `DelegateGrants`, `MintCertificate` and `QuotaWrite`, it can never be
    /// conferred on a child process. Held solely by the identity system guest,
    /// which owns the mapping durably.
    DomainWrite,
}

impl Capability {
    /// Every capability, in declaration order.
    ///
    /// This is the single source for whole-vocabulary checks (coverage tests,
    /// bootstrap-provisioning audits, documentation). [`Self::name`] is
    /// exhaustive, so adding a variant fails to compile until it is named; the
    /// coverage test then fails until the variant is listed here too.
    pub const ALL: &'static [Capability] = &[
        Self::ProcessLifecycle,
        Self::SharedMemory,
        Self::Signal,
        Self::Network,
        Self::Storage,
        Self::SessionLifecycle,
        Self::ActivityRead,
        Self::MeteringRead,
        Self::GuestLogRead,
        Self::GuestLogWrite,
        Self::HostQueue,
        Self::DelegateGrants,
        Self::RouteClaim,
        Self::MintCertificate,
        Self::QuotaWrite,
        Self::ProcessProbe,
        Self::DomainWrite,
    ];

    /// The capability's stable identifier, used in diagnostics and error
    /// messages.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ProcessLifecycle => "ProcessLifecycle",
            Self::SharedMemory => "SharedMemory",
            Self::Signal => "Signal",
            Self::Network => "Network",
            Self::Storage => "Storage",
            Self::SessionLifecycle => "SessionLifecycle",
            Self::ActivityRead => "ActivityRead",
            Self::MeteringRead => "MeteringRead",
            Self::GuestLogRead => "GuestLogRead",
            Self::GuestLogWrite => "GuestLogWrite",
            Self::HostQueue => "HostQueue",
            Self::DelegateGrants => "DelegateGrants",
            Self::RouteClaim => "RouteClaim",
            Self::MintCertificate => "MintCertificate",
            Self::QuotaWrite => "QuotaWrite",
            Self::ProcessProbe => "ProcessProbe",
            Self::DomainWrite => "DomainWrite",
        }
    }

    /// True when only the host's bootstrap may provision this capability: a
    /// spawn that confers it on a child process is denied, even when the
    /// parent holds it.
    pub const fn is_bootstrap_provisioned(&self) -> bool {
        matches!(
            self,
            Self::DelegateGrants
                | Self::RouteClaim
                | Self::MintCertificate
                | Self::QuotaWrite
                | Self::ProcessProbe
                | Self::DomainWrite
        )
    }
}

/// Identity of a resource in either local-handle or shared-resource space.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize, Deserialize,
)]
#[rkyv(bytecheck())]
pub enum ResourceIdentity {
    /// A local resource handle.
    Local(LocalResourceId),
    /// A shared resource identity.
    Shared(SharedResourceId),
}

/// Locality against which capability selectors can be matched.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum LocalityScope {
    /// Any locality is accepted.
    Any,
    /// Any process within the cluster is accepted.
    Cluster,
    /// A specific host is accepted.
    Host(String),
}

/// Class of resource protected by capability checks.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum ResourceClass {
    /// Shared memory region.
    SharedRegion,
    /// Local mapping of a shared memory region.
    SharedMapping,
    /// Signal resource.
    Signal,
    /// TCP listener resource.
    TcpListener,
    /// TCP stream resource.
    TcpStream,
    /// UDP socket resource.
    UdpSocket,
    /// Durable volume resource: an opaque byte container.
    Volume,
    /// Process resource.
    Process,
    /// Activity log resource.
    ActivityLog,
    /// Metering stream resource.
    MeteringStream,
    /// Guest log resource.
    GuestLog,
    /// Host-mediated connection queue resource.
    HostQueue,
    /// Per-tenant CPU instruction ceiling (a quota dimension, not an
    /// allocatable resource): the accountant authors a tenant's per-minute
    /// executed-instruction ceiling under this class and the runtime translates
    /// it into per-process engine execution budgets.
    Cpu,
}

/// Context used to evaluate a capability grant.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct ScopeContext {
    /// Optional tenant name associated with the operation.
    pub tenant: Option<String>,
    /// Optional resource URI associated with the operation.
    pub uri: Option<String>,
    /// Locality where the operation is performed.
    pub locality: LocalityScope,
    /// Optional class of resource being accessed.
    pub resource_class: Option<ResourceClass>,
    /// Optional concrete resource identity being accessed.
    pub resource_id: Option<ResourceIdentity>,
}

/// A requestor namespace: either the platform root (no tenant) or a tenant.
///
/// This is the requestor-tenant vocabulary shared by capability selectors
/// (`ResourceSelector::Namespace`), delegation scopes, and the control-plane /
/// bridge-server requestor-derivation path. `Root` is the platform namespace
/// (`None` process tenant); `Tenant(t)` is a named tenant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum Namespace {
    /// The platform root namespace: a scope context with no tenant.
    Root,
    /// A named tenant namespace.
    Tenant(String),
}

/// Selector that narrows where a capability grant applies.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum ResourceSelector {
    /// Match a tenant name exactly.
    Tenant(String),
    /// Match a requestor namespace hierarchically: `Namespace::Root` admits a
    /// root scope context, `Namespace::Tenant(t)` admits tenant `t` exactly.
    /// Unlike `Tenant`, this variant also carries the `Root` value, which is
    /// required for `DelegateGrants` to express root-wide delegation.
    Namespace(Namespace),
    /// Match resources whose URI starts with the prefix. For `sel://` URIs
    /// matching is path-segment aware; for `tcp://`/`udp://` endpoints it is
    /// component-aware; other URIs keep plain string-prefix semantics.
    UriPrefix(String),
    /// Match one exact canonical registry URI. Both sides are normalised to
    /// the canonical internal `sel://…` form before comparison, and the
    /// selector is never treated as a prefix.
    Uri(String),
    /// Match an operation locality.
    Locality(LocalityScope),
    /// Match a resource class.
    ResourceClass(ResourceClass),
    /// Match a concrete resource identity.
    ExplicitResource(ResourceIdentity),
    /// Match processes that are descendants of the grantee (children,
    /// grandchildren, etc.). Used by metering/activity/guest-log read
    /// grants so supervisors can read their descendants' telemetry.
    Children,
}

/// Scope an owner-declared attachable-by-resolution declaration admits.
///
/// The owner of a shared resource authors the declaration and the host
/// enforces it: a resolve-recording naming a client process is accepted as
/// that process's attach basis only when the declaration's scope admits the
/// process. A narrower scope keeps the owner's authority from being broadened
/// by a resolver.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum AttachDeclarationScope {
    /// Any client process is admitted by the declaration.
    Any,
    /// Only client processes whose tenant matches the named tenant are
    /// admitted by the declaration.
    Tenant(String),
}

/// Grant allowing one capability within the intersection of its selectors.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct CapabilityGrant {
    /// Capability being granted.
    pub capability: Capability,
    /// Selectors that must all match for the grant to apply.
    pub selectors: Vec<ResourceSelector>,
}

/// Authoritative ownership answer for `(class, id)`.
///
/// `owned` says whether the queried process owns the resource under the class
/// it asked about; when it does, `tenant` is the tenant the host minted the
/// resource under (an empty string for the root/platform tenant). The answer is
/// read from the host's mint-time records, never from a guest-projected store.
///
/// `other_classes` is populated only when `owned` is false: it lists the
/// classes under which the queried process *does* own a resource with the same
/// id. It lets an admission decision tell a class mismatch (a permission
/// failure) apart from an identity the caller does not own at all.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct ResourceAuthority {
    /// Whether the queried process owns the resource under the queried class.
    pub owned: bool,
    /// Tenant the resource was minted under, when owned.
    pub tenant: Option<String>,
    /// Classes the process owns for the same id when it does not own the
    /// queried class. Empty when `owned` is true or nothing with that id is
    /// owned.
    pub other_classes: Vec<ResourceClass>,
}

/// Stable error code returned across the host-guest ABI.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum AbiErrorCode {
    /// A supplied handle or operation id is invalid.
    InvalidHandle,
    /// A resource was used after being detached or closed.
    DetachedResource,
    /// The caller does not have the required capability.
    PermissionDenied,
    /// The caller's tenant has exhausted its quota ceiling for the resource
    /// class named in the error message.
    QuotaExceeded,
    /// Payload bytes could not be decoded or framed correctly.
    MalformedPayload,
    /// Requested resource was not found.
    NotFound,
    /// Operation timed out.
    Timeout,
    /// Host-side internal failure.
    Internal,
}

/// Error value returned over the Selium ABI.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct AbiError {
    /// Stable machine-readable error code.
    pub code: AbiErrorCode,
    /// Human-readable error details.
    pub message: String,
}

/// Descriptor for an allocated shared memory region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct SharedRegionDescriptor {
    /// Shared region id.
    pub shared_id: SharedResourceId,
    /// Region length in bytes.
    pub len: u64,
}

/// Descriptor for a local mapping of a shared memory region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct SharedMappingDescriptor {
    /// Local mapping id.
    pub local_id: LocalResourceId,
    /// Shared region id backing the mapping.
    pub shared_id: SharedResourceId,
    /// Mapping length in bytes.
    pub len: u64,
}

/// Descriptor for a signal handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct SignalDescriptor {
    /// Local signal handle id.
    pub local_id: LocalResourceId,
    /// Shared signal id.
    pub shared_id: SharedResourceId,
}

/// Memory protection level for a shared region mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
#[repr(u8)]
pub enum RegionProt {
    /// Read-only mapping (`PROT_READ`).
    ReadOnly = 0,
    /// Read-write mapping (`PROT_READ | PROT_WRITE`).
    ReadWrite = 1,
}

/// Descriptor for a shared region allocation returned by `AllocRegion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct RegionAllocation {
    /// Shared region id.
    pub region_id: u64,
    /// Page offset within guest linear memory where the region is mapped.
    pub page_offset: u32,
}

/// Descriptor for a shared region attachment returned by `AttachRegion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct RegionAttachment {
    /// Page offset within guest linear memory where the region is mapped.
    pub page_offset: u32,
    /// Total size of the attached region in bytes.
    pub len: u32,
}

/// Descriptor for a host-mediated connection queue handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct HostQueueDescriptor {
    /// Local queue handle id.
    pub local_id: LocalResourceId,
    /// Shared queue id.
    pub shared_id: SharedResourceId,
}

/// Descriptor for an opened durable volume handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct VolumeDescriptor {
    /// Local volume handle id.
    pub local_id: LocalResourceId,
    /// Current length of the volume in bytes.
    pub len: u64,
}

/// Descriptor for a guest process.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct ProcessDescriptor {
    /// Process id.
    pub local_id: ProcessId,
    /// Module id used to start the process.
    pub module_id: String,
    /// Entrypoint export used to start the process.
    pub entrypoint: String,
}

/// Source of a process's module bytes.
///
/// The runtime holds no module catalogue: every spawn supplies the module
/// bytes, either inline in the hostcall payload or as a durable slice the
/// runtime reads from its own volume layer under the caller's storage
/// authority. A slice keeps module bytes from round-tripping through a guest.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum ModuleSource {
    /// Inline module bytes carried in the hostcall payload.
    Inline(Vec<u8>),
    /// A durable slice: the runtime reads `len` bytes at `offset` from the
    /// named volume through its own volume layer.
    Slice {
        /// Volume name holding the module bytes.
        volume: String,
        /// Byte offset of the module within the volume.
        offset: u64,
        /// Byte length of the module.
        len: u64,
    },
}

/// Kind of event recorded in the activity log.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum ActivityKind {
    /// A process was started.
    ProcessStarted,
    /// A system guest was bootstrapped.
    GuestBootstrapped,
    /// A process was stopped.
    ProcessStopped,
    /// A process exited or trapped.
    ProcessExited,
    /// Metering was updated for a process.
    MeteringObserved,
}

/// Activity log event emitted by the kernel or runtime.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct ActivityEvent {
    /// Event kind.
    pub kind: ActivityKind,
    /// Process associated with the event, when any.
    pub process_id: Option<ProcessId>,
    /// Event message.
    pub message: String,
}

/// Log entry emitted by a guest.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct GuestLogEntry {
    /// Process associated with the entry, when known.
    pub process_id: Option<ProcessId>,
    /// Log level name.
    pub level: String,
    /// Log target name.
    pub target: String,
    /// Log message.
    pub message: String,
}

/// Informational tag for the intended use of an allocated shared memory region.
///
/// **Not** used for AAA decisions — a guest may spoof this value; the only effect
/// is cosmetic (e.g. discovery URI alias, UI icon).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum ResourceKind {
    /// Shared memory region for tracing log transport.
    LogChannel,
    /// Shared memory region for a live table.
    LiveTable,
    /// Shared memory region for RPC request/reply rings.
    RpcRing,
    /// Shared memory region for pub/sub topic.
    PubSubTopic,
    /// Shared memory region for network socket buffers.
    NetworkBuffer,
    /// Generic/unknown shared memory region.
    SharedMemory,
}

/// Host operation requested by a guest.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck(), attr(allow(missing_docs)))]
pub enum HostcallRequest {
    /// Bind a TCP listener.
    TcpBind {
        /// Address to bind to.
        address: String,
    },
    /// Connect to a TCP endpoint.
    TcpConnect {
        /// Address to connect to.
        address: String,
    },
    /// Bind a UDP socket.
    UdpBind {
        /// Address to bind to.
        address: String,
    },
    /// Open or create a durable volume under a serving tenant.
    VolumeOpen {
        /// Volume name.
        name: String,
        /// Tenant the volume is minted under. `None` mints under the calling
        /// process's own tenant; a different tenant requires cross-tenant
        /// allocation authority (root principal or tenant-scoped delegation).
        serving_tenant: Option<String>,
    },
    /// Append bytes to an open durable volume.
    VolumeAppend {
        /// Local volume handle id.
        local_id: LocalResourceId,
        /// Bytes to append.
        bytes: Vec<u8>,
    },
    /// Read bytes at an offset from an open durable volume.
    VolumeReadAt {
        /// Local volume handle id.
        local_id: LocalResourceId,
        /// Byte offset to read from.
        offset: u64,
        /// Maximum number of bytes to return.
        len: u32,
    },
    /// Return the current length of an open durable volume.
    VolumeLen {
        /// Local volume handle id.
        local_id: LocalResourceId,
    },
    /// List the volume names recorded under a serving tenant.
    VolumeList {
        /// Tenant whose volumes are listed (`None` is the calling process's own).
        serving_tenant: Option<String>,
    },
    /// Remove a durable volume under a serving tenant.
    VolumeRemove {
        /// Volume name to remove.
        name: String,
        /// Tenant the volume belongs to (`None` is the calling process's own).
        serving_tenant: Option<String>,
    },
    /// Flush an open durable volume's bytes to stable storage.
    VolumeSync {
        /// Local volume handle id.
        local_id: LocalResourceId,
    },
    /// Close a local durable volume handle.
    VolumeClose {
        /// Local volume handle id.
        local_id: LocalResourceId,
    },
    /// Return a tenant's resident durable-storage bytes. Gated by the
    /// `QuotaWrite` authority; the accounting guest is its sole caller.
    VolumeUsage {
        /// Tenant whose resident volume bytes are read.
        tenant: String,
    },
    /// Start a process.
    ProcessStart {
        /// Module bytes source: inline bytes or a durable slice.
        source: ModuleSource,
        /// Entrypoint export to invoke.
        entrypoint: String,
        /// Encoded entrypoint arguments.
        arguments: Vec<Vec<u8>>,
        /// Capability grants for the new process.
        grants: Vec<CapabilityGrant>,
        /// Tenant the child is spawned under. `None` inherits the parent's
        /// tenant; a tenant differing from the parent's own is admitted only
        /// for a parent holding a `DelegateGrants` grant whose scope admits
        /// the requested tenant — including a root parent, so cross-tenant
        /// authority is always a grant, never the parent's bootstrap tenant.
        tenant: Option<String>,
    },
    /// Stop a process.
    ProcessStop {
        /// Process id to stop.
        process_id: ProcessId,
    },
    /// Read activity log events.
    ActivityRead {
        /// Cursor offset to read from.
        cursor: usize,
    },
    /// Read metering for a process.
    MeteringRead {
        /// Process id to inspect.
        process_id: ProcessId,
    },
    /// Set a host-held quota counter for a tenant and resource class. Gated by
    /// the `QuotaWrite` capability (bootstrap-provisioned and non-conferable);
    /// the accounting guest is its sole author.
    QuotaSet {
        /// Tenant whose quota is authored.
        tenant: String,
        /// Resource class the quota caps.
        class: ResourceClass,
        /// Ceiling the quota enforces.
        limit: u64,
    },
    /// Remove a host-held quota counter for a tenant and resource class,
    /// restoring unrestricted allocation for that class. Gated by the
    /// `QuotaWrite` capability.
    QuotaClear {
        /// Tenant whose quota is cleared.
        tenant: String,
        /// Resource class the quota capped.
        class: ResourceClass,
    },
    /// Write a guest log entry.
    GuestLogWrite {
        /// Log entry to write.
        entry: GuestLogEntry,
    },
    /// Read guest log entries.
    GuestLogRead {
        /// Cursor offset to read from.
        cursor: usize,
        /// Optional process id filter.
        process_id: Option<ProcessId>,
    },
    /// Create a host-mediated connection queue, recorded by the host authority
    /// under the principal (serving) tenant. `None` mints the queue under
    /// the allocating process's own tenant; a tenant differing from the
    /// allocating process's own requires cross-tenant allocation authority
    /// (root principal or tenant-scoped delegation).
    HostQueueCreate {
        /// Tenant on whose behalf the queue is minted.
        serving_tenant: Option<String>,
    },
    /// Attach to an existing host-mediated connection queue.
    HostQueueAttach {
        /// Shared queue id to attach to.
        shared_id: SharedResourceId,
    },
    /// Send a value to a host-mediated connection queue.
    HostQueueSend {
        /// Local queue handle.
        local_id: LocalResourceId,
        /// Value to enqueue.
        value: u64,
        /// Opaque metadata payload attached to the handoff (e.g. the
        /// authenticated peer identity on a QUIC stream handoff). Empty when
        /// the sender has no metadata to attach.
        metadata: Vec<u8>,
    },
    /// Receive a value from a host-mediated connection queue.
    HostQueueRecv {
        /// Local queue handle.
        local_id: LocalResourceId,
    },
    /// Returns the calling process's own identity: its process id and
    /// tenant scope. Used by system guests (e.g. the bridge-server) to
    /// verify handoff identities against their own tenant.
    SelfInfo,
    /// Allocate a shared memory region mapped into guest linear memory.
    AllocRegion {
        /// Number of pages to allocate.
        pages: u32,
        /// Memory protection level.
        prot: RegionProt,
        /// Informational purpose tag for the allocated region.
        purpose: ResourceKind,
        /// Serving tenant the region is minted under. `None` (or a value
        /// equal to the allocating process's own tenant) mints under the
        /// process's own tenant; a different tenant requires tenant-scoped
        /// delegation and is denied without it.
        serving_tenant: Option<String>,
    },
    /// Returns the tenant identity assigned to another process, if any.
    /// Used by the discovery service to scope resolution to the caller's
    /// tenant.
    ProcessTenant {
        /// Process whose tenant to look up.
        process_id: ProcessId,
    },
    /// Returns whether a process holds a capability. Gated by the
    /// bootstrap-provisioned `ProcessProbe` capability rather than by any
    /// guest identity, so the runtime records which process may gate
    /// root-registration requests against another caller's grants. The probe
    /// discloses nothing beyond the capability asked for.
    ProcessCapability {
        /// Process whose grants to check.
        process_id: ProcessId,
        /// Capability to check.
        capability: Capability,
        /// Optional URI to evaluate the grant's selectors against. A
        /// `RouteClaim` names its exact URI through `ResourceSelector::Uri`,
        /// so the probe must be asked in that URI's scope.
        uri: Option<String>,
    },
    /// Returns the exact reserved URIs the host provisioned this process a
    /// `RouteClaim` for. The host knows the expectation independently of
    /// discovery publication, so reconciliation can report a claim that was
    /// never published without making discovery the accounting authority.
    ExpectedClaims {
        /// Process whose expected claims are listed.
        process_id: ProcessId,
    },
    /// Lists the live processes the host retains authority for. Gated by the
    /// bootstrap-provisioned `ProcessProbe` capability. A registry uses it to
    /// reconcile expected claims against published registrations, including
    /// processes that published nothing at all.
    ProcessList,
    /// Free a previously allocated shared memory region.
    FreeRegion {
        /// Region id to free.
        region_id: u64,
    },
    /// Attach an existing shared memory region into this guest's linear memory.
    AttachRegion {
        /// Region id to attach.
        region_id: u64,
        /// Optional reader slot index for per-page protection.
        reader_slot: Option<u32>,
        /// Memory protection level.
        prot: RegionProt,
    },
    /// Get the current wall-clock time as nanoseconds since UNIX epoch.
    TimeNow,
    /// Get the current monotonic time as nanoseconds since an arbitrary epoch.
    TimeMonotonic,
    /// Sleep for the specified number of milliseconds.
    Sleep {
        /// Duration to sleep in milliseconds.
        millis: u64,
    },
    /// Fill a buffer with cryptographically secure random bytes from the host.
    /// Used by TLS-terminating guests on wasm32 where no OS entropy source is
    /// available; the runtime enforces a maximum length to cap abuse.
    RandomBytes {
        /// Number of random bytes requested.
        len: u32,
    },
    /// Register a shared memory region as the guest's log channel with the kernel.
    GuestLogRegister {
        /// Shared region id of the log channel to register.
        shared_id: SharedResourceId,
    },
    /// Register the calling task as waiting on a generation advance of a
    /// host-writable shared-memory ring. The host will wake the task via
    /// the mailbox when the region's generation advances past `generation`.
    WaitRegister {
        /// Shared region id of the ring to watch.
        region_id: SharedResourceId,
        /// Generation value that the task is waiting to see advanced.
        generation: u64,
    },
    /// Notify the runtime that the calling guest advanced a shared-memory
    /// ring's generation. The runtime wakes any cross-guest waiters that
    /// registered interest via [`HostcallRequest::WaitRegister`]. Spurious
    /// or redundant notifications are benign: waiters re-check and re-park.
    GenerationAdvance {
        /// Shared region id of the ring that advanced.
        region_id: SharedResourceId,
        /// The new generation value written by the guest.
        generation: u64,
    },
    /// Authorise a named client process to attach a shared resource the
    /// caller owns or holds an explicit capability for. The host records the
    /// authorisation as the named peer's attach basis. No guest identity is
    /// privileged for this purpose, and exactly one resource and one client
    /// process are named.
    AuthoriseAttach {
        /// Class of the shared resource being handed off.
        resource_class: ResourceClass,
        /// Shared resource id being handed off.
        resource_id: SharedResourceId,
        /// Client process being authorised to attach the resource.
        client_process_id: ProcessId,
    },
    /// Declare a resource the caller owns attachable by resolution, with a
    /// declaration scope naming which callers the declaration admits. While a
    /// live declaration holds, the host accepts a resolve-recording naming a
    /// client process for that resource as the process's attach basis.
    DeclareAttachableByResolution {
        /// Class of the shared resource being declared.
        resource_class: ResourceClass,
        /// Shared resource id being declared.
        resource_id: SharedResourceId,
        /// Scope the declaration admits.
        scope: AttachDeclarationScope,
    },
    /// Record that a resolve performed by `client_process_id` returned the
    /// named shared resource. Accepted only while the resource carries a live
    /// attachable-by-resolution declaration whose scope admits the client; the
    /// recorded id gives the resolving client an authorisation basis for the
    /// corresponding cross-process attach hostcall (`HostQueueAttach` for
    /// queues, `AttachRegion` for shared regions). No guest identity is
    /// privileged for recording.
    RecordResolvedResourceFor {
        /// Class of the resolved shared resource.
        resource_class: ResourceClass,
        /// Shared resource id returned to the client by the resolve.
        resource_id: SharedResourceId,
        /// Process that performed the resolve.
        client_process_id: ProcessId,
    },
    /// Sign a tenant CA: generate a host-held tenant CA keypair and sign it
    /// via the online intermediate key. Returns the DER-encoded tenant CA
    /// certificate; the private key never leaves the host.
    SignTenantCa {
        /// Tenant whose CA is minted.
        tenant: String,
    },
    /// Commit the CA mint after its identity-registry record is durable,
    /// discarding the host-private snapshot of the displaced CA.
    CommitTenantCa {
        /// Tenant whose CA transaction is committed.
        tenant: String,
        /// Certificate returned by the corresponding `SignTenantCa` call.
        ca_cert_der: Vec<u8>,
    },
    /// Roll back a CA mint whose identity-registry append definitely failed,
    /// restoring the displaced CA (or removing a first, uncommitted CA).
    RollbackTenantCa {
        /// Tenant whose CA transaction is rolled back.
        tenant: String,
        /// Certificate returned by the corresponding `SignTenantCa` call.
        ca_cert_der: Vec<u8>,
    },
    /// Sign a user leaf certificate from a client-supplied SPKI via that
    /// tenant's CA key. Returns the DER-encoded leaf certificate.
    SignUserCert {
        /// Tenant whose CA key signs the leaf.
        tenant: String,
        /// DER-encoded SubjectPublicKeyInfo of the client-generated leaf key.
        spki_der: Vec<u8>,
    },
    /// Revoke a tenant CA: delete its key from the host keyring.
    RevokeCa {
        /// Tenant whose CA key is revoked.
        tenant: String,
    },
    /// Whether `process_id` owns the resource named by `resource_class` and
    /// `resource_id`.
    ///
    /// The host records ownership **synchronously**, inside the hostcall that
    /// mints the resource, so this answer is authoritative and cannot lag a
    /// resource the caller has only just created. A guest that reconstructs
    /// ownership from a pushed feed cannot make that guarantee, which is what
    /// makes this the right question to ask before admitting an action that
    /// depends on ownership (e.g. registering a route for a resource).
    ProcessOwnsResource {
        /// Process whose ownership to check.
        process_id: ProcessId,
        /// Class of the resource.
        resource_class: ResourceClass,
        /// Resource id within that class.
        resource_id: u64,
    },
    /// Resolve the platform's current discovery listener after a prior
    /// discovery session failed. The host records the caller's attach basis
    /// only when the replacement has declared itself attachable by resolution.
    DiscoveryListener,
}

/// Hostcall request paired with the guest task that initiated it.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct HostcallEnvelope {
    /// Requested host operation.
    pub request: HostcallRequest,
    /// Guest task to wake when asynchronous progress is available.
    pub task_id: Option<TaskId>,
}

/// Resource usage observation for a process.
#[derive(Debug, Clone, PartialEq, Eq, Default, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct MeteringObservation {
    /// Cumulative WebAssembly instructions executed by the process.
    pub cpu_instructions: u64,
    /// Memory usage in bytes.
    pub memory_bytes: u64,
    /// Network bandwidth usage in bytes.
    pub bandwidth_bytes: u64,
}

/// Output produced by a completed hostcall.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck(), attr(allow(missing_docs)))]
pub enum HostcallOutput {
    /// No output value.
    Empty,
    /// A local resource id.
    LocalId(LocalResourceId),
    /// A shared memory region descriptor.
    SharedRegion(SharedRegionDescriptor),

    /// A host-mediated connection queue descriptor.
    HostQueue(HostQueueDescriptor),
    /// A durable volume descriptor.
    Volume(VolumeDescriptor),
    /// A process descriptor.
    Process(ProcessDescriptor),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// Volume names.
    VolumeNames(Vec<String>),
    /// Shared resource id.
    SharedId(SharedResourceId),
    /// Activity log events.
    ActivityEvents(Vec<ActivityEvent>),
    /// Guest log entries.
    GuestLogEntries(Vec<GuestLogEntry>),
    /// Metering observation.
    Metering(MeteringObservation),
    /// Raw `u64` value.
    U64(u64),
    /// Connection queue entry with client process id, value, and metadata.
    ConnectionInfo {
        /// Process id of the connecting client.
        client_process_id: ProcessId,
        /// Enqueued value (e.g. session shared_id).
        value: u64,
        /// Opaque metadata payload attached by the sender (empty when absent).
        metadata: Vec<u8>,
    },
    /// A shared region allocation result.
    RegionAlloc(RegionAllocation),
    /// A shared region attachment result.
    RegionAttach(RegionAttachment),
    /// Cryptographically secure random bytes generated by the host.
    RandomBytes(Vec<u8>),
    /// The calling process's own identity.
    SelfInfo {
        /// Process id of the caller.
        process_id: ProcessId,
        /// Tenant scope of the caller, if provisioned.
        tenant: Option<String>,
    },
    /// A tenant identity for [`HostcallRequest::ProcessTenant`].
    Tenant(Option<String>),
    /// Authoritative ownership and mint-time tenant for
    /// [`HostcallRequest::ProcessOwnsResource`].
    ResourceAuthority(ResourceAuthority),
    /// Exact reserved URIs the host expects the process to publish
    /// ([`HostcallRequest::ExpectedClaims`]).
    Claims(Vec<String>),
    /// Live processes the host retains authority for
    /// ([`HostcallRequest::ProcessList`]).
    Processes(Vec<ProcessId>),
    /// A DER-encoded X.509 public certificate returned by a signing hostcall
    /// ([`HostcallRequest::SignTenantCa`] or [`HostcallRequest::SignUserCert`]).
    /// Carries no private-key material.
    Certificate(Vec<u8>),
}

/// Current completion state of a hostcall operation.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck(), attr(allow(missing_docs)))]
pub enum CompletionState {
    /// Hostcall completed successfully.
    Ready(HostcallOutput),
    /// Hostcall is still pending.
    Pending {
        /// Operation id to poll later.
        operation_id: OperationId,
    },
    /// Hostcall failed.
    Failed(AbiError),
}

/// Error returned by rkyv encoding or decoding helpers.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RkyvError {
    #[error("encode error: {0}")]
    /// Encoding failed.
    Encode(String),
    #[error("decode error: {0}")]
    /// Decoding failed.
    Decode(String),
}

/// Error returned when a hostcall request preamble is missing or incompatible.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostcallPreambleError {
    /// The request is too short to carry the fixed preamble.
    #[error("hostcall request is missing the ABI preamble")]
    Missing,
    /// The leading magic value is not the Selium hostcall magic.
    #[error("hostcall request has invalid magic {found:#010x}")]
    BadMagic {
        /// Magic value supplied by the sender.
        found: u32,
    },
    /// The sender's ABI version is not the host's supported version.
    #[error("host ABI version {host} is incompatible with guest ABI version {guest}")]
    VersionMismatch {
        /// Version supported by the host.
        host: u32,
        /// Version declared by the guest request.
        guest: u32,
    },
}

impl HostcallPreambleError {
    /// The structured ABI error this rejection is reported as.
    ///
    /// The message names both ABI versions, so a refused guest can report why
    /// it was refused (and against which host version) without reading the
    /// host's log.
    pub fn to_abi_error(&self) -> AbiError {
        AbiError::new(AbiErrorCode::MalformedPayload, self.to_string())
    }
}

/// Parsed components of a network endpoint URI (`tcp://` or `udp://`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkEndpoint {
    /// Scheme: `"tcp"` or `"udp"`.
    pub scheme: String,
    /// Host: lowercase, trailing dot stripped, IPv6 bracketed.
    pub host: String,
    /// Port: explicit numeric string.
    pub port: String,
}

impl EntrypointMetadata {
    /// Creates entrypoint metadata with the supplied export name.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl LocalityScope {
    /// Returns whether this scope admits the actual locality.
    pub fn matches(&self, actual: &LocalityScope) -> bool {
        match self {
            Self::Any => true,
            Self::Cluster => matches!(actual, LocalityScope::Cluster | LocalityScope::Host(_)),
            Self::Host(expected) => {
                matches!(actual, LocalityScope::Host(actual) if actual == expected)
            }
        }
    }
}

impl ResourceClass {
    /// Returns the lowercased typed URI segment for this class, drawn from the
    /// closed segment vocabulary (`proc`, `region`, `queue`, …). This is the
    /// single mapping between a resource class and the `<type>` segment of a
    /// `sel://<tenant>/<type>/<id>` URI.
    pub fn uri_segment(&self) -> &'static str {
        match self {
            Self::SharedRegion => "region",
            Self::SharedMapping => "mapping",
            Self::Signal => "signal",
            Self::TcpListener => "listener",
            Self::TcpStream => "stream",
            Self::UdpSocket => "socket",
            Self::Volume => "volume",
            Self::Process => "proc",
            Self::ActivityLog => "activity",
            Self::MeteringStream => "metering",
            Self::GuestLog => "guest-log",
            Self::HostQueue => "queue",
            Self::Cpu => "cpu",
        }
    }

    /// Returns the class named by a typed URI segment, if the segment is part
    /// of the closed vocabulary. Inverse of [`Self::uri_segment`], used to
    /// reserve class nouns so a leaf alias cannot shadow a type segment.
    pub fn from_uri_segment(segment: &str) -> Option<Self> {
        match segment {
            "region" => Some(Self::SharedRegion),
            "mapping" => Some(Self::SharedMapping),
            "signal" => Some(Self::Signal),
            "listener" => Some(Self::TcpListener),
            "stream" => Some(Self::TcpStream),
            "socket" => Some(Self::UdpSocket),
            "volume" => Some(Self::Volume),
            "proc" => Some(Self::Process),
            "activity" => Some(Self::ActivityLog),
            "metering" => Some(Self::MeteringStream),
            "guest-log" => Some(Self::GuestLog),
            "queue" => Some(Self::HostQueue),
            "cpu" => Some(Self::Cpu),
            _ => None,
        }
    }
}

impl Default for ScopeContext {
    fn default() -> Self {
        Self {
            tenant: None,
            uri: None,
            locality: LocalityScope::Any,
            resource_class: None,
            resource_id: None,
        }
    }
}

impl Namespace {
    /// Returns whether this namespace admits a scope context whose tenant is
    /// `tenant`: `Root` admits `None` only; `Tenant(t)` admits `Some(t)` exactly.
    pub fn matches_tenant(&self, tenant: Option<&str>) -> bool {
        match self {
            Self::Root => tenant.is_none(),
            Self::Tenant(expected) => tenant == Some(expected.as_str()),
        }
    }
}

impl ResourceSelector {
    /// Returns whether the runtime can evaluate this selector against a `ScopeContext`.
    ///
    /// The enforcement matrix is:
    /// - `Tenant`, `Namespace`, `Locality`, `ResourceClass`, `ExplicitResource`,
    ///   `Uri`, `Children`: evaluatable.
    /// - `UriPrefix`: evaluatable only when the same grant also carries a
    ///   resource-class selector the prefix applies to: a network class
    ///   (`TcpListener`, `TcpStream`, or `UdpSocket`) for endpoint URIs, or
    ///   `Volume` for volume names (and `sel://` registry URIs).
    pub fn is_evaluatable(&self, grant_selectors: &[ResourceSelector]) -> bool {
        match self {
            Self::UriPrefix(_) => grant_selectors.iter().any(|s| {
                matches!(
                    s,
                    Self::ResourceClass(ResourceClass::TcpListener)
                        | Self::ResourceClass(ResourceClass::TcpStream)
                        | Self::ResourceClass(ResourceClass::UdpSocket)
                        | Self::ResourceClass(ResourceClass::Volume)
                )
            }),
            Self::Tenant(_)
            | Self::Namespace(_)
            | Self::Locality(_)
            | Self::ResourceClass(_)
            | Self::ExplicitResource(_)
            | Self::Uri(_)
            | Self::Children => true,
        }
    }

    /// Returns whether this selector matches the supplied scope context.
    ///
    /// `Children` is not evaluated here — it requires process-tree knowledge
    /// only available to the runtime, so it returns `false` by default and the
    /// runtime handles it specially in `Runtime::authorises`.
    ///
    /// For `UriPrefix` on `tcp://` / `udp://` URIs, matching is component-aware
    /// (scheme exact; host exact or `*.`-label-boundary wildcard; port exact,
    /// list, or `*`). For `sel://` registry URIs, matching is path-segment
    /// aware. Other non-network URIs keep plain `starts_with` semantics.
    ///
    /// `Uri` matches one exact canonical registry URI after normalising both
    /// sides; it never acts as a prefix.
    pub fn matches(&self, context: &ScopeContext) -> bool {
        match self {
            Self::Tenant(expected) => context.tenant.as_ref() == Some(expected),
            Self::Namespace(namespace) => namespace.matches_tenant(context.tenant.as_deref()),
            Self::Uri(expected) => context
                .uri
                .as_ref()
                .is_some_and(|context_uri| canonical_uri(expected) == canonical_uri(context_uri)),
            Self::UriPrefix(prefix) => {
                let context_uri = context.uri.as_ref();
                match context_uri {
                    Some(ctx_uri) => {
                        // Component-aware matching for network URIs.
                        if let (Some(grant_ep), Some(ctx_ep)) = (
                            NetworkEndpoint::parse(prefix),
                            NetworkEndpoint::parse(ctx_uri),
                        ) {
                            NetworkEndpoint::prefix_matches(&grant_ep, &ctx_ep)
                        } else if uri::parse_sel(prefix).is_some()
                            || uri::parse_sel(ctx_uri).is_some()
                        {
                            sel_prefix_matches(prefix, ctx_uri)
                        } else {
                            // Plain string prefix for other non-network URIs.
                            ctx_uri.starts_with(prefix)
                        }
                    }
                    None => false,
                }
            }
            Self::Locality(expected) => expected.matches(&context.locality),
            Self::ResourceClass(expected) => context.resource_class.as_ref() == Some(expected),
            Self::ExplicitResource(expected) => context.resource_id == Some(*expected),
            // Handled by the runtime with process-tree access.
            Self::Children => false,
        }
    }
}

impl CapabilityGrant {
    /// Creates a capability grant with the supplied selectors.
    pub fn new(capability: Capability, selectors: Vec<ResourceSelector>) -> Self {
        Self {
            capability,
            selectors,
        }
    }

    /// Returns whether all selectors admit the supplied context.
    pub fn allows(&self, context: &ScopeContext) -> bool {
        self.selectors
            .iter()
            .all(|selector| selector.matches(context))
    }
}

impl AbiError {
    /// Creates an ABI error with the supplied code and message.
    pub fn new(code: AbiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl NetworkEndpoint {
    /// Parse a network endpoint URI like `tcp://127.0.0.1:8080` or `udp://[::1]:53`.
    pub fn parse(uri: &str) -> Option<Self> {
        let (scheme, rest) = uri.split_once("://")?;
        if scheme != "tcp" && scheme != "udp" {
            return None;
        }
        // Port is after the last colon.
        let (host_part, port) = rest.rsplit_once(':')?;

        // Validate port: empty rejected; comma-separated digits accepted; bare * accepted.
        if port.is_empty() {
            return None;
        }
        if port == "*" {
            // ok
        } else if !port
            .chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == ' ')
        {
            return None;
        }

        let host = host_part.to_lowercase();
        let host = host.strip_suffix('.').unwrap_or(&host).to_string();

        // Host must be non-empty.
        if host.is_empty() {
            return None;
        }

        Some(Self {
            scheme: scheme.to_string(),
            host,
            port: port.to_string(),
        })
    }

    /// Returns whether `grant` (the grant-side endpoint) matches `context`
    /// (the runtime-side endpoint) using component-aware semantics.
    pub fn prefix_matches(grant: &NetworkEndpoint, context: &NetworkEndpoint) -> bool {
        // Scheme must match exactly.
        if grant.scheme != context.scheme {
            return false;
        }
        // Host: exact or wildcard label boundary.
        if !Self::host_matches(&grant.host, &context.host) {
            return false;
        }
        // Port: exact, *, or comma-separated list.
        Self::port_matches(&grant.port, &context.port)
    }

    fn host_matches(grant_host: &str, context_host: &str) -> bool {
        if grant_host == context_host {
            return true;
        }
        // Wildcard: `*.example.com` matches `foo.example.com`, `bar.foo.example.com`, etc.
        if let Some(suffix) = grant_host.strip_prefix("*.") {
            // Must match at a label boundary and the context host must be longer.
            if context_host.ends_with(suffix) && context_host.len() > suffix.len() {
                // Ensure the character before the suffix is a dot (label boundary).
                let dot_pos = context_host.len() - suffix.len() - 1;
                return context_host.as_bytes().get(dot_pos) == Some(&b'.');
            }
        }
        false
    }

    fn port_matches(grant_port: &str, context_port: &str) -> bool {
        if grant_port == "*" {
            return true;
        }
        // Comma-separated list.
        grant_port.split(',').any(|p| p.trim() == context_port)
    }
}

impl<T> RkyvEncode for T where
    T: Archive + for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RancorError>>
{
}

/// Encodes a hostcall request with the fixed ABI magic/version preamble.
///
/// The preamble precedes the rkyv envelope so the host can validate
/// compatibility before attempting to interpret enum variant layouts.
pub fn encode_hostcall_request(envelope: &HostcallEnvelope) -> Result<Vec<u8>, RkyvError> {
    let payload = encode_rkyv(envelope)?;
    let mut encoded = Vec::with_capacity(HOSTCALL_PREAMBLE_SIZE + payload.len());
    encoded.extend_from_slice(&HOSTCALL_MAGIC.to_le_bytes());
    encoded.extend_from_slice(&SELIUM_ABI_VERSION.to_le_bytes());
    encoded.extend_from_slice(&payload);
    Ok(encoded)
}

/// Validates a hostcall request preamble, returning the rkyv payload.
///
/// Missing preambles, wrong magic, and mismatched ABI versions are rejected
/// before any rkyv decoding; a version mismatch reports both versions.
pub fn hostcall_request_payload(bytes: &[u8]) -> Result<&[u8], HostcallPreambleError> {
    let magic_bytes = bytes
        .get(..4)
        .ok_or(HostcallPreambleError::Missing)?
        .try_into()
        .map_err(|_error| HostcallPreambleError::Missing)?;
    let magic = u32::from_le_bytes(magic_bytes);
    if magic != HOSTCALL_MAGIC {
        return Err(HostcallPreambleError::BadMagic { found: magic });
    }
    let version_bytes = bytes
        .get(4..HOSTCALL_PREAMBLE_SIZE)
        .ok_or(HostcallPreambleError::Missing)?
        .try_into()
        .map_err(|_error| HostcallPreambleError::Missing)?;
    let guest = u32::from_le_bytes(version_bytes);
    if guest != SELIUM_ABI_VERSION {
        return Err(HostcallPreambleError::VersionMismatch {
            host: SELIUM_ABI_VERSION,
            guest,
        });
    }
    bytes
        .get(HOSTCALL_PREAMBLE_SIZE..)
        .ok_or(HostcallPreambleError::Missing)
}

/// Decodes an rkyv value from bytes after validation.
pub fn decode_rkyv<T>(bytes: &[u8]) -> Result<T, RkyvError>
where
    T: Archive + Sized,
    for<'a> T::Archived: Deserialize<T, HighDeserializer<RancorError>>
        + rkyv::bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    rkyv::from_bytes::<T, RancorError>(bytes).map_err(|error| RkyvError::Decode(error.to_string()))
}

/// Removes a length prefix from a framed payload and validates the frame length.
pub fn deframe_bytes(payload: &[u8]) -> Result<&[u8], AbiError> {
    let prefix = payload.get(..4).ok_or_else(|| {
        AbiError::new(
            AbiErrorCode::MalformedPayload,
            "missing frame length prefix",
        )
    })?;
    let len = u32::from_le_bytes(prefix.try_into().map_err(|_error| {
        AbiError::new(
            AbiErrorCode::MalformedPayload,
            "invalid frame length prefix",
        )
    })?) as usize;
    let frame = payload.get(4..4 + len).ok_or_else(|| {
        AbiError::new(
            AbiErrorCode::MalformedPayload,
            "frame length exceeds buffer",
        )
    })?;
    if payload.len() != len + 4 {
        return Err(AbiError::new(
            AbiErrorCode::MalformedPayload,
            "frame contains trailing bytes",
        ));
    }
    Ok(frame)
}

/// Encodes a value to rkyv bytes.
pub fn encode_rkyv<T>(value: &T) -> Result<Vec<u8>, RkyvError>
where
    T: RkyvEncode,
{
    rkyv::to_bytes::<RancorError>(value)
        .map(|bytes| bytes.into_vec())
        .map_err(|error| RkyvError::Encode(error.to_string()))
}

/// Prefixes a payload with its little-endian `u32` length.
pub fn frame_bytes(payload: &[u8]) -> Result<Vec<u8>, AbiError> {
    let len = u32::try_from(payload.len()).map_err(|_error| {
        AbiError::new(
            AbiErrorCode::MalformedPayload,
            "frame payload length exceeds u32",
        )
    })?;
    let mut framed = Vec::with_capacity(payload.len() + 4);
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(payload);
    Ok(framed)
}

/// Packs a hostcall status and value into one `u64` ABI return value.
pub fn pack_hostcall_status(status: u32, value: u32) -> u64 {
    ((status as u64) << 32) | value as u64
}

/// Unpacks a hostcall status and value from one `u64` ABI return value.
pub fn unpack_hostcall_status(encoded: u64) -> (u32, u32) {
    ((encoded >> 32) as u32, encoded as u32)
}

/// Normalises a registry URI to its canonical internal `sel://` spelling:
/// trailing separators removed, and wire names projected to their internal
/// path. Non-`sel` values (network endpoints, volume names) are returned
/// unchanged.
fn canonical_uri(raw: &str) -> String {
    if let Some((tenant, path)) = uri::parse_sel(raw) {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return format!("{}{tenant}/", uri::SEL_PREFIX);
        }
        return format!("{}{tenant}/{path}", uri::SEL_PREFIX);
    }
    if raw.contains("://") {
        return raw.to_string();
    }
    match uri::resolve_wire_name(raw, &uri::DomainTable::new()) {
        Some((tenant, path)) if path.is_empty() => format!("{}{tenant}/", uri::SEL_PREFIX),
        Some((tenant, path)) => format!("{}{tenant}/{}", uri::SEL_PREFIX, path.join("/")),
        None => raw.to_string(),
    }
}

/// Path-segment-aware prefix matching for `sel://` registry URIs: a prefix
/// matches at a segment boundary, so `sel:///control` does not match
/// `sel:///control-plane`.
fn sel_prefix_matches(prefix: &str, context: &str) -> bool {
    let prefix = canonical_uri(prefix);
    let context = canonical_uri(context);
    if context == prefix {
        return true;
    }
    match context.strip_prefix(&prefix) {
        Some(rest) => prefix.ends_with('/') || rest.starts_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Namespace` round-trips through the rkyv codec in both variants; the
    /// selector vocabulary that uses it (and the delegation fence) shares the
    /// same ABI codec.
    #[test]
    fn namespace_round_trips_through_rkyv() {
        for namespace in [Namespace::Root, Namespace::Tenant("acme".to_string())] {
            let encoded = encode_rkyv(&namespace).expect("encode namespace");
            let decoded: Namespace = decode_rkyv(&encoded).expect("decode namespace");
            assert_eq!(decoded, namespace);
        }
    }

    // scenario: selium-abi/regionprot-enum/regionprot-serialisation
    #[test]
    fn region_prot_serialises_to_stable_discriminants() {
        // The wire form is the discriminant: `RegionProt` travels inside
        // `AllocRegion`/`AttachRegion` envelopes, so a reordering would
        // silently invert a read-only request into a writable one.
        assert_eq!(RegionProt::ReadOnly as u8, 0);
        assert_eq!(RegionProt::ReadWrite as u8, 1);

        for prot in [RegionProt::ReadOnly, RegionProt::ReadWrite] {
            let encoded = encode_rkyv(&prot).expect("encode region protection");
            let decoded: RegionProt = decode_rkyv(&encoded).expect("decode region protection");
            assert_eq!(decoded, prot);
        }
    }

    /// `ResourceSelector::Namespace` matches hierarchically: a tenant selector
    /// admits only its own tenant, and the root selector admits only a root
    /// (tenant-less) scope context.
    // scenario: capability-enforcement/namespace-selector-hierarchy/tenant-selector-admits-only-its-tenant
    // scenario: capability-enforcement/namespace-selector-hierarchy/root-selector-admits-the-root-namespace
    #[test]
    fn namespace_selector_matches_exactly_and_root() {
        let acme = ResourceSelector::Namespace(Namespace::Tenant("acme".to_string()));
        let root = ResourceSelector::Namespace(Namespace::Root);

        let acme_context = ScopeContext {
            tenant: Some("acme".to_string()),
            ..ScopeContext::default()
        };
        let root_context = ScopeContext {
            tenant: None,
            ..ScopeContext::default()
        };
        let beta_context = ScopeContext {
            tenant: Some("beta".to_string()),
            ..ScopeContext::default()
        };

        // Exact-tenant admission.
        assert!(acme.matches(&acme_context));
        assert!(!acme.matches(&beta_context));
        assert!(
            !acme.matches(&root_context),
            "tenant selector must not admit root"
        );

        // Root admission.
        assert!(root.matches(&root_context));
        assert!(
            !root.matches(&acme_context),
            "root selector must not admit a tenant"
        );
    }

    #[test]
    fn scope_grants_use_intersection_semantics() {
        let grant = CapabilityGrant::new(
            Capability::ProcessLifecycle,
            vec![
                ResourceSelector::Tenant("acme".to_string()),
                ResourceSelector::UriPrefix("sel://acme/payments/".to_string()),
            ],
        );
        let allowed = ScopeContext {
            tenant: Some("acme".to_string()),
            uri: Some("sel://acme/payments/worker".to_string()),
            ..ScopeContext::default()
        };
        let denied = ScopeContext {
            tenant: Some("acme".to_string()),
            uri: Some("sel://acme/other/worker".to_string()),
            ..ScopeContext::default()
        };

        assert!(grant.allows(&allowed));
        assert!(!grant.allows(&denied));
    }

    // scenario: selium-abi/host-guest-boundary-only/hostcall-contract-is-unchanged
    #[test]
    fn encode_and_decode_round_trip() {
        let request = HostcallEnvelope {
            request: HostcallRequest::AllocRegion {
                pages: 16,
                prot: RegionProt::ReadWrite,
                purpose: ResourceKind::SharedMemory,
                serving_tenant: None,
            },
            task_id: Some(42),
        };

        let encoded = encode_rkyv(&request).expect("encode request");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode request");
        assert_eq!(decoded, request);
    }

    #[test]
    fn explicit_error_codes_round_trip() {
        let error = AbiError::new(AbiErrorCode::DetachedResource, "mapping detached");
        let encoded = encode_rkyv(&error).expect("encode error");
        let decoded: AbiError = decode_rkyv(&encoded).expect("decode error");

        assert_eq!(decoded.code, AbiErrorCode::DetachedResource);
        assert_eq!(decoded.message, "mapping detached");
    }

    #[test]
    fn quota_exceeded_error_code_round_trip() {
        // A quota denial is a distinct code: guests must be able to tell
        // "you lack the capability" apart from "your tenant hit its ceiling".
        let error = AbiError::new(
            AbiErrorCode::QuotaExceeded,
            "quota exceeded for tenant acme on SharedRegion",
        );
        let encoded = encode_rkyv(&error).expect("encode error");
        let decoded: AbiError = decode_rkyv(&encoded).expect("decode error");

        assert_eq!(decoded.code, AbiErrorCode::QuotaExceeded);
        assert!(decoded.message.contains("acme"));
    }

    #[test]
    fn frame_and_deframe_bytes_round_trip() {
        let framed = frame_bytes(b"hello").expect("frame bytes");
        let deframed = deframe_bytes(&framed).expect("deframe bytes");
        assert_eq!(deframed, b"hello");
    }

    // scenario: selium-abi/hostqueuecreate-serving-tenant/serving-tenant-round-trips
    #[test]
    fn host_queue_create_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::HostQueueCreate {
                serving_tenant: Some("acme".to_string()),
            },
            task_id: Some(1),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    // scenario: selium-abi/host-and-guest-negotiate-abi-compatibility/current-versions-match
    #[test]
    fn hostcall_preamble_accepts_the_matching_abi_version() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::SelfInfo,
            task_id: Some(5),
        };
        let encoded = encode_hostcall_request(&envelope).expect("encode with preamble");
        assert_eq!(
            encoded.get(..4),
            Some(HOSTCALL_MAGIC.to_le_bytes().as_slice())
        );
        assert_eq!(
            encoded.get(4..HOSTCALL_PREAMBLE_SIZE),
            Some(SELIUM_ABI_VERSION.to_le_bytes().as_slice())
        );
        let payload = hostcall_request_payload(&encoded).expect("matching version");
        let decoded: HostcallEnvelope = decode_rkyv(payload).expect("decode payload");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn hostcall_preamble_rejects_missing_or_truncated_headers() {
        assert_eq!(
            hostcall_request_payload(&[]),
            Err(HostcallPreambleError::Missing)
        );
        assert_eq!(
            hostcall_request_payload(&HOSTCALL_MAGIC.to_le_bytes()),
            Err(HostcallPreambleError::Missing),
            "a magic without a version is still missing the preamble"
        );
    }

    #[test]
    fn hostcall_preamble_rejects_bad_magic() {
        let mut bytes = HOSTCALL_MAGIC.to_le_bytes().to_vec();
        bytes.extend_from_slice(&SELIUM_ABI_VERSION.to_le_bytes());
        bytes[0] ^= 0xff;
        assert_eq!(
            hostcall_request_payload(&bytes),
            Err(HostcallPreambleError::BadMagic {
                found: u32::from_le_bytes(
                    bytes
                        .get(..4)
                        .and_then(|slice| slice.try_into().ok())
                        .expect("four bytes")
                )
            })
        );
    }

    // scenario: selium-abi/host-and-guest-negotiate-abi-compatibility/guest-abi-is-incompatible
    #[test]
    fn hostcall_preamble_reports_both_versions_on_mismatch() {
        for guest in [0, SELIUM_ABI_VERSION + 1, u32::MAX] {
            let mut bytes = HOSTCALL_MAGIC.to_le_bytes().to_vec();
            bytes.extend_from_slice(&guest.to_le_bytes());
            let error = hostcall_request_payload(&bytes).expect_err("version must be rejected");
            assert_eq!(
                error,
                HostcallPreambleError::VersionMismatch {
                    host: SELIUM_ABI_VERSION,
                    guest
                }
            );
            let message = error.to_string();
            assert!(
                message.contains(&SELIUM_ABI_VERSION.to_string())
                    && message.contains(&guest.to_string()),
                "the error must name both versions: {message}"
            );
        }
    }

    #[test]
    fn hostcall_preamble_rejection_is_a_structured_abi_error() {
        let error = HostcallPreambleError::VersionMismatch {
            host: SELIUM_ABI_VERSION,
            guest: SELIUM_ABI_VERSION + 7,
        };
        let abi = error.to_abi_error();
        assert_eq!(abi.code, AbiErrorCode::MalformedPayload);
        assert!(
            abi.message.contains(&SELIUM_ABI_VERSION.to_string())
                && abi.message.contains(&(SELIUM_ABI_VERSION + 7).to_string()),
            "the guest-visible error names both versions: {}",
            abi.message
        );
        assert_eq!(
            HostcallPreambleError::Missing.to_abi_error().code,
            AbiErrorCode::MalformedPayload
        );
    }

    #[test]
    fn host_queue_attach_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::HostQueueAttach { shared_id: 42 },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn host_queue_send_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::HostQueueSend {
                local_id: 7,
                value: 99,
                metadata: Vec::new(),
            },
            task_id: Some(3),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn host_queue_send_metadata_round_trip() {
        let metadata: Vec<u8> = vec![0x01, 0x02, 0x03, 0x04];
        let envelope = HostcallEnvelope {
            request: HostcallRequest::HostQueueSend {
                local_id: 7,
                value: 99,
                metadata: metadata.clone(),
            },
            task_id: Some(3),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        let HostcallRequest::HostQueueSend {
            metadata: decoded_metadata,
            ..
        } = decoded.request
        else {
            panic!("expected HostQueueSend");
        };
        assert_eq!(decoded_metadata, metadata);
    }

    #[test]
    fn capability_delegate_grants_round_trip() {
        for capability in [
            Capability::DelegateGrants,
            Capability::HostQueue,
            Capability::MintCertificate,
            Capability::ProcessProbe,
        ] {
            let encoded = encode_rkyv(&capability).expect("encode");
            let decoded: Capability = decode_rkyv(&encoded).expect("decode");
            assert_eq!(decoded, capability);
        }
    }

    #[test]
    fn capability_mint_certificate_round_trip() {
        // `MintCertificate` is rkyv-encodable like every other variant, so it
        // carries the same ABI stability guarantee as the rest of the enum.
        let capability = Capability::MintCertificate;
        let encoded = encode_rkyv(&capability).expect("encode");
        let decoded: Capability = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, Capability::MintCertificate);
    }

    #[test]
    fn capability_quota_write_round_trip() {
        // `QuotaWrite` is rkyv-encodable like every other variant: the quota
        // hostcalls it gates are gated by an ordinary capability grant that
        // travels over the same ABI as the rest of the capability set.
        let capability = Capability::QuotaWrite;
        let encoded = encode_rkyv(&capability).expect("encode");
        let decoded: Capability = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, Capability::QuotaWrite);
    }

    #[test]
    fn host_queue_recv_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::HostQueueRecv { local_id: 7 },
            task_id: Some(4),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn connection_info_output_round_trip() {
        let output = HostcallOutput::ConnectionInfo {
            client_process_id: 123,
            value: 456,
            metadata: Vec::new(),
        };
        let encoded = encode_rkyv(&output).expect("encode");
        let decoded: HostcallOutput = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, output);
    }

    #[test]
    fn connection_info_metadata_round_trip() {
        let output = HostcallOutput::ConnectionInfo {
            client_process_id: 123,
            value: 456,
            metadata: b"tenant=acme,fp=abcd".to_vec(),
        };
        let encoded = encode_rkyv(&output).expect("encode");
        let decoded: HostcallOutput = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, output);
    }

    #[test]
    fn volume_hostcalls_round_trip() {
        let requests = vec![
            HostcallRequest::VolumeOpen {
                name: "orders".to_string(),
                serving_tenant: Some("acme".to_string()),
            },
            HostcallRequest::VolumeAppend {
                local_id: 1,
                bytes: vec![1, 2, 3],
            },
            HostcallRequest::VolumeReadAt {
                local_id: 1,
                offset: 4,
                len: 8,
            },
            HostcallRequest::VolumeLen { local_id: 1 },
            HostcallRequest::VolumeList {
                serving_tenant: None,
            },
            HostcallRequest::VolumeRemove {
                name: "orders".to_string(),
                serving_tenant: Some("acme".to_string()),
            },
            HostcallRequest::VolumeSync { local_id: 1 },
            HostcallRequest::VolumeClose { local_id: 1 },
            HostcallRequest::VolumeUsage {
                tenant: "acme".to_string(),
            },
        ];
        for request in requests {
            let envelope = HostcallEnvelope {
                request,
                task_id: Some(9),
            };
            let encoded = encode_rkyv(&envelope).expect("encode");
            let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
            assert_eq!(decoded, envelope);
        }

        for output in [
            HostcallOutput::Volume(VolumeDescriptor {
                local_id: 3,
                len: 64,
            }),
            HostcallOutput::VolumeNames(vec!["a".to_string(), "b".to_string()]),
        ] {
            let encoded = encode_rkyv(&output).expect("encode");
            let decoded: HostcallOutput = decode_rkyv(&encoded).expect("decode");
            assert_eq!(decoded, output);
        }
    }

    // scenario: selium-abi/volume-resource-class/log-and-blob-classes-are-absent
    #[test]
    fn storage_classes_leave_the_abi() {
        assert_eq!(ResourceClass::Volume.uri_segment(), "volume");
        assert_eq!(
            ResourceClass::from_uri_segment("volume"),
            Some(ResourceClass::Volume)
        );
        assert_eq!(ResourceClass::from_uri_segment("log"), None);
        assert_eq!(ResourceClass::from_uri_segment("blob"), None);
    }
    // scenario: selium-abi/process-metering-observation/observation-fields-round-trip
    // scenario: selium-abi/process-metering-observation/cpu-microseconds-absent
    // scenario: selium-abi/process-metering-observation/storage-is-not-a-per-process-field

    #[test]
    fn metering_observation_round_trip() {
        // The observation carries the cumulative instruction count and the
        // memory/bandwidth gauges. Neither the former microsecond CPU field
        // nor a per-process storage field exists on the type: tenant storage
        // is read through the volume-usage hostcall.
        let output = HostcallOutput::Metering(MeteringObservation {
            cpu_instructions: 123_456,
            memory_bytes: 4_096,
            bandwidth_bytes: 8_192,
        });
        let encoded = encode_rkyv(&output).expect("encode");
        let decoded: HostcallOutput = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, output);

        let HostcallOutput::Metering(observation) = decoded else {
            panic!("expected metering observation");
        };
        assert_eq!(observation.cpu_instructions, 123_456);
        assert_eq!(observation.memory_bytes, 4_096);
        assert_eq!(observation.bandwidth_bytes, 8_192);
    }

    #[test]
    fn self_info_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::SelfInfo,
            task_id: Some(3),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);

        let output = HostcallOutput::SelfInfo {
            process_id: 42,
            tenant: Some("acme".to_string()),
        };
        let encoded = encode_rkyv(&output).expect("encode");
        let decoded: HostcallOutput = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, output);
    }

    // scenario: selium-abi/certificate-signing-hostcall-variants/signtenantca-round-trips
    #[test]
    fn sign_tenant_ca_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::SignTenantCa {
                tenant: "acme".to_string(),
            },
            task_id: Some(1),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn tenant_ca_transaction_round_trips() {
        for request in [
            HostcallRequest::CommitTenantCa {
                tenant: "acme".to_string(),
                ca_cert_der: vec![0x30, 0x01],
            },
            HostcallRequest::RollbackTenantCa {
                tenant: "acme".to_string(),
                ca_cert_der: vec![0x30, 0x02],
            },
        ] {
            let envelope = HostcallEnvelope {
                request,
                task_id: None,
            };
            let encoded = encode_rkyv(&envelope).expect("encode");
            let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
            assert_eq!(decoded, envelope);
        }
    }

    // scenario: selium-abi/certificate-signing-hostcall-variants/signusercert-round-trips
    #[test]
    fn sign_user_cert_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::SignUserCert {
                tenant: "acme".to_string(),
                spki_der: vec![0x30, 0x82, 0x01, 0x02],
            },
            task_id: Some(2),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn revoke_ca_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::RevokeCa {
                tenant: "acme".to_string(),
            },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn certificate_output_round_trip() {
        let output = HostcallOutput::Certificate(vec![0x30, 0x03, 0x02, 0x01]);
        let encoded = encode_rkyv(&output).expect("encode");
        let decoded: HostcallOutput = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, output);
    }

    #[test]
    fn authorise_attach_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::AuthoriseAttach {
                resource_class: ResourceClass::SharedRegion,
                resource_id: 7,
                client_process_id: 42,
            },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn declare_attachable_by_resolution_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::DeclareAttachableByResolution {
                resource_class: ResourceClass::HostQueue,
                resource_id: 9,
                scope: AttachDeclarationScope::Tenant("acme".to_string()),
            },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn record_resolved_resource_for_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::RecordResolvedResourceFor {
                resource_class: ResourceClass::SharedRegion,
                resource_id: 7,
                client_process_id: 42,
            },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn a_durable_slice_module_source_round_trips() {
        // The slice variant carries the whole module reference (volume, offset,
        // length) across the envelope; a truncated or reordered field would
        // spawn from the wrong bytes.
        let envelope = HostcallEnvelope {
            request: HostcallRequest::ProcessStart {
                source: ModuleSource::Slice {
                    volume: "selium.control-plane.modules".to_string(),
                    offset: 4096,
                    len: 65_536,
                },
                entrypoint: "main".to_string(),
                arguments: Vec::new(),
                grants: Vec::new(),
                tenant: None,
            },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn attach_declaration_scope_round_trip() {
        for scope in [
            AttachDeclarationScope::Any,
            AttachDeclarationScope::Tenant("acme".to_string()),
        ] {
            let encoded = encode_rkyv(&scope).expect("encode");
            let decoded: AttachDeclarationScope = decode_rkyv(&encoded).expect("decode");
            assert_eq!(decoded, scope);
        }
    }

    #[test]
    fn metadata_max_bytes_is_bounded() {
        // The metadata cap must stay a small, bounded budget: it bounds
        // kernel queue memory per pending handoff entry.
        const _: () = {
            assert!(METADATA_MAX_BYTES <= 64 * 1024);
            assert!(METADATA_MAX_BYTES >= client_identity::FINGERPRINT_LEN + 16);
        };
    }

    #[test]
    fn tcp_bind_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::TcpBind {
                address: "127.0.0.1:8080".to_string(),
            },
            task_id: Some(1),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn tcp_connect_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::TcpConnect {
                address: "127.0.0.1:443".to_string(),
            },
            task_id: Some(2),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn capability_vocabulary_covers_every_variant() {
        // One list, one exhaustive name match: a new variant breaks the match
        // at compile time, and this test then fails until `ALL` lists it.
        let mut seen = std::collections::BTreeSet::new();
        for capability in Capability::ALL {
            assert!(
                seen.insert(capability.name()),
                "duplicate capability name {}",
                capability.name()
            );
            assert!(!capability.name().is_empty());
        }
        assert_eq!(
            seen.len(),
            Capability::ALL.len(),
            "every capability appears exactly once in ALL"
        );

        // The bootstrap-provisioned set is the non-delegable set; keep it in
        // declaration order so a reviewer can diff it.
        let provisioned: Vec<&str> = Capability::ALL
            .iter()
            .filter(|capability| capability.is_bootstrap_provisioned())
            .map(Capability::name)
            .collect();
        assert_eq!(
            provisioned,
            [
                "DelegateGrants",
                "RouteClaim",
                "MintCertificate",
                "QuotaWrite",
                "ProcessProbe",
                "DomainWrite",
            ]
        );
    }

    // scenario: selium-abi/cpu-resource-class/cpu-variant-joins-the-resource-vocabulary
    #[test]
    fn resource_class_uri_segments_cover_every_variant() {
        // Task 1.3: the segment vocabulary covers the entire closed set, and
        // the mapping round-trips. Documented names are asserted exactly.
        let every = [
            (ResourceClass::SharedRegion, "region"),
            (ResourceClass::SharedMapping, "mapping"),
            (ResourceClass::Signal, "signal"),
            (ResourceClass::TcpListener, "listener"),
            (ResourceClass::TcpStream, "stream"),
            (ResourceClass::UdpSocket, "socket"),
            (ResourceClass::Volume, "volume"),
            (ResourceClass::Process, "proc"),
            (ResourceClass::ActivityLog, "activity"),
            (ResourceClass::MeteringStream, "metering"),
            (ResourceClass::GuestLog, "guest-log"),
            (ResourceClass::HostQueue, "queue"),
            (ResourceClass::Cpu, "cpu"),
        ];
        for (class, expected) in every {
            assert_eq!(class.uri_segment(), expected, "segment for {class:?}");
            assert_eq!(
                ResourceClass::from_uri_segment(expected),
                Some(class),
                "inverse for {expected}"
            );
            assert!(expected.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
            let _ = class;
        }
        assert_eq!(ResourceClass::from_uri_segment("unknown"), None);
    }

    #[test]
    fn process_tenant_hostcall_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::ProcessTenant { process_id: 42 },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);

        let output = HostcallOutput::Tenant(Some("acme".to_string()));
        let encoded = encode_rkyv(&output).expect("encode");
        let decoded: HostcallOutput = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, output);
    }

    #[test]
    fn resource_kind_round_trip() {
        for kind in [
            ResourceKind::LogChannel,
            ResourceKind::LiveTable,
            ResourceKind::RpcRing,
            ResourceKind::PubSubTopic,
            ResourceKind::NetworkBuffer,
            ResourceKind::SharedMemory,
        ] {
            let encoded = encode_rkyv(&kind).expect("encode");
            let decoded: ResourceKind = decode_rkyv(&encoded).expect("decode");
            assert_eq!(decoded, kind);
        }
    }

    #[test]
    fn alloc_region_with_purpose_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::AllocRegion {
                pages: 8,
                prot: RegionProt::ReadWrite,
                purpose: ResourceKind::LogChannel,
                serving_tenant: Some("acme".to_string()),
            },
            task_id: Some(5),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn guest_log_register_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::GuestLogRegister { shared_id: 42 },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    // --- Network endpoint URI tests ---

    #[test]
    fn parse_tcp_endpoint() {
        let ep = NetworkEndpoint::parse("tcp://93.184.216.34:443").expect("parse");
        assert_eq!(ep.scheme, "tcp");
        assert_eq!(ep.host, "93.184.216.34");
        assert_eq!(ep.port, "443");
    }

    #[test]
    fn parse_udp_endpoint() {
        let ep = NetworkEndpoint::parse("udp://10.0.0.5:53").expect("parse");
        assert_eq!(ep.scheme, "udp");
        assert_eq!(ep.host, "10.0.0.5");
        assert_eq!(ep.port, "53");
    }

    #[test]
    fn parse_ipv6_bracketed() {
        let ep = NetworkEndpoint::parse("tcp://[2001:db8::1]:443").expect("parse");
        assert_eq!(ep.scheme, "tcp");
        assert_eq!(ep.host, "[2001:db8::1]");
        assert_eq!(ep.port, "443");
    }

    #[test]
    fn parse_lowercases_host() {
        let ep = NetworkEndpoint::parse("tcp://EXAMPLE.COM:8080").expect("parse");
        assert_eq!(ep.host, "example.com");
    }

    #[test]
    fn parse_strips_trailing_dot() {
        let ep = NetworkEndpoint::parse("tcp://example.com.:8080").expect("parse");
        assert_eq!(ep.host, "example.com");
    }

    #[test]
    fn parse_rejects_non_network_scheme() {
        assert!(NetworkEndpoint::parse("sel://acme/payments/").is_none());
    }

    #[test]
    fn parse_rejects_missing_port() {
        assert!(NetworkEndpoint::parse("tcp://127.0.0.1").is_none());
    }

    #[test]
    fn socket_addr_format_ipv4() {
        let addr: std::net::SocketAddr = "93.184.216.34:443".parse().unwrap();
        let uri = format!("tcp://{addr}");
        assert_eq!(uri, "tcp://93.184.216.34:443");
    }

    #[test]
    fn socket_addr_format_ipv6() {
        let addr: std::net::SocketAddr = "[2001:db8::1]:53".parse().unwrap();
        let uri = format!("tcp://{addr}");
        assert_eq!(uri, "tcp://[2001:db8::1]:53");
    }

    // --- Component-aware matching tests ---

    #[test]
    fn uri_prefix_exact_match_tcp() {
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("tcp://93.184.216.34:443".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx = ScopeContext {
            uri: Some("tcp://93.184.216.34:443".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&ctx));
    }

    // scenario: capability-enforcement/component-aware-uri-prefix-matching/port-wildcard
    #[test]
    fn uri_prefix_wildcard_port() {
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("tcp://127.0.0.1:*".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx = ScopeContext {
            uri: Some("tcp://127.0.0.1:8080".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&ctx));
    }

    #[test]
    fn uri_prefix_wildcard_port_only_matches_127_0_0_1() {
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("tcp://127.0.0.1:*".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        // Different host — should NOT match.
        let ctx = ScopeContext {
            uri: Some("tcp://10.0.0.5:8080".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(!grant.allows(&ctx));
    }

    #[test]
    fn uri_prefix_wildcard_label_boundary() {
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("tcp://*.example.com:443".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx = ScopeContext {
            uri: Some("tcp://foo.example.com:443".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&ctx));
    }

    #[test]
    fn uri_prefix_wildcard_multi_label() {
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("tcp://*.example.com:443".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx = ScopeContext {
            uri: Some("tcp://bar.foo.example.com:443".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&ctx));
    }

    // scenario: capability-enforcement/component-aware-uri-prefix-matching/label-suffix-attack-rejected
    #[test]
    fn uri_prefix_rejects_label_suffix_attack() {
        // `tcp://example.com:443` must NOT match `tcp://example.com.evil.com:443`.
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("tcp://example.com:443".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx = ScopeContext {
            uri: Some("tcp://example.com.evil.com:443".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(!grant.allows(&ctx));
    }

    #[test]
    fn uri_prefix_rejects_wildcard_partial_label_prefix() {
        // `*.example.com` must NOT match `badexample.com` (no dot before suffix).
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("tcp://*.example.com:443".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx = ScopeContext {
            uri: Some("tcp://badexample.com:443".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(!grant.allows(&ctx));
    }

    #[test]
    fn uri_prefix_port_list() {
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("tcp://127.0.0.1:80,443".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx_80 = ScopeContext {
            uri: Some("tcp://127.0.0.1:80".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        let ctx_443 = ScopeContext {
            uri: Some("tcp://127.0.0.1:443".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        let ctx_8080 = ScopeContext {
            uri: Some("tcp://127.0.0.1:8080".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&ctx_80));
        assert!(grant.allows(&ctx_443));
        assert!(!grant.allows(&ctx_8080));
    }

    #[test]
    fn uri_prefix_non_network_keeps_string_prefix() {
        // A non-`sel` URI keeps plain starts_with semantics.
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("selium.control".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx_match = ScopeContext {
            uri: Some("selium.control-plane".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        let ctx_no_match = ScopeContext {
            uri: Some("other.control-plane".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&ctx_match));
        assert!(!grant.allows(&ctx_no_match));
    }

    // scenario: capability-enforcement/component-aware-uri-prefix-matching/registry-prefix-respects-segment-boundaries
    #[test]
    fn registry_prefix_respects_segment_boundaries() {
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("sel:///control".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::HostQueue),
            ],
        );
        let same = ScopeContext {
            uri: Some("sel:///control".to_string()),
            resource_class: Some(ResourceClass::HostQueue),
            ..ScopeContext::default()
        };
        let nested = ScopeContext {
            uri: Some("sel:///control/status".to_string()),
            resource_class: Some(ResourceClass::HostQueue),
            ..ScopeContext::default()
        };
        let lookalike = ScopeContext {
            uri: Some("sel:///control-plane".to_string()),
            resource_class: Some(ResourceClass::HostQueue),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&same));
        assert!(grant.allows(&nested));
        assert!(
            !grant.allows(&lookalike),
            "a registry prefix must not match at a non-segment boundary"
        );
    }

    // scenario: capability-enforcement/exact-uri-selector/exact-uri-matches
    // scenario: capability-enforcement/exact-uri-selector/longer-name-does-not-match
    #[test]
    fn exact_uri_selector_matches_only_the_canonical_name() {
        let selector = ResourceSelector::Uri("sel:///control".to_string());
        let exact = ScopeContext {
            uri: Some("sel:///control".to_string()),
            ..ScopeContext::default()
        };
        let longer = ScopeContext {
            uri: Some("sel:///control-plane".to_string()),
            ..ScopeContext::default()
        };
        let nested = ScopeContext {
            uri: Some("sel:///control/status".to_string()),
            ..ScopeContext::default()
        };
        assert!(selector.matches(&exact));
        assert!(!selector.matches(&longer));
        assert!(!selector.matches(&nested), "Uri is exact, never a prefix");
    }

    // scenario: capability-enforcement/exact-uri-selector/wire-name-is-normalised
    #[test]
    fn exact_uri_selector_normalises_wire_names_and_trailing_separators() {
        let selector = ResourceSelector::Uri("sel:///control".to_string());
        let wire = ScopeContext {
            uri: Some("control".to_string()),
            ..ScopeContext::default()
        };
        let trailing = ScopeContext {
            uri: Some("sel:///control/".to_string()),
            ..ScopeContext::default()
        };
        assert!(selector.matches(&wire));
        assert!(selector.matches(&trailing));
    }

    // scenario: selium-abi/route-claim-abi-variants/uri-selector-is-part-of-the-selector-vocabulary
    #[test]
    fn uri_selector_is_evaluatable() {
        let selector = ResourceSelector::Uri("sel:///control".to_string());
        assert!(selector.is_evaluatable(std::slice::from_ref(&selector)));
    }

    // scenario: selium-abi/route-claim-abi-variants/route-claim-variants-round-trip
    #[test]
    fn route_claim_grant_round_trips() {
        let grant = CapabilityGrant {
            capability: Capability::RouteClaim,
            selectors: vec![ResourceSelector::Uri("sel:///control".to_string())],
        };
        let encoded = encode_rkyv(&grant).expect("encode");
        let decoded: CapabilityGrant = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, grant);
    }

    // --- is_evaluatable tests ---

    #[test]
    fn uri_prefix_evaluatable_with_tcp_stream_class() {
        let selectors = vec![
            ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ResourceSelector::UriPrefix("tcp://127.0.0.1:443".to_string()),
        ];
        assert!(selectors[1].is_evaluatable(&selectors));
    }

    #[test]
    fn uri_prefix_evaluatable_with_tcp_listener_class() {
        let selectors = vec![
            ResourceSelector::ResourceClass(ResourceClass::TcpListener),
            ResourceSelector::UriPrefix("tcp://0.0.0.0:8080".to_string()),
        ];
        assert!(selectors[1].is_evaluatable(&selectors));
    }

    #[test]
    fn uri_prefix_evaluatable_with_udp_socket_class() {
        let selectors = vec![
            ResourceSelector::ResourceClass(ResourceClass::UdpSocket),
            ResourceSelector::UriPrefix("udp://10.0.0.5:53".to_string()),
        ];
        assert!(selectors[1].is_evaluatable(&selectors));
    }

    #[test]
    fn uri_prefix_evaluatable_with_volume_class() {
        let selectors = vec![
            ResourceSelector::ResourceClass(ResourceClass::Volume),
            ResourceSelector::UriPrefix("selium.control-plane".to_string()),
        ];
        assert!(selectors[1].is_evaluatable(&selectors));

        // The prefix scopes which volume names the grant admits: plain
        // `starts_with` matching on the volume name.
        let grant = CapabilityGrant::new(Capability::Storage, selectors);
        let admitted = ScopeContext {
            resource_class: Some(ResourceClass::Volume),
            uri: Some("selium.control-plane.modules".to_string()),
            ..ScopeContext::default()
        };
        let refused = ScopeContext {
            resource_class: Some(ResourceClass::Volume),
            uri: Some("acme.private".to_string()),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&admitted));
        assert!(!grant.allows(&refused));
    }

    #[test]
    fn uri_prefix_not_evaluatable_without_an_applicable_class() {
        let selectors = vec![
            ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
            ResourceSelector::UriPrefix("tcp://10.0.0.5:443".to_string()),
        ];
        assert!(!selectors[1].is_evaluatable(&selectors));
    }

    #[test]
    fn uri_prefix_not_evaluatable_with_empty_selectors() {
        let selectors = vec![ResourceSelector::UriPrefix(
            "tcp://10.0.0.5:443".to_string(),
        )];
        assert!(!selectors[0].is_evaluatable(&selectors));
    }

    #[test]
    fn other_selectors_always_evaluatable() {
        let empty: &[ResourceSelector] = &[];
        assert!(ResourceSelector::Tenant("acme".to_string()).is_evaluatable(empty));
        assert!(ResourceSelector::Locality(LocalityScope::Cluster).is_evaluatable(empty));
        assert!(ResourceSelector::ResourceClass(ResourceClass::SharedRegion).is_evaluatable(empty));
        assert!(
            ResourceSelector::ExplicitResource(ResourceIdentity::Shared(1)).is_evaluatable(empty)
        );
        assert!(ResourceSelector::Children.is_evaluatable(empty));
    }

    // scenario: selium-abi/waitregister-hostcall/round-trip-encoding
    #[test]
    fn wait_register_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::WaitRegister {
                region_id: 42,
                generation: 7,
            },
            task_id: Some(3),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    // scenario: selium-abi/quota-hostcall-variants/quotaset-round-trips
    #[test]
    fn quota_set_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::QuotaSet {
                tenant: "acme".to_string(),
                class: ResourceClass::SharedRegion,
                limit: 1_073_741_824,
            },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn quota_clear_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::QuotaClear {
                tenant: "acme".to_string(),
                class: ResourceClass::Volume,
            },
            task_id: Some(7),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `selium-abi` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `addressing` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
