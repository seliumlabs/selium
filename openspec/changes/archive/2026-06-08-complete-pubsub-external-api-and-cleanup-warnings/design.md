## Context

Four independent pieces of unfinished work are producing build warnings. They span three crates and one entire guest crate. They're all "stubs for future work" rather than dead code, but none have spec coverage. This change creates specs for all four and outlines implementation approaches.

## Goals / Non-Goals

**Goals:**
- Implement pub/sub generation-change detection so `Subscriber` can detect overwritten data
- Expose `has_ready_frame` as a public async poll primitive for Quinn and other consumers
- Spec the external API text protocol, delegation pipeline, and its dependencies
- Clean up vestigial kernel fields or document their purpose
- Keep changes minimal — implement only what's needed to eliminate the warnings with working, spec-compliant code

**Non-Goals:**
- Full Quinn integration (covered by `add-sleep-hostcall-and-fix-quinn-timer`)
- Cross-process pub/sub testing (requires multi-guest runtime)
- Implementing the inbound network bridge itself (that's a runtime concern; this change specs the API side and stubs the runtime side)
- Implementing `SchedulerPlace`/`SchedulerStop`/`SchedulerScale` in the scheduler guest (separate follow-up)

## Decisions

### Decision 1: Generation-change detection via wrapped-counter comparison

The shared region's generation counter is a monotonically-increasing u64. The subscriber tracks `last_generation` — the generation value at the time of its last successful read. To detect overwrites:

```
if current_generation - last_generation > capacity {
    return Err(Error::Overwritten);
}
```

Since u64 subtraction wraps on overflow, and the counter monotonically increases, the difference is always correct modulo 2^64. The ring buffer capacity is the maximum data in flight; if the publisher has advanced the generation counter by more than capacity, the subscriber's position has necessarily been overwritten.

**Rationale:** Simple integer arithmetic, no modulo or position tracking needed. The generation counter already advances by frame count (each written frame increments it by 1), so the delta directly represents the number of frames written since the subscriber last read.

**Alternatives considered:**
- Track read position and compare to tail: requires the subscriber to track its own position relative to the ring, which is already partially done but more complex.
- Use a separate "overwritten" flag: adds write overhead on every publish.

### Decision 2: `poll_ready` on `StrongReader` wrapping `has_ready_frame`

`has_ready_frame` checks whether the next frame header has the READY flag set (after acquire fence). This is a non-blocking check. Expose it as:

```rust
impl StrongReader {
    pub fn poll_ready(&mut self) -> Result<bool> {
        self.has_ready_frame()
    }
}
```

For async consumers (Quinn), the pattern is: call `poll_ready`, if false, register a wake on the generation counter via `memory.atomic.wait32`, then re-poll. The `has_ready_frame` method already exists and works — it just needs to be called.

**Rationale:** No new logic needed, just a rename to `poll_ready` to follow Rust async naming conventions and document that it's non-blocking.

### Decision 3: External API text protocol

The external API accepts a simple whitespace-delimited text protocol over a TCP connection (or future QUIC stream). Grammar:

```
request = command [workload_id] [replicas]
command = "deploy" | "start" | "stop" | "scale" | "resolve"
workload_id = <non-whitespace string>
replicas = <unsigned integer>
```

The parsing pipeline is:

```
TCP bytes → String → parse_intent → UserIntent → decompose_intent → Vec<DelegatedInteraction>
```

Each `DelegatedInteraction` is dispatched to the appropriate guest over RPC:
- `DiscoveryResolve { uri }` → `discovery.request(DiscoveryRequest::Resolve(uri))`
- `SchedulerPlace { workload_id, replicas }` → `scheduler.request(SchedulerRequest::Place { ... })`
- `SchedulerStop { workload_id }` → `scheduler.request(SchedulerRequest::Stop { ... })`
- `SchedulerScale { workload_id, replicas }` → `scheduler.request(SchedulerRequest::Scale { ... })`

The API guest receives pre-connected RPC clients to discovery and scheduler via its `Context`, similar to how application guests receive a discovery client.

**Rationale:** The existing functions (`parse_intent`, `decompose_intent`, `accept_request`, etc.) already implement this pipeline correctly. They just need to be called from the entrypoint once a network bridge delivers TCP connections. The protocol is intentionally minimal — it's an MVP control plane, not a user-facing REST API.

### Decision 4: Inbound network bridge

The runtime configures a TCP listener (address from bootstrap config) and accepts inbound connections. For each connection, it spawns the external API guest instance (or routes to a pool). The connection's read/write halves are exposed to the guest via shared-memory ring buffers using the same `TcpStream` layout defined in `fix-shared-channel-state-and-rpc-net` (two rings: inbound + outbound).

For this change, we spec the API side fully and stub the runtime bridge. The runtime bridge implementation is deferred to when the TCP guest handles are working (dependent on `fix-shared-channel-state-and-rpc-net` being complete).

### Decision 5: Kernel `SharedMappingState` field cleanup

The `page_offset`, `prot`, and `reader_slot` fields in `SharedMappingState` are stored on creation but never read. Since `per-page-memory-protection` is enforced by wasmtiny at `map_shared_region` time (via `mprotect`), the kernel has no need to re-read these values — the OS enforces protection, and the kernel's job is done once the mapping is created.

**Remove them.** If a future feature needs to re-inspect mapping properties (e.g., for audit logging or live migration), the fields can be re-added with purpose. The remaining fields (`region_id`, `shared_id`) are the ones actively used for lookup and cleanup.

**Rationale:** Dead fields are misleading and produce warnings. The `per-page-memory-protection` spec is clear that enforcement is at the wasmtiny level. The kernel doesn't need a duplicate copy.

## Risks / Trade-offs

- **Generation-change detection accuracy**: The `current_generation - last_generation > capacity` check assumes the generation counter increments by exactly 1 per frame. If the counter ever increments by more than 1 (e.g., for multi-frame atomic batches), the check could produce false positives. **Mitigation:** Document the invariant that generation counter increments by 1 per frame. If multi-frame batches are added later, update the check.

- **External API protocol is human-readable but not versioned**: The text protocol has no version negotiation. A future breaking change would require a new port or a `/v2` prefix. **Mitigation:** Acceptable for MVP. Add a `version` command when needed.

- **Network bridge is stubbed**: The external API guest compiles but won't receive real connections until the runtime bridge is built. **Mitigation:** The spec covers both sides; the runtime task is explicitly deferred.

## Open Questions

- Should `has_ready_frame` be renamed to `poll_ready` or kept as-is? Leaning toward `poll_ready` for Rust convention.
- Should the external API use the same `Context` as application guests, or a dedicated `ApiContext` with pre-connected scheduler + discovery clients? Leaning toward a dedicated `ApiContext` since the API needs different bootstrap resources than a normal guest.
