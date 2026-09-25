## Purpose

`selium-runtime` executes Selium guest Wasm modules using Wasmtiny as the WebAssembly runtime substrate, dispatching hostcalls (shared memory, networking, discovery, logging) and managing guest lifecycle including automatic resource registration and revocation with the discovery service.

## Requirements

### Requirement: Wasmtiny-Backed Guest Execution
`selium-runtime` SHALL execute Selium guests using Wasmtiny as the WebAssembly runtime substrate, including the mmap-backed shared memory primitives and hostcall dispatch for networking and RPC.

#### Scenario: Runtime starts a guest module
- **WHEN** the runtime starts a valid guest module
- **THEN** it SHALL instantiate and execute that guest through Wasmtiny with access to `alloc_region`, `free_region`, `attach_region`, `TcpConnect`, `TcpBind`, and `UdpBind` host functions

### Requirement: Shared Memory Hostcall Passthrough
`selium-runtime` SHALL dispatch `AllocRegion`, `FreeRegion`, and `AttachRegion` hostcalls to the kernel's `MemoryRegistry` without additional kernel-layer mediation beyond capability validation.

#### Scenario: Authorised region allocation
- **WHEN** a guest with a capability grant for shared memory invokes `AllocRegion`
- **THEN** the runtime SHALL validate the capability and delegate the allocation to `MemoryRegistry`

#### Scenario: Unauthorised region allocation denied
- **WHEN** a guest without a shared memory capability invokes `AllocRegion`
- **THEN** the runtime SHALL return `AbiErrorCode::PermissionDenied`

### Requirement: Region Lifetime Tied to Guest Lifecycle
When a guest instance terminates, `selium-runtime` SHALL automatically clean up all regions allocated by or attached to that guest.

#### Scenario: Guest exits with attached regions
- **WHEN** a guest that has attached to shared regions exits
- **THEN** the runtime SHALL unmap all shared regions from that guest's memory before releasing the instance

### Requirement: Network Hostcall Dispatch
`selium-runtime` SHALL dispatch `TcpConnect`, `TcpBind`, and `UdpBind` hostcalls by spawning tokio tasks that manage the underlying OS sockets and proxy data through shared ring buffers. The kernel's `NetworkState` SHALL be used only for metadata tracking.

#### Scenario: Guest connects to TCP endpoint via runtime
- **WHEN** a guest with a `Network` capability invokes `TcpConnect`
- **THEN** the runtime SHALL validate the capability, create a shared ring region via the kernel's `MemoryRegistry`, record metadata in the kernel's `NetworkState`, spawn a tokio task for the TCP proxy, and return a `SharedRegionDescriptor`

#### Scenario: Guest binds TCP listener via runtime
- **WHEN** a guest with a `Network` capability invokes `TcpBind`
- **THEN** the runtime SHALL validate the capability, bind an async `TcpListener`, create a host queue via `HostQueueRegistry`, record metadata in `NetworkState`, spawn a tokio task for the accept loop, and return a `HostQueueDescriptor`

#### Scenario: Guest binds UDP socket via runtime
- **WHEN** a guest with a `Network` capability invokes `UdpBind`
- **THEN** the runtime SHALL validate the capability, bind an async `UdpSocket`, create a shared ring region via `MemoryRegistry`, record metadata in `NetworkState`, spawn tokio tasks for recv/send proxying, and return a `SharedRegionDescriptor`

### Requirement: Runtime Uses Tokio for Timers
`selium-runtime` SHALL replace the dedicated timer driver thread with `tokio::spawn` tasks using `tokio::time::sleep`. The `Sleep` hostcall SHALL spawn a tokio task that marks the operation ready and wakes the guest mailbox when the deadline arrives.

#### Scenario: Guest calls Sleep
- **WHEN** a guest invokes `Sleep { millis: 100 }`
- **THEN** the runtime SHALL spawn a tokio task that sleeps for 100ms, then marks the operation ready and enqueues the task ID in the guest's mailbox

#### Scenario: No dedicated timer thread
- **WHEN** the runtime is constructed
- **THEN** no `std::thread::spawn` for timer management occurs
- **AND** no `std::sync::mpsc::channel` for timer requests is created

### Requirement: Sub-Struct Architecture
`selium-runtime` SHALL decompose `Runtime` into sub-structs, each with its own methods:

- `GuestTable` — loaded guests, module registry
- `ProcessAuthorityTable` — process authorities, grants, parent relationships
- `ResourceTracker` — local handle owners, shared resource owners, region purposes
- `HostcallEngine` — operation state machine, mailboxes
- `DiscoveryState` — discovery publisher, listener handle

#### Scenario: Sub-structs are independently usable
- **WHEN** a caller holds a `ResourceTracker` handle
- **THEN** the caller SHALL be able to call `claim_local_handle`, `release_local_handle`, and other resource tracking methods without accessing `Runtime` directly

#### Scenario: Cross-subsystem orchestration uses function parameters
- **WHEN** an operation requires multiple sub-structs (e.g., `cleanup_process_resources` needs `ResourceTracker`, `DiscoveryState`, and `Kernel`)
- **THEN** it SHALL be implemented as a function that takes the required sub-structs as parameters, rather than as a method on a single monolithic struct

### Requirement: Tokio Dependency
`selium-runtime` SHALL depend on tokio with `net`, `rt-multi-thread`, `sync`, `time`, and `macros` features. The runtime SHALL NOT spawn its own tokio runtime — it SHALL use `tokio::spawn` and `tokio::time::sleep` from the ambient context provided by the binary's `#[tokio::main]`.

#### Scenario: Runtime uses ambient tokio context
- **WHEN** `Runtime::new` is called outside of a tokio context and a method that spawns tasks is invoked
- **THEN** the call SHALL panic with "there is no reactor running"

#### Scenario: Tests use #[tokio::test]
- **WHEN** a test in `selium-runtime` spawns network resources
- **THEN** the test function SHALL be annotated with `#[tokio::test]`

### Requirement: Async Network I/O
`selium-runtime` SHALL implement TCP and UDP proxy I/O using tokio's async `TcpListener`, `TcpStream`, and `UdpSocket` types. All proxy operations (`proxy_inbound`, `proxy_outbound`, `accept_loop`, `udp_proxy_recv`, `udp_proxy_send`) SHALL be async functions spawned as tokio tasks.

#### Scenario: TCP accept loop uses async I/O
- **WHEN** the runtime's TCP accept loop is running
- **THEN** it SHALL use `listener.accept().await` rather than non-blocking accept with `thread::sleep`

#### Scenario: TCP proxy uses async I/O
- **WHEN** the runtime proxies data between a TCP socket and a shared ring buffer
- **THEN** it SHALL use `stream.read().await` and `stream.write_all().await` rather than blocking `read()`/`write()` with `thread::sleep`

### Requirement: Discovery-Enabled Bootstrap

`selium-runtime` SHALL support `start_discovery` in `RuntimeConfig`, creating the Tier-1 feed ring and RPC listener, injecting tagged `WasmValue` entrypoint arguments (feed region id and listener handle into the discovery guest; listener handle into other guests with empty argument lists), and gating readiness per guest on `mark_ready()`.

#### Scenario: Discovery wiring uses tagged argument encoding

- **WHEN** the runtime injects discovery arguments into a guest descriptor
- **THEN** `decode_wasm_arguments` decodes every injected value without error, for all possible u64 handle values

#### Scenario: Readiness is per-guest

- **WHEN** a bootstrapped guest does not call `mark_ready()` within the readiness window
- **THEN** the runtime rolls back the bootstrap and reports `ReadinessUnsatisfied` naming that guest

#### Scenario: Application guest receives discovery handle

- **WHEN** the runtime bootstraps an application guest
- **THEN** the guest's entrypoint SHALL receive the discovery `shared_id`
  as the first entrypoint argument slot, and the entrypoint macro's
  generated glue SHALL construct the `Context` from it for
  `Context`-leading entrypoints (guests do not call `Context::from_raw`
  themselves)

### Requirement: Runtime discovery RPC session
`selium-runtime` SHALL hold an `RpcClient<DiscoveryRequest, DiscoveryResponse>` connected to the discovery guest, established during bootstrap alongside the existing discovery queue for guest `Context` connections. This session SHALL be used for authoritative Tier-1 resource registration.

#### Scenario: Runtime connects to discovery guest
- **WHEN** the runtime bootstraps
- **THEN** it SHALL create an `RpcClient` to the discovery guest for authoritative resource registration

### Requirement: Automatic resource registration on allocation
When the runtime dispatches an `AllocRegion` hostcall, it SHALL send `DiscoveryRequest::Register` for the allocated region under `sel://<tenant>/region/<id>`, where `<tenant>` is the allocation's principal tenant (the tenant on whose behalf the region is minted). The runtime SHALL NOT auto-register purpose aliases; aliases are registered explicitly by the resource's owner.

#### Scenario: Runtime registers log channel on AllocRegion
- **WHEN** a guest invokes `AllocRegion { purpose: LogChannel, ... }`, the runtime allocates region 7, and the allocation's principal tenant is "acme"
- **THEN** the runtime SHALL register `sel://acme/region/7` and SHALL NOT register a purpose alias

#### Scenario: Runtime registers generic SharedMemory region
- **WHEN** a guest invokes `AllocRegion { purpose: SharedMemory, ... }` and the runtime allocates region 3 under tenant "acme"
- **THEN** the runtime SHALL register `sel://acme/region/3`

### Requirement: Process Teardown Revocation
When a process exits, the runtime SHALL publish Tier-1 revocation events for the process's region URIs and for its process node before reclaiming its resources. Teardown bookkeeping SHALL be staged: a revocation's pending entry SHALL be removed only after its publish succeeds, so a failed teardown loses no bookkeeping. A stop that fails part-way SHALL retain the process authority so the stop can be retried and the remaining revocations completed, rather than silently skipping them.

#### Scenario: Exit revokes before reclaim
- **WHEN** a process with allocated regions is stopped
- **THEN** revocation events for its region URIs SHALL be published to the discovery feed before its shared resources are reclaimed

#### Scenario: Runtime revokes all process URIs on exit
- **WHEN** process 42 of tenant "acme" terminates
- **THEN** the runtime SHALL revoke `sel://acme/region/*` for the regions it allocated and SHALL revoke `sel://acme/proc/42`
- **AND** subsequent `Resolve` calls for those URIs SHALL return `NotFound`

#### Scenario: Failed teardown is retryable
- **WHEN** a process stop fails part-way because a discovery revocation cannot be published
- **THEN** the stop SHALL return an error (not succeed silently), the pending revocations SHALL remain staged, and a later stop of the same process SHALL publish them

### Requirement: GuestLogRegister hostcall validation
The runtime SHALL validate that the `shared_id` in a `GuestLogRegister` hostcall was allocated by the calling process. If the `shared_id` belongs to a different process, the runtime SHALL return an error.

#### Scenario: GuestLogRegister accepted for own region
- **WHEN** process 42 sends `GuestLogRegister { shared_id }` and `shared_id` corresponds to a region allocated by process 42
- **THEN** the runtime SHALL attach to the region as a log reader and return success

#### Scenario: GuestLogRegister rejected for foreign region
- **WHEN** process 42 sends `GuestLogRegister { shared_id }` and `shared_id` corresponds to a region allocated by process 99
- **THEN** the runtime SHALL return an error without attaching

### Requirement: Discovery handle passed to guest entrypoints

The runtime SHALL prepend the discovery host queue `shared_id` as the
first entrypoint argument slot (existing behaviour, unchanged). For
`Context`-leading entrypoints, the `#[entrypoint]` macro constructs the
`Context` from that slot via `Context::from_raw`; guests do not call
`Context::from_raw` in their own code. The runtime's own authoritative
discovery RPC session SHALL be separate from the guest-facing discovery
queue.

#### Scenario: Application guest receives discovery handle (unchanged)

- **WHEN** the runtime bootstraps an application guest
- **THEN** the guest's entrypoint SHALL receive the discovery `shared_id`
  as the first argument slot
- **AND** the entrypoint macro SHALL construct the leading `Context` from
  that slot for `Context`-declaring entrypoints

### Requirement: Grant Admission and Evaluation
`selium-runtime` SHALL reject, at spawn or `ProcessStart`, any grant
whose selectors it cannot evaluate, and SHALL evaluate every accepted
grant against authority-derived scope contexts. Empty selector lists
SHALL mean "unrestricted within the capability" and be documented as such.

#### Scenario: Accept-then-deny is impossible

- **WHEN** a guest is spawned with a grant the runtime would never be
  able to satisfy (unevaluatable selector)
- **THEN** spawning fails immediately with the selector named — the
  grant cannot enter the accept-then-always-deny state

#### Scenario: Errors attribute correctly

- **WHEN** any authorisation check fails
- **THEN** the error identifies the denied capability and the relevant
  scope values (tenant/class/identity) rather than a generic or
  misattributed capability

### Requirement: Entrypoint Failure Detection
`selium-runtime` SHALL check the Wasm return value after executing a guest entrypoint and fail fast if it indicates an error.

#### Scenario: Non-zero exit code fails bootstrap
- **WHEN** `execute_entrypoint` returns `[WasmValue::I32(1)]`
- **THEN** the runtime SHALL return `Error::EntrypointFailed` before reaching the readiness check
- **AND** the existing cleanup path SHALL be invoked (record `ProcessExited`, `cleanup_failed_process`)

#### Scenario: Zero exit code proceeds normally
- **WHEN** `execute_entrypoint` returns `[WasmValue::I32(0)]`
- **THEN** the runtime SHALL proceed to the readiness check as normal

#### Scenario: Void entrypoint (no return value) proceeds normally
- **WHEN** `execute_entrypoint` returns an empty `Vec<WasmValue>` (existing `()`-returning entrypoints)
- **THEN** the runtime SHALL proceed to the readiness check as normal

### Requirement: Entrypoint Argument Injection

`selium-runtime` SHALL inject entrypoint arguments by decoding each
`SystemGuestDescriptor` argument into tagged `WasmValue`s. Integer
arguments SHALL be encoded as `WasmValue::I64`. Pointer arguments SHALL
carry a byte payload that the runtime copies into the guest's linear
memory before invoking the entrypoint; the pair `(address, length)` SHALL
then be encoded as two consecutive `WasmValue::I64` slots (address first,
then length).

#### Scenario: Integer argument encoded as i64

- **WHEN** a descriptor declares an integer argument
- **THEN** `decode_wasm_arguments` decodes it without error for all possible u64 values

#### Scenario: Pointer argument bytes injected into guest memory

- **WHEN** a descriptor declares a pointer argument with payload bytes
- **THEN** the runtime copies the payload into the guest's linear memory before invoking the entrypoint
- **AND** the entrypoint receives two `i64` arguments: the address the bytes were written at, and the byte length

#### Scenario: Pointer argument layout is declaration-ordered

- **WHEN** a descriptor declares an integer argument followed by a pointer argument
- **THEN** the entrypoint receives the integer in the first slot and the pointer pair in the following two slots

#### Scenario: Oversized pointer payload rejected

- **WHEN** a pointer-argument payload cannot be written into the guest's linear memory
- **THEN** the runtime SHALL fail the bootstrap with a descriptive error rather than truncating or silently dropping the payload

### Requirement: Process Node Registration
The runtime SHALL publish a Tier-1 registration for a process node `sel://<tenant>/proc/<id>` when a process is spawned, and a revocation when the process is cleaned up, so every process is discoverable even before it allocates any resource.

#### Scenario: Process node registered at spawn
- **WHEN** process 123 of tenant "acme" is spawned
- **THEN** the runtime SHALL register `sel://acme/proc/123` so it resolves through discovery

#### Scenario: Process node revoked at teardown
- **WHEN** process 123 exits
- **THEN** the runtime SHALL revoke `sel://acme/proc/123` so it no longer resolves

### Requirement: Principal-Provenance Allocation
`AllocRegion` and `HostQueueCreate` SHALL mint the resource under the serving tenant supplied with the allocation, which MAY differ from the allocating process's own tenant. A root principal (a process with no tenant) MAY mint for any tenant — trusted edge infrastructure such as connectors runs as root. A tenant-scoped process MAY mint for another tenant only with a `DelegateGrants` grant scoped to that tenant; allocation crossing a tenant boundary without that authority SHALL be denied with `AbiErrorCode::PermissionDenied`. A `DelegateGrants` grant SHALL carry at least one `Tenant` selector; a selector-less `DelegateGrants` grant would vacuously match every tenant (including root) and SHALL be rejected at spawn.

#### Scenario: System process allocates for another tenant
- **WHEN** a root/system process holding tenant-scoped delegation allocates a region on behalf of tenant "acme"
- **THEN** the runtime SHALL register the region under `sel://acme/region/<id>`

#### Scenario: Root principal allocates for any tenant without delegation
- **WHEN** a root process with no `DelegateGrants` grants allocates a region on behalf of tenant "acme" (the connector path: a root connector minting per-stream channels under an authenticated client's tenant)
- **THEN** the runtime SHALL register the region under `sel://acme/region/<id>`

#### Scenario: Unauthorized cross-tenant allocation denied
- **WHEN** a tenant-scoped process without tenant-scoped delegation attempts to allocate a region for a tenant other than its own
- **THEN** the runtime SHALL return `AbiErrorCode::PermissionDenied`

#### Scenario: Host queue minted under the serving tenant
- **WHEN** a guest in tenant "acme" creates a host queue with no serving tenant, the queue is registered under `sel://acme/queue/<id>`; when a cross-tenant mint is authorized, the queue is registered under the serving tenant
- **THEN** teardown SHALL revoke the queue under the tenant it was minted for

#### Scenario: Selector-less delegation grant rejected
- **WHEN** a system guest descriptor carries a `DelegateGrants` grant with no `Tenant` selector
- **THEN** the runtime SHALL reject the spawn with an invalid-grant error

### Requirement: Entrypoint Completion Terminates the Process
`selium-runtime` SHALL treat completion of a spawned guest's poll-owner entrypoint as the process's normal exit: when the guest's poll export reports that the entrypoint future has completed, the runtime SHALL record `ProcessExited` and run the existing process teardown — revoking the process's discovery URIs, reclaiming its regions and queued pipe slots, and releasing its process-quota reservation — rather than leaving the guest resident as an idle reactor.

#### Scenario: Completed entrypoint reaps the process
- **WHEN** a bridge-channel's pipe tears down so its entrypoint future completes
- **THEN** the runtime SHALL record `ProcessExited` and reclaim the process, its regions, and its pipe slots

#### Scenario: Long-running entrypoint stays resident
- **WHEN** a guest's poll-owner entrypoint future parks (a service loop that never returns)
- **THEN** the runtime SHALL keep the process resident and SHALL NOT reap it

### Requirement: Process Spawn Quota
`selium-runtime` SHALL enforce the tenant-scoped `ResourceClass::Process` quota when a process is spawned: a spawn whose target tenant is at its authored process ceiling SHALL be denied with `QuotaExceeded` naming the tenant and the Process dimension, before the child is instantiated. The reservation SHALL be released when the spawned process exits or is reaped. Tenant-less (root) processes SHALL NOT be metered.

#### Scenario: Spawn over the process ceiling denied
- **WHEN** the bridge-server spawns a bridge-channel for a tenant whose process quota is exhausted
- **THEN** the spawn SHALL fail with `QuotaExceeded` and no child process SHALL be created

#### Scenario: Quota distinct from lifecycle grant
- **WHEN** a process holds `ProcessLifecycle` but its target tenant is at its process ceiling
- **THEN** the spawn SHALL still be denied by the quota counter

#### Scenario: Exited process returns its slot
- **WHEN** a spawned process exits
- **THEN** its process-quota reservation SHALL return to the tenant's counter
