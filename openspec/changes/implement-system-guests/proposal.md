# Proposal: Implement System Guests for Selium arch3

## Why

The Selium architecture described in `ARCHITECTURE.md` requires five system guests to manage the platform:

1. **selium-scheduler** - Places workloads across hosts based on capacity and dependencies
2. **selium-supervisor** - Monitors guest health and handles restart policies
3. **selium-discovery** - Maps URIs to host + resource IDs for service discovery
4. **selium-cluster** - Tracks hosts in the cluster and coordinates shared fabric state
5. **selium-external-api** - Bridges external QUIC connections to internal guest interfaces

Without these, the platform has a host/guest foundation but no guest-owned control plane for accepting user intent, deciding placement, exposing discovery, monitoring workloads, or coordinating host state.

The foundation work from `specify-host-guest-foundation` has landed and is archived. This change must therefore build on the current workspace, not the older proposal assumptions: core crates now live under `crates/core/...`, guest-side libraries live under `crates/libs/...`, and system guests live under `crates/guests/...`.

## What Changes

### Core Implementation

- **selium-cluster** guest: Tracks host membership, projects host load and availability, and owns the day 1 cluster state topics/tables consumed by other guests
- **selium-discovery** guest: Maintains Selium URI and interface metadata mappings, backed by durable storage and live-table projection where appropriate
- **selium-scheduler** guest: Accepts workload placement intent, writes scheduler-owned desired state, and reconciles local host process state through the runtime/kernel primitives
- **selium-supervisor** guest: Observes runtime activity and metering, maintains health state, and emits restart or rescheduling intent
- **selium-external-api** guest: Accepts externally authenticated user intent, interprets it narrowly, delegates policy to the relevant system guests, and reports progress

### Architectural Patterns

- **Foundation reuse**: System guests build on the implemented `selium-abi`, `selium-kernel`, `selium-runtime`, `selium-guest`, `selium-guest-macros`, and `selium-io` crates
- **I/O patterns in `selium-io`**: Shared-memory topics, typed channels, pub/sub, and live tables come from `selium-io`; `selium-guest` remains the primitive handle, codec, logging, and macro-facing SDK
- **Request/response without RPC privilege**: Synchronous interactions use the current network request-exchange or explicit typed channels where needed, without making RPC the privileged substrate
- **State machine pattern**: gate or observe state, compute the next desired state, write through owned durable/live state, then reconcile observed host state
- **Narrow orchestrator**: external-api interprets user intent and delegates instead of owning placement, discovery, or recovery policy
- **Generic bootstrap**: system guests are started by `selium-runtime` from `SystemGuestDescriptor` configuration rather than hard-coded startup logic

### Current Foundation Facts

- `selium-runtime` exposes `RuntimeConfig`, `SystemGuestDescriptor`, `ReadinessCondition`, and dependency-ordered bootstrap.
- `selium-guest-macros::entrypoint` currently supports async zero-argument entrypoints returning `()` and emits entrypoint metadata.
- `selium-guest-macros::pattern_interface` emits interface metadata, but does not generate a transport implementation.
- `selium-io` provides shared-memory-backed channels, pub/sub topics, and live tables; it does not yet provide a complete request/reply or fanout API surface beyond the implemented primitives.
- `selium-kernel` network primitives are protocol-neutral listener, session, stream, and request-exchange resources; QUIC and mTLS semantics must be provided by the runtime/network bridge or added explicitly before guests depend on them.

### Prior Art to Reuse Carefully

From **arch2/openspec/changes/implement-guest-modules/**:
- State-machine structure for scheduler and supervisor guests
- Service and discovery patterns for guest metadata and routing
- Guest logging via `tracing` integration

From **arch2/exploration-remote-cli-architecture.md**:
- Narrow orchestrator model for external intent interpretation
- Channel ownership and source-of-truth patterns
- Metadata-generation ideas for guest-facing interfaces

From **wasmtiny**:
- Hot-pluggable shared memory as the primitive substrate replacing old queue-shaped workarounds

## Capabilities

### Existing Foundation Capabilities Used

- **Process lifecycle authority**: Required by scheduler and supervisor to start, stop, and inspect guest processes
- **Storage, shared-memory, and signal authority**: Required by discovery, scheduler, cluster, and supervisor to manage durable state, topics, and live tables through `selium-io`
- **Network authority**: Required by external-api and cluster for configured listeners, sessions, streams, and request exchanges
- **Activity and metering visibility**: Required by supervisor and cluster to react to lifecycle and load changes
- **Guest log authority**: Required by all system guests for structured guest logging through `selium-guest`

### Capability Scoping

Per `ARCHITECTURE.md`, capabilities use resource scopes to limit access. System guests need:
- Scheduler: authority across placement-relevant scopes
- Supervisor: authority to observe and recover managed processes
- Discovery: authority to resolve and register platform-visible resources
- Cluster: authority to coordinate host-level state across the cluster
- External API: authority to accept external user intent and delegate into internal guest interfaces

Exact grants should be expressed as `CapabilityGrant` values using tenant, URI-prefix, locality, resource-class, and explicit-resource selectors from `selium-abi`.

## Impact

### New Workspace Crates

- `crates/guests/external-api/` - External API guest
- `crates/guests/supervisor/` - Supervisor guest
- `crates/guests/scheduler/` - Scheduler guest
- `crates/guests/discovery/` - Discovery guest
- `crates/guests/cluster/` - Cluster guest

Each crate should use the package name `selium-<guest-name>` while keeping the directory name unprefixed.

### Dependencies on Existing Foundation Work

- **ABI**: `selium-abi` for capability, scope, handle, hostcall, metadata, and framing contracts
- **Kernel primitives**: `selium-kernel` for shared memory, signalling, network, storage, process, activity, and metering primitives
- **Runtime**: `selium-runtime` for generic system guest bootstrap and scoped capability grants
- **Guest SDK**: `selium-guest` for handles, codecs, logging, platform calls, and macro integration
- **I/O patterns**: `selium-io` for channels, pub/sub topics, and live tables
- **Macros**: `selium-guest-macros` for entrypoint glue and guest interface metadata generation

## Deferred Items (Separate Proposals)

The following are out of scope for day 1 but should be tracked:

1. **Channel replication**: Write-master/read-slaves with election for new master
2. **Large cluster scaling**: Full mesh to gossip transition
3. **Process migration**: Guest snapshot and restore
4. **Channel resilience**: Quorum-based durability
5. **Full request/reply pattern library**: Dedicated typed request/reply abstraction if the network request-exchange primitive is not sufficient
6. **Transport security implementation details**: QUIC and mTLS bridge work not already covered by runtime/network primitives

## Risks

- **Complexity**: Five inter-connected system guests is substantial, so implement one at a time and test integration as each guest lands
- **Runtime/guest macro mismatch**: Runtime supports arguments but the current entrypoint macro is zero-argument only, so initial guests should use zero-argument entrypoints or add macro support deliberately
- **Pattern surface mismatch**: `selium-io` currently covers channels, pub/sub, and live tables, but not every named pattern in the architecture; avoid depending on unimplemented abstractions without adding tasks first
- **Transport security gap**: Current network primitives are protocol-neutral; avoid claiming guest-level QUIC/mTLS until the runtime bridge or guest capability exists
- **Multi-node coordination**: Shared state across hosts remains complex; start with single-host behaviour and add minimal cross-host visibility incrementally

## Timeline Considerations

1. First: Create system guest crates and metadata using the current foundation APIs
2. Second: Implement cluster and discovery state surfaces needed by scheduler and external-api
3. Third: Implement scheduler and supervisor control loops
4. Fourth: Implement external-api delegation and client feedback
5. Fifth: Bootstrap all guests through `selium-runtime` and validate a single-host demo

This proposal is ready to action against the current foundation once the OpenSpec change validates.
