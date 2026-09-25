# AGENTS.md

Agentic coding guidelines for this Rust workspace.

**Read [`DESIGN-INTENT.md`](DESIGN-INTENT.md) first.** It codifies the
project's non-negotiables (WASM-only guests, no WASI, hot-swap shared
memory, fenced wasmtiny, dumb host/smart guest, everything-is-a-channel),
the rejected alternatives you must not re-propose, and the invariants for
evaluating changes. Rules there override defaults from your training data.

## Build Commands

```bash
# Build all crates
cargo build --workspace

# Build the guest SDK and system guests for WASM
cargo build --target wasm32-unknown-unknown -p selium-guest -p selium-discovery -p selium-spine-demo

# Release build
cargo build --release --workspace
```

## Lint Commands

```bash
# Format code
cargo fmt --all

# Run clippy with strict warnings
cargo clippy --workspace --all-targets -- -D warnings
```

## Test Commands

```bash
# Run all tests (workspace, all targets including doc tests)
cargo test --workspace --all-targets

# Run the golden-path spine test (requires the wasm32 guest build first)
cargo build --target wasm32-unknown-unknown -p selium-spine-demo
cargo test -p selium-runtime --test spine -- --ignored

# Run single test by name
cargo test test_name_here -- --nocapture

# Run tests for specific crate
cargo test -p selium-runtime
cargo test -p selium-guest
```

## CRITICAL IMPERATIVES

- **Rust Edition 2024 only** - Use 2024 edition features. Do not use 2021 edition patterns.
- **NO WASI** - Never use `wasm32-wasi` target. Use `wasm32-unknown-unknown` exclusively.
- **Pre-commit checks** - Before creating a commit/PR, you MUST run:
  1. `cargo fmt --all`
  2. `cargo clippy --workspace --all-targets -- -D warnings`
  3. `cargo test --workspace --all-targets`
  4. `cargo build --target wasm32-unknown-unknown -p selium-spine-demo -p selium-discovery`
  5. `cargo test -p selium-runtime --test spine -- --ignored`
  6. `scripts/check-wasm-patches.sh` (guards the ring/getrandom wasm patches against silent fallback to the JS-backed RNG; see `[patch.crates-io]` in `Cargo.toml`)
- **Workspace dependencies** - Use `[workspace.dependencies]` in root `Cargo.toml`. Do not pin different versions.
- **wasmtiny sibling checkout** - The workspace patches `wasmtiny` from `../../wasmtiny`. Keep the two repos side-by-side, and keep changes to the engine minimal and spec-driven.
- **International English only** - Do not use American English anywhere in the project unless required for calling third party APIs.
- **WASM page size is 64 KiB** - `WASM_PAGE_SIZE` (65536) is the page unit for region offsets; `RING_HEADER_SIZE` (4096) is the ring coordination-header layout constant. Never conflate them.

## Code Style

### Formatting
- Run `cargo fmt --all` before committing
- `rustfmt.toml` enforces `reorder_imports = true`
- Imports are ordered deterministically (no special grouping)

### Imports
```rust
// External crates first, then crate modules
use parking_lot::RwLock;
use std::collections::HashMap;

use crate::error::{Error, Result};
```

### Naming Conventions
- **Types/Enums**: `CamelCase` (e.g., `GuestId`, `CapabilityRegistry`, `StorageHandle`)
- **Functions/Methods**: `snake_case` (e.g., `next_guest_id()`, `register_capability()`)
- **Modules**: `snake_case` (e.g., `async_host`, `capabilities`)
- **Constants**: `SCREAMING_SNAKE_CASE` (e.g., `HOST_VERSION`)
- **Handle types**: `XxxHandle` pattern (e.g., `StorageHandle`, `NetworkHandle`)
- **ID types**: `XxxId` pattern (e.g., `GuestId`, `HandleId`, `TaskId`, `ProcessId`)
- **Private fields**: `snake_case` with no underscore prefix (e.g., `id: u64`)
- **Crate directories**: omit the "selium-" prefix (e.g. "selium-guest" becomes "guest/")

### Error Handling

**Library crates**: Use `thiserror`
```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Capability not found: {0}")]
    CapabilityNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
```

- Use `#[from]` for automatic error conversion
- Propagate with `?` operator
- Avoid `unwrap()`/`expect()` in production code
- DO NOT suppress unused results unless explicitly authorised by a human, e.g. `let _ =`
- When creating stubs for new functions, do not return fake values. Use the `todo!()` macro.

### Module Structure
- Public modules: `pub mod module_name;`
- Re-export frequently used items at crate root
- Group related functionality in submodules
- Place tests in `#[cfg(test)] mod tests` at end of file

### Documentation
- Crate-level doc comment: `//! Description`
- Module doc comments for public APIs
- No doc comments on private/internal functions
- Use inline `//` comments for complex logic only

### Async Code
- Use `#[tokio::test]` for async tests
- Prefer explicit error types over `Box<dyn Error>`
- Use `parking_lot` primitives (`RwLock`, `Mutex`) over std equivalents

### Conditional Compilation
- Use `#[cfg(target_arch = "wasm32")]` for WASM-specific code
- Use `#[cfg(not(target_arch = "wasm32"))]` for native test fallbacks
- Document why conditional compilation is needed

## Architecture Notes

### Crate roles
- `selium-abi`: canonical host/guest contract (capabilities, scopes, hostcalls, framing)
- `selium-kernel`: primitive host-side resources (shared memory, network, storage, processes, activity, metering)
- `selium-runtime`: wasmtiny-backed execution, sessions, capability enforcement, config-driven bootstrap of system guests
- `selium-guest`: ergonomic guest SDK (typed handles, codecs, tracing, async reactor)
- `selium-guest-macros`: procedural macros for entrypoints and interface metadata
- `selium-memory` / `selium-shm` / `selium-wire`: shared-memory substrate, ring channels, and transport-agnostic messaging patterns

### Design rules
- Keep `selium-kernel` primitive. Do not move guest policy or orchestration logic into it.
- Keep `selium-runtime` generic. It bootstraps guests from descriptors and readiness rules rather than hard-coded guest names.
- Keep `selium-abi` stable and explicit. Host and guest meet there first.
- Guest I/O is shared-memory-first; hostcalls are for control, not data.
- `AttachRegion` maps regions into the **calling guest's own memory** (via `HostCaller`), so guests can attach mid-entrypoint.
- **Ring protocol single-writer-domain rule**: the ring protocol (`selium-shm::layout`) is defined once and consumed by both guest-side and host-side code. Each ring MUST be single-writer-domain: all writers on a given ring operate within the same atomicity domain (guest hardware atomics OR host mutex-mediated atomics, never mixed). Mixed-domain writes are out-of-contract and may corrupt data. Readers may cross domains safely. This is asserted in debug builds where a domain tag is available.
```