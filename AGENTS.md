# AGENTS.md

> **Guidelines for AI coding agents contributing to Selium**

Selium is a channel‑native compute fabric. Guests are WASM services with typed ports; all I/O is via channels; no ambient authority; Flatbuffers only.

This document defines what agents may change, how to do it, and what "done" means.

---

## Prime directives (do not break these)
 - **Channels‑only I/O.** No sockets, no WASI/POSIX.
 - **Flatbuffers on wires.** Public wires and persisted artefacts use Flatbuffers; internal code uses idiomatic Rust types.
 - **No use of WASI toolchain.** Code and docs should never reference `wasm(32|64)-wasi-*`.
 - **Strict capabilities.** Guests may only do what their capability bundle allows. No ambient lookups.
 - **Stable Rust only.** Annotate public items with Rustdoc. Log via tracing.
 - **Small, reviewable patches (unless specifically required by a human developer).** Never push or commit; output patches and file diffs only.

---

## Coding standards

 - **Edition:** Rust 2024.
 - **Lint gate:** `cargo clippy -- -D warnings` must be clean.
 - **Fmt:** `cargo fmt` required.
 - **Tests:** `cargo test --all-targets` must pass.
 - **Logging:** `tracing` only.
 - **Async:** `tokio` (multi‑thread runtime).
 - **Error handling:** `anyhow` for binaries. `thiserror` for libraries.
 - **Error handling (strict):**
   - Do not suppress `Result` values. Never write `let _ = some_result;` or otherwise drop errors. Propagate with `?` and map with `map_err`/`context` as appropriate.
   - Do not use `unwrap`/`expect` in host code. Handle errors gracefully and return typed errors; panics in host paths create denial‑of‑service risk. Exception: permitted inside `#[test]` scopes. Explicit human permission required for any other exceptions.
 - **Unsafe:** avoid; if necessary, encapsulate and document reasoning.
 - **Human language:** all documentation, comments, binding names etc. should use International English.
 - **Order of code:** each Rust code file should be laid out in the following order:
    1. Module/library doc comments
    2. `use` statements
    3. `type` definitions
    4. `const` and `static` definitions
    5. `trait` definitions
    6. `struct` and `enum` definitions
    7. `impl` blocks
    8. Free functions
    9. Tests (in a `#[cfg(test)] mod tests` block at the end of the file)
 - **Documentation:** all public items must have `///` Rustdoc comments

---

## Flatbuffers policy

 - Public wires and cross‑process/persisted artefacts are Flatbuffers. No JSON for runtime wires.
 - Keep generated Rust modules checked in (build must not require network).
 - Schema ids: compute a 16‑byte BLAKE3 of the .fbs file content. The `#[schema]` macro must emit a const with this id for use in port metadata.

---

## Security invariants

 - **Capabilities:** every I/O handle the guest uses must be backed by a capability entry minted by the authoriser; host validates on each call.
 - **No ambient authority:** forbid any API that opens Catalogue paths or channels by string unless explicitly authorised.
 - **Isolation:** one wasmtime store per instance; configure memory caps; pooling; fuel/epoch interruption to bound CPU.

---

## Pre‑finish checklist (every patch)
 - `cargo fmt` (after every changeset)
 - `cargo clippy -- -D warnings` clean
 - `cargo test --all-targets` passes
 - No flexbuffers, no bincode, no ambient I/O
 - Public items have /// Rustdoc
 - Flatbuffers root/file ids intact (SDIC/SCAT/STOP/SBND/SCAP)
 - If schemas changed: generated code updated and checked in
 - If bindings/rules changed: tests updated or added
 - No suppressed `Result`s; no `unwrap`/`expect` in host code (tests are exempt)

---

## What NOT to do
 - ❌ Push commits or rewrite git history
 - ❌ Introduce network, filesystem, or POSIX/WASI dependencies for guests
 - ❌ Add heavy dependencies casually
 - ❌ Bypass capability checks or create "debug backdoors"
 - ❌ Hide transforms/rules in opaque “network” layers; transforms must be explicit services
 - ❌ Use American English
 - ❌ Suppress `Result`s (e.g., `let _ = some_result;`). Propagate with `?` and map errors.
 - ❌ Use `unwrap`/`expect` in host code (except inside `#[test]` scopes). Convert to fallible flows and surface errors.
 - X Use `thiserror` in binary crates. Use `anyhow`.
 - X Use `anyhow` in library crates. Use `thiserror`.

---

## Contact the humans

When trade‑offs are non‑obvious, prefer a TODO with a short rationale:

```rust
// TODO(@maintainer): We could adopt the channel's schema on first write.
// Leaving strict-equality for now to avoid accidental schema drift.
```

---

Thank you! Following these rules keeps Selium simple, safe, and pleasant to work in.
