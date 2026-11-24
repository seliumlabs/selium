# Proposal: Harden Platform Foundations

## Why

The engineering assessment found that Selium's architectural spine is sound, but memory safety, scoped authority, wake/cancellation behaviour, durable acknowledgement, protocol consistency, and verification honesty have not kept pace with feature breadth. The ordinary workspace suite is green while the required traceability gate is red and several high-impact paths lack regression coverage, so the project needs a coordinated correctness hardening change before further feature expansion or external release.

## What Changes

- Validate every untrusted hostcall and shared-memory layout before allocation or pointer use; make atomic APIs alignment-safe and deadline arithmetic non-panicking.
- Authorise certificate and delegation operations against the requested target and the ABI's intersection semantics; keep the root private key out of online runtime storage.
- Close wake, operation-lifetime, task-join, async-cancellation, timer, volume-append, quota-rollback, runtime-isolation, and shutdown defects with deterministic regression tests.
- Make store recovery and durable acknowledgement truthful across restart and I/O failure; apply the same rule to identity and accounting projections.
- Consolidate ring/frame logic behind the canonical protocol implementation; repair large-frame streaming, asymmetric channels, writer shutdown, framed-write continuation, and live-table mutation arbitration.
- Repair discovery replacement indexes, route-cache convergence, domain replay, and malformed HTTP framing.
- Preserve structured ABI errors and improve the unified guest SDK surface. **BREAKING**: guest timer completion and exhaustive guest-error matching may change where honest error propagation cannot be represented by the current APIs.
- Add an explicit host/guest ABI compatibility handshake. **BREAKING**: incompatible host and guest versions shall fail loudly instead of decoding requests as the wrong variants.
- Restore semantic verification: cover all scenarios with adjacent assertions or reviewed waivers, add real HTTP/CLI and component-level crash tests, pin toolchains, and correct gate scripts.
- Reconcile current-state documentation and active/main specs, define the shared-memory consumer trust boundary, and make release artifacts and crates.io publication reproducible and consumer-tested.

## Capabilities

### New Capabilities

- `release-engineering`: versioned, reproducible publication of host, guest, CLI, and system-guest artifacts with downstream consumer verification.

### Modified Capabilities

- `capability-enforcement`: target-scoped certificate authority and intersection-preserving delegation.
- `channel-wake-wait`: race-free mailbox and deadline wake semantics.
- `discovery-registration`: coherent replacement of labels, aliases, and reverse indexes.
- `framed-io`: cancellation-safe framed writes with no false payload acknowledgement.
- `guest-networking`: correct streaming across large frames, asymmetric rings, and shutdown.
- `guest-shared-memory`: aligned atomics and validated multi-memory layouts.
- `http-connector`: strict HTTP request boundaries and replacement-aware routing.
- `quic-connector`: replacement-aware route-cache convergence.
- `region-provider`: runtime-scoped native providers without cross-runtime global coupling.
- `ring-protocol-core`: one canonical implementation for all ring and multi-memory primitives.
- `selium-abi`: explicit host/guest ABI version negotiation and bounded hostcall envelopes.
- `selium-accountant`: failed durable writes are not acknowledged as applied policy.
- `selium-cli`: connection names match certificate-derived tenant routing and are covered end to end.
- `selium-guest`: structured errors, cancellation-safe clients, race-free joins, and authoritative table mutation.
- `selium-identity`: durable-before-visible principal/domain state and a genuinely offline root.
- `selium-kernel`: unique, ordered offsets for concurrent appends to one volume.
- `selium-rpc`: cancellation-safe session establishment and resource ownership.
- `selium-runtime`: bounded hostile input, transactional quotas, retryable state preparation, and bounded background lifecycle.
- `selium-store`: recovery before mutation and content-validated, slice-stable blob persistence.
- `sleep-hostcall`: overflow-safe and error-honest sleep behaviour.
- `verification-gate`: semantically adjacent traceability, real end-to-end paths, component crash injection, and reproducible tooling.

## Impact

- Core crates: `selium-abi`, `selium-memory`, `selium-shm`, `selium-wire`, `selium-kernel`, `selium-runtime`, and `selium-guest`.
- System guests: store, identity, accountant, discovery, HTTP/QUIC connectors, and the CLI client path.
- Public guest error/timer APIs and the host/guest compatibility contract; shared-memory layouts remain stable except where a versioned migration is explicitly required.
- Integration, failure-injection, cancellation, concurrency, fuzz, and crash-recovery suites; OpenSpec traceability and known-unknowns documentation.
- CI/toolchain configuration, release workflows, package metadata, README/current-state documents, and stale active specifications.
- The existing `hardened-consumer-shared-memory` change retains ownership of consumer write isolation; this change documents the present trust assumption and coordinates acceptance coverage rather than duplicating that implementation.
