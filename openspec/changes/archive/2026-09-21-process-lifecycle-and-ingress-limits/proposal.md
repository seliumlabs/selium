# Proposal

## Why

The bridge's stream-flood defence is a per-identity, *lifetime* spawn cap, and it does not actually bound the damage: a bridge-channel whose pipe tears down never exits its WASM process (its entrypoint completes silently and no one observes it), so its budget slot never refills — and process count is the one resource dimension with no host-enforced quota. Whether driven by an attacker minting QUIC streams or a broken-but-legitimate API orchestrator, the platform can accumulate idle processes and pay per-stream region/queue churn with no correct limiter.

## What Changes

- **Guest lifecycle (fixes the zombie bug):** entrypoint completion becomes the process lifetime. When a poll-owner entrypoint future completes, the process terminates and the runtime reaps it — recording `ProcessExited` and releasing its reservations — so a bridge-channel closes itself once its downstream connection terminates. Remaining stuck/trapped guests are still reaped by the supervisor as a last resort.
- **Per-tenant process quota:** the runtime gates `ProcessStart` on a per-tenant `ResourceClass::Process` quota (denying over-ceiling spawns with `QuotaExceeded`) and releases the slot when the process exits. The accountant authors the ceiling — a sane default (100) that operators can raise per tenant on request.
- **Bridge budget delegation:** the bridge-server drops its guest-local, never-refilling `SpawnBudget` in favour of the host-enforced quota, surfacing a denied spawn to the client as attach-then-close (stream EOF) rather than tracking spawns itself.
- **Edge admission limits:** the QUIC connector caps concurrent bidirectional streams per connection and rate-limits new stream admissions per tenant (token bucket with burst), refusing cheaply before a region is allocated so a flood costs the least possible before it reaches any guest.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `selium-runtime`: entrypoint completion terminates and reaps the process; `ProcessStart` enforces a per-tenant process quota released on exit.
- `selium-guest`: entrypoint completion semantics — a completed poll-owner entrypoint signals process completion instead of idling.
- `selium-guest-macros`: the generated `__selium_guest_poll` export returns the reactor completion code (`i32`) instead of unit.
- `selium-accountant`: the accountant authors the per-tenant process-quota ceiling (default 100, operator-raisable).
- `guest-bridge`: the spawn bound is delegated to the host process quota; the guest-local lifetime spawn budget is removed.
- `quic-connector`: per-tenancy stream-admission rate limit and per-connection bidirectional stream cap, with cheap pre-allocation refusal.

## Impact

- **Code:** `crates/runtime` (process lifecycle, quota enforcement on `ProcessStart`, teardown release, `__selium_guest_poll` completion handling), `crates/guest` + `crates/guest/macros` (completion signal export), `guests/accountant` (process-quota authoring), `guests/bridge` (remove `SpawnBudget`), `guests/connector-quic` (token-bucket admission, per-connection stream cap).
- **ABI:** `__selium_guest_poll` returns a completion code rather than unit; no new `HostcallRequest`/`HostcallOutput` variants are expected (reuses `QuotaSet`/`QuotaClear`, `ProcessExited`, and the existing `ResourceClass::Process`); a new `AccountantControl::SetProcessQuota` operator-control message authors the process ceiling.
- **Behaviour:** completed guests now exit rather than idling; operators gain a per-tenant process ceiling (default 100, raisable); the edge admits streams at a bounded rate and refuses overflow before allocation. Over-capacity client behaviour is a clean stream close / refusal, unchanged in kind from today.
