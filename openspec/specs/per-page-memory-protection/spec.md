## Purpose

Define per-page memory protection for shared-region attachments: reader-slot cursor pages are the only writable pages for consumers, producers get full access, and enforcement happens in the OS kernel via `mprotect` rather than runtime software checks.

## Requirements

### Requirement: Per-Page Memory Protection on Attach
When a guest attaches to a shared region with a `reader_slot` parameter, the host SHALL map the region such that only the designated reader cursor page is writable; all other pages SHALL be mapped read-only.

#### Scenario: Consumer attaches with reader slot
- **WHEN** a guest calls `attach_region` with `reader_slot: Some(3)`
- **THEN** the host SHALL map the region `PROT_READ` on all pages except page 3, which SHALL be mapped `PROT_READ | PROT_WRITE`

#### Scenario: Consumer attempts write to data page
- **WHEN** a consumer guest with a reader-slot-protected mapping attempts to store to a data page
- **THEN** the store SHALL trap with a memory protection fault

#### Scenario: Consumer writes to its own cursor page
- **WHEN** a consumer guest writes to its designated reader cursor page
- **THEN** the store SHALL succeed and update the cursor value

#### Scenario: Consumer attempts write to another reader's cursor page
- **WHEN** a consumer guest attempts to store to a reader cursor page it was not assigned
- **THEN** the store SHALL trap with a memory protection fault

### Requirement: Producer Full Access
A producer attaching without a `reader_slot` SHALL receive full read-write access to all pages in the shared region.

#### Scenario: Producer attaches without reader slot
- **WHEN** a guest calls `attach_region` with `reader_slot: None`
- **THEN** the host SHALL map the entire region `PROT_READ | PROT_WRITE`

### Requirement: Protection Is Kernel-Enforced
All memory protection SHALL be enforced by the operating system kernel via `mprotect`, not by runtime software checks.

#### Scenario: Malicious guest bypass attempt
- **WHEN** a guest attempts to write to a read-only shared page via any WASM store instruction
- **THEN** the kernel SHALL deliver `SIGSEGV` and the runtime SHALL translate it to a WASM trap
