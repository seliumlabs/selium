## MODIFIED Requirements

### Requirement: Per-Page Memory Protection on Attach
When a guest attaches to a shared region, the host MAY use per-page `mprotect` to grant writable access to specific pages within an otherwise read-only region. For channel consumers, the runtime SHALL attach with `reader_slot: None` and `RegionProt::ReadOnly`, relying on hostcall-mediated slot writes instead of per-page writable slots for reader position updates.

The per-page reader-slot mechanism (`reader_slot: Some(n)`) REMAINS SUPPORTED for backward compatibility and non-channel use cases, but is NOT the default for channel consumers.

#### Scenario: Consumer attaches with no reader slot (new default)
- **WHEN** a consumer guest attaches to a channel shared region
- **THEN** the retention SHALL use `reader_slot: None, prot: ReadOnly`, mapping the entire region `PROT_READ`
- **AND** position updates SHALL go through `write_slot` hostcall rather than direct memory stores

#### Scenario: Consumer attempts any write to shared region
- **WHEN** a consumer guest with a read-only mapping attempts any WASM store to any page in the shared region
- **THEN** the store SHALL trap with a memory protection fault

### Requirement: Producer Full Access
A producer attaching without a `reader_slot` SHALL receive full read-write access to all pages in the shared region.

#### Scenario: Producer attaches without reader slot
- **WHEN** a writer guest calls `attach_region` with `reader_slot: None` and `prot: ReadWrite`
- **THEN** the host SHALL map the entire region `PROT_READ | PROT_WRITE`

## REMOVED Requirements

### Requirement: Consumer Writes to Cursor Page (REMOVED)
The scenario where a consumer guest writes directly to a designated reader cursor page SHALL be replaced by hostcall-mediated slot writes. Consumers SHALL NOT receive any writable pages within shared regions.

**Reason**: Per-page read-only regions with a single writable cursor page provide page-level granularity only — any other consumer attached to the same region could corrupt another consumer's cursor if their slots share the writable page. Hostcall-mediated writes provide byte-level granularity with runtime-enforced ownership validation, eliminating this attack surface.
