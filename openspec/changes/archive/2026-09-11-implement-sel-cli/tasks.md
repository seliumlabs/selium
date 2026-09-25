## 1. Crate scaffolding

- [x] 1.1 Create `crates/cli` with package `selium-cli`, edition 2024, and `[[bin]] name = "sel"`; add it to the root `Cargo.toml` workspace members. Verify `cargo check -p selium-cli` succeeds.
- [x] 1.2 Add the dependency set from design.md section 2 (`selium-client`, `clap` with derive/help/std, `tokio` with macros and rt, `anyhow`) using workspace deps. Verify `cargo check -p selium-cli` succeeds and that `selium_client::selium_service::{ControlRequest, ControlResponse}` resolve from the crate alone.

## 2. CLI surface

- [x] 2.1 Define the clap surface (`Cli`/`Command`) with global flags `--tenant`, `--connector`, `--ca`, `--client-cert`, `--client-key` and the six subcommands (`deploy`, `scale`, `stop`, `status`, `resolve`, `upload`) per the spec, with tenant-derived `server_name = bridge.<tenant>` and `control_route = sel://<tenant>/control`. Verify a unit test parses `sel --tenant acme --connector 127.0.0.1:4433 status api` and derives `bridge.acme` and `sel://acme/control`.
- [x] 2.2 Mark `--connector` required with no default (design.md section 3). Verify a unit test shows parsing without `--connector` fails as a usage error.

## 3. Request mapping

- [x] 3.1 Implement the pure `build_request(&Command) -> Result<ControlRequest>` seam mapping every verb one-to-one to its `ControlRequest` variant (design.md section 6). Verify unit tests assert the exact request for `deploy`, `scale`, `stop`, `status`, and `resolve`.
- [x] 3.2 Implement `upload` byte-loading from `--file` inside the request seam, returning a typed error when the file cannot be read. Verify unit tests assert the bytes round-trip for an existing file and that a missing path errors without producing a request.

## 4. Failure and output mapping

- [x] 4.1 Implement the `render(&Command, ControlResponse) -> (String, ExitCode)` seam covering every variant: accepted-applied success with the verb's own outcome line (deploy/scale/stop), accepted-deferred failure (print `delegated.context`), `Error` (print `step: context`), `Status`/`Resolved` not-found, and the success verb outputs. Verify unit tests assert the rendered line and exit code for each variant, including `Deferred` mapping to non-zero.
- [x] 4.2 Wire `main` so success exits 0, any failure exits non-zero, and top-level `anyhow` errors print to stderr. Verify a unit test drives the render seam's exit codes and a manual `cargo run -p selium-cli --bin sel -- --help` prints usage without erroring.

## 5. Connection path

- [x] 5.1 Implement `ConnectOptions` construction from `--ca`, `--client-cert`, `--client-key`, and the derived `server_name`, parsing PEMs with `selium_client::certificates_from_pem` / `private_key_from_pem`. Verify a unit test builds options from the `guests/connector-quic/tests/fixtures` certificates without erroring.
- [x] 5.2 Implement the `run` path: `connect`, `client.rpc::<ControlRequest, ControlResponse>(route)`, one `request`, `render`, and handle drop after the single round trip (design.md section 4). Verify the crate compiles and the mapping seams stay independent of a live connection; the full external end-to-end test is deferred to a follow-up (documented in the crate doc comment).

## 6. Workspace verification

- [x] 6.1 Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p selium-cli`'s suite; verify all pass and that `cargo build --workspace` remains green (the CLI is a native crate and adds no wasm target work).
