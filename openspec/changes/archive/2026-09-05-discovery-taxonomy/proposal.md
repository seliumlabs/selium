## Why

Discovery today is a process-keyed phonebook under a reserved `sel://_sys/proc/<pid>/…` namespace, with a fake `sel-proto://` route scheme, no aliases, no labels, and tenant scoping stubbed (`None`). Processes that allocate nothing register nothing and are invisible; there is no way to address a group of processes; and "find by name" is split across two incompatible URI grammars. This change replaces it with a deterministic, tenant-scoped URI taxonomy — one `sel://` grammar, leaf aliases, classification labels, an external-name registry, and wired tenant scoping — so guests, external clients, and the accountant all address resources through one surface.

## What Changes

- **Rigid URI schema (BREAKING)**: `sel://<tenant>/<type>/<id>`, where `<type>` is drawn from the closed `ResourceClass` set (`proc`, `region`, `queue`, …). The old `sel://_sys/proc/<pid>/…` and `sel-proto://` grammars are retired; arbitrary user-defined path segments are gone.
- **Root tenant**: system/runtime registrations live under the empty tenant (`sel:///proc/1001`, alias `sel:///discovery`). Guest registration requires a non-empty tenant — root is unreservable by guests by construction.
- **Leaf aliases**: `sel://<tenant>/<name>` resolves to a typed target `(type, id)`; aliases are one-hop names, and revoking a target revokes its aliases.
- **Classification labels**: each resource target carries `labels: [(key, value)]`; discovery answers label queries ("processes with `app=web`"). Grouping is classification, not a URI hierarchy.
- **Principal provenance**: resources are minted under the tenant they serve, not the allocating process's tenant; cross-tenant allocation is authorized by a root principal (trusted edge infrastructure, e.g. connectors minting stream channels under an authenticated client's tenant) or by tenant-scoped delegation.
- **Processes as first-class entries**: the runtime registers a process node at spawn and revokes it at teardown — no forgotten processes.
- **External names are opaque keys**: the `sel-proto://` family is dropped; external bindings register their real address (`https://acme.com/path/`, bare hostname for SNI) as an opaque name→target key that only the connector interprets.
- **Query surface**: exact resolve, prefix/wildcard listing, label queries, and external-name resolve — all tenant-scoped, with enumeration wired to the caller's tenant rather than stubbed, and failing closed when the caller's tenant cannot be verified.
- **Tier-2 revocation is authorized**: guests may revoke only their own custom registrations (aliases, external names) within their own tenant; typed URIs are revoked only over the Tier-1 feed, and staged teardown bookkeeping makes a failed stop retryable instead of silently lossy.

## Capabilities

### New Capabilities

None — this rewrites and extends the existing discovery contract in place.

### Modified Capabilities

- `discovery-registration`: the URI taxonomy, leaf aliases, labels, prefix/label/external-name queries, tenant-scoped enumeration, root-namespace reservation, process-node addressing, authorized Tier-2 revocation, and fail-closed tenant verification.
- `selium-abi`: `ResourceTarget` gains a resource class and labels; `DiscoveryRequest`/`DiscoveryResponse` gain enumeration-query variants; `HostQueueCreate` gains a serving tenant; the FlatBuffers codec decodes strictly.
- `selium-runtime`: registration URIs move to the new schema; process nodes register at spawn and revoke at teardown; `AllocRegion` and `HostQueueCreate` record the serving tenant under root-principal or delegated authority; well-known channels move to the root tenant; teardown bookkeeping is staged and retryable.
- `discovery-bootstrap`: the Tier-1 registration-flow scenarios are updated from the retired `sel://process/<id>/…` grammar to the new schema.
- `quic-connector`: SNI route resolution matches normalised bare external names (retiring `sel-quic://` matching), and cache eviction normalises the raw SNI.
- `guest-bridge`: the bridge-server registers its serving route as the leaf alias `sel://<tenant>/bridge` (retiring `sel-quic://<tenant>/bridge`).

## Impact

- **`selium-abi`**: `ResourceTarget` fields (`class`, `labels`); `DiscoveryRequest`/`DiscoveryResponse` variants; `HostQueueCreate` serving-tenant field — rkyv wire-format changes (BREAKING).
- **`selium-runtime`**: `discovery.rs` URI generation, bootstrap/hostcall registration paths, `AllocRegion`/`HostQueueCreate` principal/tenant propagation with root-principal and delegated authorization, process-node spawn/teardown with staged, retryable revocation, well-known channels move to the root tenant.
- **`guests/discovery`**: store rewrite — typed-id map, alias map with reverse revocation, label index, prefix/label query handling, wired tenant scoping, opaque external-name storage, authorized Tier-2 revocation, fail-closed caller verification.
- **Connectors** (`http`, `dns`, `quic`): route resolution moves from `sel-<proto>://` construction to normalized external-name lookup (and the bridge route becomes a leaf alias).
- **Out of scope** (deliberate): the accountant's audit-trail guest, a runtime `ProcessInspect` hostcall (containment stays runtime-side, composed in the SDK), and domain-binding authority verification (recorded as a known gap).
