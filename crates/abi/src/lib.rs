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
//! | `UriPrefix` | **Enforced*** | `uri` (network endpoints only) |
//!
//! \* `UriPrefix` is evaluatable only when the same grant also carries a
//! network `ResourceClass` selector (`TcpListener`, `TcpStream`, or
//! `UdpSocket`). Otherwise the grant is rejected at registration time.
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
    /// Permission to use durable logs and blob stores.
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
    /// Permission to register routes in the root/system tenant (`sel:///…`),
    /// replacing the runtime's well-known-URI provisioning. Without this grant
    /// a guest's `serve` registration in the root namespace is forbidden by
    /// discovery.
    SystemRegistration,
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
    /// Durable log resource.
    DurableLog,
    /// Blob store resource.
    BlobStore,
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
    /// Match resources whose URI starts with the prefix.
    UriPrefix(String),
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

/// Grant allowing one capability within the intersection of its selectors.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct CapabilityGrant {
    /// Capability being granted.
    pub capability: Capability,
    /// Selectors that must all match for the grant to apply.
    pub selectors: Vec<ResourceSelector>,
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

/// Descriptor for a durable log handle.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct DurableLogDescriptor {
    /// Local log handle id.
    pub local_id: LocalResourceId,
    /// Shared log id.
    pub shared_id: SharedResourceId,
    /// Durable log name.
    pub name: String,
}

/// Descriptor for a blob store handle.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct BlobStoreDescriptor {
    /// Local blob store handle id.
    pub local_id: LocalResourceId,
    /// Shared blob store id.
    pub shared_id: SharedResourceId,
    /// Blob store name.
    pub name: String,
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

/// Record stored in a durable log.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct StorageRecord {
    /// Monotonic log sequence number.
    pub sequence: u64,
    /// Record timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// User-supplied header key-value pairs.
    pub headers: Vec<(String, String)>,
    /// Record payload bytes.
    pub payload: Vec<u8>,
}

/// Kind of event recorded in the activity log.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum ActivityKind {
    /// A process was started.
    ProcessStarted,
    /// A guest reported readiness.
    GuestReady,
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
    /// Shared memory region for durable log storage.
    DurableLog,
    /// Shared memory region for blob store.
    BlobStore,
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
    /// Open a durable log.
    StorageOpenLog {
        /// Durable log name.
        name: String,
    },
    /// Close a durable log handle.
    StorageLogClose {
        /// Local log handle id to close.
        local_id: LocalResourceId,
    },
    /// Append a record to a durable log.
    StorageLogAppend {
        /// Local log handle id.
        local_id: LocalResourceId,
        /// Record timestamp in milliseconds.
        timestamp_ms: u64,
        /// Record headers.
        headers: Vec<(String, String)>,
        /// Record payload bytes.
        payload: Vec<u8>,
    },
    /// Replay records from a durable log.
    StorageLogReplay {
        /// Local log handle id.
        local_id: LocalResourceId,
        /// Optional first sequence number to include.
        from_sequence: Option<u64>,
        /// Maximum number of records to return.
        limit: u32,
    },
    /// Store a named durable log checkpoint.
    StorageLogCheckpoint {
        /// Local log handle id.
        local_id: LocalResourceId,
        /// Checkpoint name.
        name: String,
        /// Sequence number to record.
        sequence: u64,
    },
    /// Read a named durable log checkpoint.
    StorageLogCheckpointRead {
        /// Local log handle id.
        local_id: LocalResourceId,
        /// Checkpoint name.
        name: String,
    },
    /// Open a blob store.
    StorageOpenBlobStore {
        /// Blob store name.
        name: String,
    },
    /// Close a blob store handle.
    StorageBlobStoreClose {
        /// Local blob store handle id to close.
        local_id: LocalResourceId,
    },
    /// Put bytes into a blob store.
    StorageBlobPut {
        /// Local blob store handle id.
        local_id: LocalResourceId,
        /// Blob bytes to store.
        bytes: Vec<u8>,
    },
    /// Get bytes from a blob store.
    StorageBlobGet {
        /// Local blob store handle id.
        local_id: LocalResourceId,
        /// Blob id to read.
        blob_id: String,
    },
    /// Set a named manifest to a blob id.
    StorageBlobSetManifest {
        /// Local blob store handle id.
        local_id: LocalResourceId,
        /// Manifest name.
        name: String,
        /// Blob id to associate with the manifest.
        blob_id: String,
    },
    /// Read a named manifest from a blob store.
    StorageBlobGetManifest {
        /// Local blob store handle id.
        local_id: LocalResourceId,
        /// Manifest name.
        name: String,
    },
    /// Start a process.
    ProcessStart {
        /// Module id to execute.
        module_id: String,
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
    /// Create a host-mediated connection queue, registered with discovery
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
    /// Returns the process id of the registered Tier-1 protocol handler for
    /// `scheme` (e.g. `sel-quic`), if one is registered. Handler
    /// registrations are bootstrap-authoritative (runtime-published), so
    /// the result cannot be forged by guests. Used by serve-side guests to
    /// pin which process may legitimately deliver handoffs.
    ResolveProtocolHandler {
        /// Protocol scheme to resolve (e.g. `sel-quic`).
        scheme: String,
    },
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
    /// Returns whether a process holds a capability. Restricted to the
    /// discovery system guest so it can gate root-registration requests
    /// against the caller's grants.
    ProcessCapability {
        /// Process whose grants to check.
        process_id: ProcessId,
        /// Capability to check.
        capability: Capability,
    },
    /// Records that a route was registered by a process, so the runtime can
    /// gate the readiness of a role-declared system guest on its registration
    /// being observable in discovery. Restricted to the discovery system
    /// guest.
    RecordRegistration {
        /// Process that registered the route.
        process_id: ProcessId,
        /// Internal URI the route was registered under.
        uri: String,
    },
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
    /// Record that a discovery resolve performed by `client_process_id`
    /// returned `shared_id`. Callable only by the discovery system guest;
    /// the runtime rejects the hostcall from any other process. This gives
    /// the resolved client an authorisation basis for cross-process
    /// `HostQueueAttach` without requiring an `ExplicitResource` grant.
    RecordResolvedQueueFor {
        /// Process that performed the discovery resolve.
        client_process_id: ProcessId,
        /// Queue id returned to that process by the resolve.
        shared_id: SharedResourceId,
    },
    /// Record that a discovery resolve performed by `client_process_id`
    /// returned a shared region. Callable only by the discovery system
    /// guest; the recorded id gives the resolving client an authorisation
    /// basis for `AttachRegion` on a region it did not allocate (the basis
    /// peer guests use to attach an identity guest's published live tables).
    RecordResolvedRegionFor {
        /// Process that performed the discovery resolve.
        client_process_id: ProcessId,
        /// Region id returned to that process by the resolve.
        shared_id: SharedResourceId,
    },
    /// Sign a tenant CA: generate a host-held tenant CA keypair and sign it
    /// via the online intermediate key. Returns the DER-encoded tenant CA
    /// certificate; the private key never leaves the host.
    SignTenantCa {
        /// Tenant whose CA is minted.
        tenant: String,
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
    /// Storage usage in bytes.
    pub storage_bytes: u64,
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
    /// A durable log descriptor.
    DurableLog(DurableLogDescriptor),
    /// A blob store descriptor.
    BlobStore(BlobStoreDescriptor),
    /// A process descriptor.
    Process(ProcessDescriptor),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// Blob id string.
    BlobId(String),
    /// Optional sequence number.
    Sequence(Option<u64>),
    /// Shared resource id.
    SharedId(SharedResourceId),
    /// Durable log records.
    StorageRecords(Vec<StorageRecord>),
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
            Self::DurableLog => "log",
            Self::BlobStore => "blob",
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
            "log" => Some(Self::DurableLog),
            "blob" => Some(Self::BlobStore),
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
    ///   `Children`: evaluatable.
    /// - `UriPrefix`: evaluatable only when the same grant also carries a network
    ///   `ResourceClass` selector (`TcpListener`, `TcpStream`, or `UdpSocket`).
    pub fn is_evaluatable(&self, grant_selectors: &[ResourceSelector]) -> bool {
        match self {
            Self::UriPrefix(_) => grant_selectors.iter().any(|s| {
                matches!(
                    s,
                    Self::ResourceClass(ResourceClass::TcpListener)
                        | Self::ResourceClass(ResourceClass::TcpStream)
                        | Self::ResourceClass(ResourceClass::UdpSocket)
                )
            }),
            Self::Tenant(_)
            | Self::Namespace(_)
            | Self::Locality(_)
            | Self::ResourceClass(_)
            | Self::ExplicitResource(_)
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
    /// list, or `*`). Non-network URIs keep plain `starts_with` semantics.
    pub fn matches(&self, context: &ScopeContext) -> bool {
        match self {
            Self::Tenant(expected) => context.tenant.as_ref() == Some(expected),
            Self::Namespace(namespace) => namespace.matches_tenant(context.tenant.as_deref()),
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
                        } else {
                            // Plain string prefix for non-network URIs.
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

    /// `ResourceSelector::Namespace` matches hierarchically: a tenant selector
    /// admits only its own tenant, and the root selector admits only a root
    /// (tenant-less) scope context.
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
    fn metering_observation_round_trip() {
        // The observation carries the cumulative instruction count and the
        // memory/storage/bandwidth gauges. The former microsecond CPU field
        // is intentionally absent from the type, so there is nothing to read.
        let output = HostcallOutput::Metering(MeteringObservation {
            cpu_instructions: 123_456,
            memory_bytes: 4_096,
            storage_bytes: 512,
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
        assert_eq!(observation.storage_bytes, 512);
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

    #[test]
    fn resolve_protocol_handler_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::ResolveProtocolHandler {
                scheme: "sel-quic".to_string(),
            },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }

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
    fn record_resolved_region_for_round_trip() {
        let envelope = HostcallEnvelope {
            request: HostcallRequest::RecordResolvedRegionFor {
                client_process_id: 42,
                shared_id: 7,
            },
            task_id: None,
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
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
            (ResourceClass::DurableLog, "log"),
            (ResourceClass::BlobStore, "blob"),
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
            ResourceKind::DurableLog,
            ResourceKind::BlobStore,
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
        // Non-network URI keeps plain starts_with semantics.
        let grant = CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::UriPrefix("sel://acme/payments/".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        );
        let ctx_match = ScopeContext {
            uri: Some("sel://acme/payments/worker".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        let ctx_no_match = ScopeContext {
            uri: Some("sel://acme/other/worker".to_string()),
            resource_class: Some(ResourceClass::TcpStream),
            ..ScopeContext::default()
        };
        assert!(grant.allows(&ctx_match));
        assert!(!grant.allows(&ctx_no_match));
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
    fn uri_prefix_not_evaluatable_without_network_class() {
        let selectors = vec![
            ResourceSelector::ResourceClass(ResourceClass::DurableLog),
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
                class: ResourceClass::DurableLog,
            },
            task_id: Some(7),
        };
        let encoded = encode_rkyv(&envelope).expect("encode");
        let decoded: HostcallEnvelope = decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, envelope);
    }
}
