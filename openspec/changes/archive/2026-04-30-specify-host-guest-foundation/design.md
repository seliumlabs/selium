## Context

arch3 currently has an architecture document and several feature proposals, but the shared foundation between host and guest remains underspecified. The `implement-system-guests` change already assumes clear contracts for capabilities, bootstrap, primitive resources, and guest-side communication, yet those contracts do not exist as first-class specifications.

This gap matters more now because the intended runtime substrate has changed. Earlier Selium iterations leaned on queue-shaped primitives as a workaround for Wasmtime's limitations around hot-pluggable shared memory. arch3 instead targets Wasmtiny, whose shared-memory model makes it reasonable to treat shared regions as first-class runtime objects and build higher-level messaging patterns above them.

The design therefore needs to pin down five crates and the boundaries between them:

- `selium-abi`
- `selium-kernel`
- `selium-runtime`
- `selium-guest`
- `selium-guest-macros`

## Goals / Non-Goals

**Goals:**
- Define the architectural split between ABI, kernel primitives, runtime orchestration, guest SDK ergonomics, and macro generation.
- Replace the old queue-centric model with a shared-memory-first primitive substrate.
- Define a compound capability scope model that can combine tenant, URI, locality, and explicit resource selectors.
- Make guest communication pattern-oriented, with request/reply treated as one pattern among several.
- Define generic, config-driven bootstrap for system guests so the host runtime does not encode guest-specific logic.

**Non-Goals:**
- Implementing any of the crates in this change.
- Finalising every wire type for every hostcall.
- Specifying the behaviour of individual system guests in detail.
- Defining distributed replication or cluster-scaling semantics beyond the foundation needed to support them later.

## Decisions

### 1. `selium-abi` becomes an explicit architectural layer

**Decision:** The host/guest contract is specified as its own capability, not folded implicitly into host implementation details.

**Rationale:** The ABI is the seam that both `selium-runtime` and `selium-guest` depend on. Treating it as a first-class contract keeps crate boundaries explicit and prevents system guest behaviour from depending on undocumented host assumptions.

**Alternative considered:** Keep the ABI as a runtime-owned detail.
- Rejected because it hides the contract that all guest-side ergonomics rely on.

### 2. The primitive substrate is shared-memory-first, not queue-first

**Decision:** `selium-kernel` exposes shared memory, signalling, network, storage, and process primitives. Higher-level channels and messaging patterns are composed above those primitives in `selium-guest`.

**Rationale:** Wasmtiny supports hot-pluggable shared memory, so the old queue workaround should not dictate the whole architecture. This keeps the host side primitive and lets the guest SDK decide how to compose transport, durability, and coordination.

**Alternative considered:** Preserve queues as the central primitive and layer all communication above them.
- Rejected because it keeps an old runtime workaround at the centre of a new architecture.

### 3. `selium-kernel` and `selium-runtime` have distinct jobs

**Decision:** `selium-kernel` owns primitive resource types and low-level operations. `selium-runtime` owns Wasmtiny execution, process lifecycle orchestration, config-driven bootstrap, capability enforcement, session persistence, activity-log projection, and metering integration.

**Rationale:** This split keeps the host "dumb" in the architectural sense while still giving one layer responsibility for execution and bootstrap. Kernel remains primitive; runtime remains generic.

**Alternative considered:** Collapse kernel and runtime into one host crate.
- Rejected because it blurs execution concerns with primitive resource contracts and makes future host evolution harder to reason about.

### 4. Capability scopes use intersection semantics across selector kinds

**Decision:** Grants are modelled as capability plus one or more selectors, where selectors can include tenant, URI prefix, host locality, resource class, and explicit resource identifiers. Effective authority is the intersection of all provided selectors.

**Rationale:** No single selector kind is expressive enough on its own. Intersection semantics allow broad system authority and narrow user authority to be represented within one model.

**Alternative considered:** Separate scope systems for resources, tenants, and URIs.
- Rejected because it fragments the security model and complicates guest-side reasoning.

### 5. `selium-guest` owns the messaging-pattern layer

**Decision:** The guest SDK provides fabric and pattern abstractions such as pub/sub, fanout, request/reply, streams, and live tables. Request/reply may back RPC-style APIs, but RPC is not treated as a privileged substrate.

**Rationale:** `ARCHITECTURE.md` already frames messaging as overlays. Treating request/reply as only one pattern keeps Selium aligned with streaming and channel composition rather than collapsing back into service-RPC design.

**Alternative considered:** Make RPC the default inter-guest abstraction and model other patterns as special cases.
- Rejected because it over-optimises for one interaction mode and weakens the broader messaging model.

### 6. System guest bootstrap is generic and config-driven

**Decision:** `selium-runtime` starts system guests from config descriptors containing module identity, entrypoint, bootstrap arguments, capability grants, scope grants, dependencies, and readiness conditions.

**Rationale:** The host should know how to bootstrap guests generically, not how to bootstrap "scheduler" or "discovery" specifically. This preserves the architecture's smart-guest / dumb-host direction.

**Alternative considered:** Hard-code startup logic for named system guests.
- Rejected because it bakes control-plane semantics into the runtime.

### 7. `selium-guest-macros` generates guest bootstrap and pattern metadata

**Decision:** Procedural macros generate entrypoint glue and metadata needed for discovery and guest-side pattern bindings.

**Rationale:** This keeps repetitive ABI glue out of guest code while still making metadata generation part of the explicit foundation.

**Alternative considered:** Manual registration and handwritten boilerplate.
- Rejected because it is error-prone and undermines developer ergonomics.

## Risks / Trade-offs

- **[Shared-memory-first substrate complexity]** -> Start with primitive contracts and keep higher-level fabric composition in `selium-guest`, not in the kernel surface.
- **[Compound scope model may become hard to reason about]** -> Use one scope envelope with explicit intersection semantics rather than several partially overlapping systems.
- **[Pattern layer may leak primitive details]** -> Require the SDK to expose safe handles and typed APIs that hide primitive composition by default.
- **[Generic bootstrap may create circular startup dependencies]** -> Model dependencies and readiness explicitly in runtime configuration and keep startup ordering declarative.
- **[Proposal drift with existing system guest work]** -> Update `implement-system-guests` to depend on the new foundation and remove RPC-first framing.

## Migration Plan

1. Create foundation specs for all five capabilities in this change.
2. Amend dependent proposals, starting with `implement-system-guests`, so they target the new crate names and abstraction boundaries.
3. Implement the foundation crates before system guest implementation begins.
4. Use the new runtime bootstrap model to define system guest startup generically.
5. Layer system guest changes and future cluster work on top of the new contracts.

**Rollback:** Because this change is specification-only, rollback is limited to reverting the OpenSpec artifacts and dependent proposal edits.

## Open Questions

1. What is the minimum signalling primitive needed alongside shared memory to support efficient guest coordination?
2. Which messaging patterns should be part of day 1 `selium-guest`, and which should remain future overlays?
3. How much service or pattern metadata should `selium-guest-macros` emit for discovery?
4. Which activity-log and metering fields are mandatory for day 1 versus later extensions?
