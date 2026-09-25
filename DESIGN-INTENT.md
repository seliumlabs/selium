# Design Intent

This document codifies the intentions of the Selium project. It exists so
that contributors — human or AI — make decisions consistent with the
project's aspirations rather than with whatever the default assumption in
our industry happens to be this year. Read it before proposing anything.

## The mission

Selium is a software-defined world: developers compose entire application
stacks in code — typed I/O, strict capabilities, zero external
configuration — and never touch traditional infrastructure, networking, or
orchestration files. In one sentence: **developers should own the whole
stack, and the stack should be software all the way down.**

The project's north star is **developer ergonomics**. Everything is
subservient to it, except where it introduces security vulnerabilities.

The deeper ambition is a rethink of the relationship between developers
and systems: if operating systems, kernels, networks, memory, and disks
were all abstracted away, what would the abstraction look like? Selium's
answer: **everything is a channel, and the only verb is attach.**

## Non-negotiables

These are load-bearing. Do not relitigate them without strong new
evidence.

1. **WASM guests are the primary and only execution target.**
   The guest is the unit of deployment, placement, and isolation. Building
   around this boundary from day one is deliberate: it prevents the SDK
   from accreting host-only idioms (e.g. `tokio::net`) that cannot cross
   into guests later. Native execution exists only as a test harness.

2. **No WASI.**
   WASI's trajectory is POSIX-compliance (sockets, files, fds) — the exact
   model this project replaces — and its cross-language neutrality forces
   least-common-denominator ABIs that cannot be made idiomatic in Rust.
   Guests use the `selium-abi` hostcall contract (rkyv-encoded) and the
   `selium-guest` SDK, both of which are ours to shape.

3. **Hot-swap shared memory is non-negotiable.**
   Guests attach to channels discovered at runtime (e.g. "subscribe to all
   worker logs" when workers are elastic). Without append-only attach of
   new shared regions into a running guest, resources would have to be
   declared upfront in configuration — the model this project exists to
   kill. This requirement is the sole reason wasmtiny exists; commodity
   engines cannot attach memory to a running instance.

4. **wasmtiny is fenced.**
   The engine's scope is: the smallest runtime that executes
   `wasm32-unknown-unknown` modules against `selium-abi`, plus append-only
   multi-memory attach. No JIT/AOT ambitions, no WASI, no snapshotting
   (restart-based recovery, like Kubernetes). It is scaffolding for the
   platform, not a product.

5. **Dumb host, smart guest.**
   The host provides primitives (memory, signalling, storage, time,
   lifecycle) and enforcement. Policy — scheduling, supervision,
   discovery, clustering — lives in system guests written against the same
   SDK as user guests. Dogfooding is how the SDK earns its ergonomics.

6. **One primitive, many overlays.**
   The shared-memory channel is the only IPC noun. Pub/sub, RPC, live
   tables, logs, network streams, and durable logs are overlays on it —
   not separate mechanisms. Control flows over hostcalls; data flows over
   shared memory.

7. **Capability-driven security, no ambient authority.**
   Every privileged action is gated by an explicit, scoped grant. If a
   capability cannot be enforced honestly, it is rejected loudly at grant
   time — never accepted and then silently denied at use time.

## Rejected alternatives (do not propose these again)

| Assumption | Why it is rejected | Do instead |
| --- | --- | --- |
| "Use wasmtime" | Cannot attach shared memory to a running instance; hot-swap is non-negotiable (point 3). | wasmtiny, fenced (point 4). |
| "Use WASI" | POSIX-shaped, least-common-denominator, slow-moving; the anti-goal of this project (point 2). | `selium-abi` + `selium-guest` SDK. |
| "Guests as native processes/dylibs" | Backs the SDK into host-only corners; multi-tenant isolation dies; the WASM boundary is the point (point 1). | Native execution as a test harness only. |
| "Standardise on tokio/std net" | WASM-incompatible; would make the WASM guest a second-class citizen forever. | Channel overlays + the guest reactor. |
| "Declare channels in config upfront" | Config-codified topology is what we are replacing; discovery-driven attach is the idiom. | Runtime discovery + hot attach. |
| "Text protocols for control" | Contradicts typed end-to-end I/O, the founding thesis. | Typed RPC (FlatBuffers/rkyv) everywhere. |
| "Guest snapshotting/migration" | Nice-to-have, not load-bearing; it derailed earlier prototypes. | Restart-based recovery via supervisor. |
| "Per-op hostcall data plane" | Tried; slow and ugly. Hostcalls are control; rings are data. | Shared-memory channels. |
| "Consensus/Raft early" | Where I/O fabrics go to die; live tables need an arbiter constraint long before quorum. | Single-writer/arbiter constraints first. |

## Invariants for contributors

When evaluating any proposal, apply these tests in order:

1. **Time-to-first-guest goes down.** If it makes a developer's first
   deploy→log-line loop slower or harder, it is wrong, regardless of
   elegance.
2. **Guests see pure software.** If a design leaks OS primitives (paths,
   fds, sockets, PIDs, signals-as-OS-concepts) into the guest model, it
   is wrong. Guests see channels, capabilities, URIs, and processes.
3. **Capabilities are honest.** A grant that cannot be enforced is
   rejected at grant time. Isolation by obscurity is not isolation.
4. **The channel stays the only noun.** New I/O patterns are overlays on
   the ring protocol, not new IPC mechanisms.
5. **Docs describe what runs.** If it does not execute in CI, it does not
   get documented as working. Delete docs that describe the next
   prototype.
6. **The golden path stays green.** WASM guest → bootstrap → channel →
   pub/sub → log line, in CI (`cargo test -p selium-runtime --test spine`).
   A feature that cannot be exercised there does not exist yet.

## Current state of truth

The spine works: real WASM guests bootstrap on wasmtiny, allocate and
attach shared memory, use typed channels, and stream logs to the host —
proven by the golden-path test. Networking, discovery wiring, AAA
enforcement, wake/wait, and multi-host are deferred with specs in
`openspec/changes/` (see `discovery-bootstrap-slice`, `channel-wake-wait`,
`consolidate-ring-protocol`, `aaa-capability-enforcement`). The README's
"What runs today" section is the authoritative status; trust it over any
other document, including this one.

## A note on prototypes

This repository has lived through several prototypes (worktree branches
`main`, `newkernel`, `newarch`, `arch2`, `arch3`). Each died in the
runtime/isolation layer, never in the I/O layer. The lesson that shapes
everything above: **make the isolation layer boring and minimal, spend the
savings on the fabric and the developer experience.** When in doubt, cut
scope from the engine and add it to the SDK.
