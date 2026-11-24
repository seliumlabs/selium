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
`selium-runtime` SHALL dispatch `TcpConnect`, `TcpBind`, and `UdpBind` hostcalls using non-blocking std sockets registered with the kernel's mio poller for inbound readiness and blocking std threads with generation waits for outbound draining. `NetworkState` SHALL retain lifecycle metadata and running flags.

#### Scenario: Guest connects to TCP endpoint via runtime
- **WHEN** a guest with a `Network` capability invokes `TcpConnect`
- **THEN** the runtime SHALL validate the capability, create a shared ring region, register the stream with the mio poller and lifecycle state, start the outbound drain thread, and return a `SharedRegionDescriptor`

#### Scenario: Guest binds TCP listener via runtime
- **WHEN** a guest with a `Network` capability invokes `TcpBind`
- **THEN** the runtime SHALL validate the capability, bind a non-blocking std `TcpListener`, create a host queue, register listener lifecycle state and the poller accept path, and return a `HostQueueDescriptor`

#### Scenario: Guest binds UDP socket via runtime
- **WHEN** a guest with a `Network` capability invokes `UdpBind`
- **THEN** the runtime SHALL validate the capability, bind a non-blocking std `UdpSocket`, create a shared ring region, register lifecycle state and mio receive handling, start outbound draining, and return a `SharedRegionDescriptor`

### Requirement: Dedicated Timer Driver
`selium-runtime` SHALL own guest `Sleep` deadlines in a dedicated timer driver
that runs on its own thread and delivers completion through the ordinary mailbox
wake path, independently of any ambient executor. A guest sleep SHALL complete at
its deadline whether or not the caller is on a Tokio thread.

#### Scenario: Guest calls Sleep
- **WHEN** a guest invokes `Sleep { millis: 100 }`
- **THEN** the timer driver SHALL complete the operation at the deadline and enqueue the task's wake in the guest's mailbox

#### Scenario: Timer completes without an ambient executor
- **WHEN** a guest sleep is scheduled from a thread with no Tokio reactor
- **THEN** the driver SHALL still deliver the wake at the deadline

#### Scenario: A cancelled sleep is not woken
- **WHEN** the operation behind a scheduled sleep has been dropped before the deadline
- **THEN** the driver SHALL complete nothing and SHALL NOT wake the guest

### Requirement: Cross-Process Channel Wake Delivery
When a region's generation advances, the runtime SHALL deliver a mailbox wake to
every other process that registered interest in that region through
`WaitRegister`. The host SHALL deliver the same wake for its own commits to a
region it writes.

#### Scenario: Cross-guest write wakes a parked reader
- **WHEN** one process commits a frame to a region another process is parked on
- **THEN** the runtime SHALL wake the parked task in the other process through the mailbox

#### Scenario: Host commit wakes its subscriber
- **WHEN** the host publishes to the discovery feed
- **THEN** it SHALL deliver the same wake a guest writer would

#### Scenario: Waking a stopped process is a no-op
- **WHEN** a generation advance targets a process that has already exited
- **THEN** delivery SHALL be dropped without error and without leaving a stale registration

### Requirement: Resource Revocation by Identity

The runtime SHALL revoke a resource's registration by resource identity (class and id) rather than by a runtime-maintained tenant side map: a resource minted under a serving tenant SHALL have its registration revoked under the tenant it was minted for, and process teardown SHALL revoke everything the registry records against the process as owner.

#### Scenario: Minted resource revokes under its tenant

- **WHEN** a resource minted for tenant `acme` is freed
- **THEN** its registration SHALL be revoked under `acme`

#### Scenario: Teardown needs no side map

- **WHEN** a process exits
- **THEN** its registrations SHALL be revoked from the owner recorded in the registry, without the runtime holding per-resource tenant records

### Requirement: Sub-Struct Architecture
`selium-runtime` SHALL keep one coherent `Runtime` owner for guest, authority, resource, operation, and discovery state while separating hostcall resource families behind a common dispatcher. Network, certificate-signing, and time handlers SHALL live in dedicated modules and share lifecycle helpers rather than duplicate state.

#### Scenario: Sub-structs are independently usable
- **WHEN** a resource-family handler is exercised through the runtime dispatcher
- **THEN** it SHALL perform its resource operation without requiring a second runtime or an unrelated guest execution path

#### Scenario: Cross-subsystem orchestration uses function parameters
- **WHEN** an operation crosses resource families or requires kernel and discovery state
- **THEN** orchestration SHALL pass through explicit runtime/kernel/provider contexts rather than ambient process-global providers

### Requirement: Tokio Dependency
`selium-runtime` MAY use Tokio for its binary, native clients, and async integration surfaces, but guest network hostcalls SHALL NOT require an ambient Tokio reactor. The runtime SHALL NOT create a second Tokio runtime inside the library.

#### Scenario: Runtime uses ambient tokio context
- **WHEN** `Runtime::new` and network hostcalls are used outside a Tokio context
- **THEN** std/mio network execution SHALL proceed without a reactor panic; APIs that explicitly require Tokio SHALL document and enforce their own ambient-context requirement

#### Scenario: Tests use #[tokio::test]
- **WHEN** a runtime test exercises an async API that requires a Tokio reactor
- **THEN** the test SHALL be annotated with `#[tokio::test]`; std/mio-only tests MAY remain synchronous

### Requirement: Async Network I/O
`selium-runtime` SHALL implement event-driven TCP and UDP proxy I/O using non-blocking std sockets and mio readiness for inbound traffic, plus blocking std threads with condition-variable generation waits for outbound traffic. Proxy paths SHALL NOT use Tokio socket tasks or sleep-based polling.

#### Scenario: TCP accept loop uses async I/O
- **WHEN** a registered TCP listener becomes readable
- **THEN** the kernel's mio poller SHALL accept the connection without a Tokio `listener.accept().await` task or `thread::sleep` retry loop

#### Scenario: TCP proxy uses async I/O
- **WHEN** the runtime proxies data between a TCP socket and a shared ring buffer
- **THEN** inbound readiness SHALL come from mio and outbound blocking reads/writes SHALL be coordinated by generation waits rather than async Tokio socket methods or sleep polling

### Requirement: Discovery-Enabled Bootstrap

`selium-runtime` SHALL support `start_discovery` in `RuntimeConfig`, creating the Tier-1 feed ring and RPC listener, injecting tagged `WasmValue` entrypoint arguments (feed region id and listener handle into the discovery guest; listener handle into other guests with empty argument lists), and spawning declared guests without gating their startup on another guest's readiness.

#### Scenario: Discovery wiring uses tagged argument encoding

- **WHEN** the runtime injects discovery arguments into a guest descriptor
- **THEN** `decode_wasm_arguments` decodes every injected value without error, for all possible u64 handle values

#### Scenario: Readiness is per-guest

- **WHEN** a bootstrapped guest has not registered its serving route
- **THEN** the runtime SHALL still start it and SHALL NOT roll back the bootstrap on that basis, because readiness is no longer a per-guest gate

#### Scenario: Application guest receives discovery handle

- **WHEN** the runtime bootstraps an application guest
- **THEN** the guest's entrypoint SHALL receive the discovery `shared_id`
  as the first entrypoint argument slot, and the entrypoint macro's
  generated glue SHALL construct the `Context` from it for
  `Context`-leading entrypoints (guests do not call `Context::from_raw`
  themselves)

### Requirement: Runtime discovery publication session
`selium-runtime` SHALL establish the runtime→discovery publication session during bootstrap: the Tier-1 feed publisher, plus the discovery listener queue wired into guest `Context` connections. The session SHALL be used only for authoritative revocation events; guest-visible registrations are published by the guests that own the resources.

#### Scenario: Runtime connects to discovery guest
- **WHEN** the runtime bootstraps with discovery enabled
- **THEN** it SHALL create the runtime→discovery feed publisher and the guest-facing discovery listener
- **AND** authoritative revocation events SHALL be published on that feed

### Requirement: Resource authority record on allocation
When the runtime dispatches `AllocRegion`, `HostQueueCreate`, or a volume open, it SHALL record the minted resource's authoritative identity `(class, id)` and its principal tenant (the tenant on whose behalf the resource is minted) in its authority records, updated synchronously within the minting hostcall.

#### Scenario: Mint records the principal tenant
- **WHEN** a guest invokes `AllocRegion { purpose: LogChannel, ... }`, the runtime allocates region 7, and the allocation's principal tenant is "acme"
- **THEN** the runtime SHALL record that region 7 exists, is owned by the caller, and was minted under tenant "acme"

#### Scenario: Authority answers before the owner publishes
- **WHEN** the owner asks the runtime whether it owns region 7 before it has published the canonical record
- **THEN** the runtime SHALL answer from its mint-time authority record, including tenant "acme"

### Requirement: No host registration publication
The runtime SHALL NOT publish a Tier-1 registration for any resource or process node, and SHALL NOT auto-register purpose aliases. The owner publishes its own canonical, process, and named records.

#### Scenario: Allocation publishes nothing
- **WHEN** a guest allocates a region or creates a host queue
- **THEN** the runtime SHALL NOT publish a registration event for it; the owner publishes the canonical record

#### Scenario: Process start publishes nothing
- **WHEN** a process is spawned
- **THEN** the runtime SHALL NOT publish its process node; the process publishes it

### Requirement: Process Teardown Revocation
When a process exits, the runtime SHALL publish Tier-1 revocation events — `ProcessGone { process_id }` for owner-keyed removal, and `ResourceFreed { class, id }` for each resource identity it reclaims — before or alongside reclaiming its resources. Teardown bookkeeping SHALL be staged: a revocation's pending entry SHALL be removed only after its publish succeeds, so a failed teardown loses no bookkeeping. A stop that fails part-way SHALL retain the process authority so the stop can be retried and the remaining revocations completed, rather than silently skipping them.

#### Scenario: Exit revokes before reclaim
- **WHEN** a process with allocated regions is stopped
- **THEN** revocation events SHALL be published to the discovery feed before its shared resources are reclaimed

#### Scenario: Runtime revokes all process URIs on exit
- **WHEN** process 42 of tenant "acme" terminates
- **THEN** the runtime SHALL publish `ProcessGone { process_id: 42 }`, which revokes its process node and every route it owned
- **AND** subsequent `Resolve` calls for those URIs SHALL return `NotFound`

#### Scenario: Runtime revokes freed resource identities
- **WHEN** process 42 terminates and its region 7 and queue 9 are reclaimed
- **THEN** the runtime SHALL publish `ResourceFreed { class: "region", id: 7 }` and `ResourceFreed { class: "queue", id: 9 }`

#### Scenario: Failed teardown is retryable
- **WHEN** a process stop fails part-way because a discovery revocation cannot be published
- **THEN** the stop SHALL return an error (not succeed silently), the pending revocations SHALL remain staged, and a later stop of the same process SHALL publish them

#### Scenario: Co-owned resource survives an owner exit
- **WHEN** two processes own the same shared region and one of them exits
- **THEN** the runtime SHALL NOT publish `ResourceFreed` for that region, and the region SHALL remain live for the surviving owner
- **AND** the last owner's exit SHALL publish `ResourceFreed` for it

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

### Requirement: Owner-published process nodes
The runtime SHALL NOT publish a process node. A process SHALL publish its own `sel://<tenant>/proc/<id>` node when its entrypoint starts, using its host-attested identity, and the runtime SHALL revoke it on exit with `ProcessGone { process_id }`, so an unpublished process stays undiscoverable but fully accounted. A registry entrypoint that takes infrastructure handles instead of a `Context` is exempt and publishes its routes directly.

#### Scenario: Process node is published by the process
- **WHEN** process 123 of tenant "acme" starts and publishes its node
- **THEN** `sel://acme/proc/123` SHALL resolve through discovery

#### Scenario: Process node revoked at teardown
- **WHEN** process 123 exits
- **THEN** the runtime SHALL publish `ProcessGone { process_id: 123 }` and `sel://acme/proc/123` SHALL no longer resolve

#### Scenario: Unpublished process is accounted but undiscoverable
- **WHEN** process 123 never publishes its node
- **THEN** `sel://acme/proc/123` SHALL NOT resolve
- **AND** the runtime SHALL still know the process and its owned resources

#### Scenario: Registry publishes its routes without a self-RPC
- **WHEN** the registry entrypoint takes the feed and listener handles instead of a `Context`
- **THEN** it SHALL publish its own routes directly and SHALL NOT issue an RPC to its own listener

### Requirement: Principal-Provenance Allocation
`AllocRegion`, `HostQueueCreate` and volume open SHALL mint the resource under the serving tenant supplied with the call, which MAY differ from the caller's own tenant. A root principal (a process with no tenant) MAY mint for any tenant — trusted edge infrastructure such as connectors runs as root. A tenant-scoped process MAY mint for another tenant only with a `DelegateGrants` grant scoped to that tenant; allocation crossing a tenant boundary without that authority SHALL be denied with `AbiErrorCode::PermissionDenied`. A `DelegateGrants` grant SHALL carry at least one `Tenant` selector; a selector-less `DelegateGrants` grant would vacuously match every tenant (including root) and SHALL be rejected at spawn.

#### Scenario: System process allocates for another tenant
- **WHEN** a root/system process holding tenant-scoped delegation allocates a region on behalf of tenant "acme"
- **THEN** the runtime SHALL record the region as minted under tenant "acme", and the owner SHALL publish it as `sel://acme/region/<id>`

#### Scenario: Root principal allocates for any tenant without delegation
- **WHEN** a root process with no `DelegateGrants` grants allocates a region on behalf of tenant "acme" (the connector path: a root connector minting per-stream channels under an authenticated client's tenant)
- **THEN** the runtime SHALL record the region as minted under tenant "acme", and the owner SHALL publish it as `sel://acme/region/<id>`

#### Scenario: Unauthorised cross-tenant allocation denied
- **WHEN** a tenant-scoped process without tenant-scoped delegation attempts to allocate a region for a tenant other than its own
- **THEN** the runtime SHALL return `AbiErrorCode::PermissionDenied`

#### Scenario: Host queue minted under the serving tenant
- **WHEN** a guest in tenant "acme" creates a host queue with no serving tenant, the queue is recorded under `sel://acme/queue/<id>`; when a cross-tenant mint is authorised, the queue is recorded under the serving tenant
- **THEN** teardown SHALL revoke the queue under the tenant it was minted for

#### Scenario: Volume minted under the serving tenant
- **WHEN** a root system guest opens a volume for tenant "acme"
- **THEN** the volume SHALL be recorded under tenant "acme" and counted against acme's volume usage

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

### Requirement: State Root Boot

`selium-runtime` SHALL accept a state directory and root the kernel's volume layer there. A boot against an existing state directory SHALL reconstruct durable state with no host-side index: per-tenant volume usage SHALL be re-derived from the directory's contents, and the store system guest SHALL boot before every guest that depends on durable storage.

#### Scenario: Dependants start after the store

- **WHEN** the runtime bootstraps a store guest and a guest that declares it as a dependency
- **THEN** the store SHALL be ready before the dependant starts

#### Scenario: Usage reconstructed at boot

- **WHEN** the runtime boots against a state directory holding tenant volumes
- **THEN** each tenant's volume usage SHALL reflect the bytes resident there before any guest runs

### Requirement: Host Keyring Persistence

`selium-runtime` SHALL persist host PKI key material — the online intermediate and the per-tenant CA keys — under the state directory and load it at boot before any guest can mint, so a restart never silently replaces the hierarchy and previously issued certificates continue to validate.

#### Scenario: First boot generates and persists

- **WHEN** the host boots with an empty state directory
- **THEN** it SHALL generate a hierarchy and persist it so subsequent boots reuse the same material

#### Scenario: Restart preserves the hierarchy

- **WHEN** the host restarts from its state directory and mints a certificate for a tenant onboarded before the restart
- **THEN** the issued certificate SHALL chain to the tenant CA recorded before the restart

### Requirement: Volume Hostcall Enforcement

`selium-runtime` SHALL enforce volume hostcalls: the storage capability SHALL be required, the volume's serving tenant SHALL be authorised exactly as allocation provenance requires, and the tenant's `Volume` quota SHALL be reserved before appended bytes are written. Usage SHALL be accounted at the volume layer and released when a volume is removed.

#### Scenario: Append beyond the ceiling denied

- **WHEN** an append would exceed the tenant's volume ceiling
- **THEN** the hostcall SHALL fail with `QuotaExceeded` naming the tenant and the Volume dimension, and no bytes SHALL be written

#### Scenario: Removal releases usage

- **WHEN** a volume is removed
- **THEN** its bytes SHALL be released from the tenant's volume usage

### Requirement: Protocol handler route claims
The runtime SHALL provision a route claim for each protocol scheme a guest
descriptor declares, so the guest can publish `sel:///handler/<scheme>` itself
under an authority that cannot be forged. The runtime SHALL NOT publish the
handler's registration event and SHALL NOT keep a host-side handler registry.
Handler resolution SHALL remain a discovery lookup.

#### Scenario: Handler claim is host-provisioned
- **WHEN** the runtime spawns a guest whose descriptor declares a protocol handler
- **THEN** it SHALL provision that handler's singleton route claim for the process
- **AND** it SHALL NOT publish a handler registration event

#### Scenario: Handler guest publishes its claim
- **WHEN** the handler guest publishes `sel:///handler/<scheme>` under its claim
- **THEN** a serve-side guest resolving the scheme SHALL obtain the owning process from discovery

#### Scenario: Guests cannot forge a handler
- **WHEN** a guest publishes a handler route it was not assigned a claim for
- **THEN** the registration SHALL be refused

### Requirement: A concurrent stop is not undone by an in-flight poll

When a process is stopped while one of its reactor passes is executing, the
finishing pass SHALL NOT re-insert the guest into the loaded-guest table. The
stop wins: the guest execution is dropped, so a stopped process cannot leave a
stale loaded guest behind.

#### Scenario: Stop during a poll reclaims the execution

- **WHEN** `stop_process` runs while the guest's poll pass is in flight
- **THEN** the finishing poll SHALL observe the process is no longer running
  and drop the guest instead of re-inserting it

#### Scenario: Repeated stop cycles leave no loaded guests

- **WHEN** a runtime spawns and stops guests repeatedly
- **THEN** after the last stop the loaded-guest table SHALL be empty
