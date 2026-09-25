//! Shared helpers for the wasm-guest integration tests.
//!
//! The spine-style tests load `wasm32-unknown-unknown` guest modules from the
//! target directory. Guest wasm is **not mixed-version safe** against the
//! runtime: the guest ABI uses rkyv enums whose variant indices shift whenever
//! a variant is inserted, so a wasm module built against older sources
//! produces silent, incomprehensible failures at runtime (a guest whose
//! hostcalls decode as the wrong variants — readiness timeouts, traps, or
//! parked entrypoints) instead of a build error.
//!
//! [`read_guest_wasm`] and [`read_guest_wasm_debug`] therefore delegate
//! freshness to cargo — the only source of truth for "does this artifact
//! match its sources today". Before reading a module they run
//! `cargo build --target wasm32-unknown-unknown -p <guest>` for the requested
//! profile, which re-emits the wasm exactly when its inputs have changed and
//! no-ops when it is already current. This is a content- and
//! configuration-based check (source files, manifests, features, flags, rustc
//! version, target), so it neither false-positives on cosmetic edits nor
//! false-negatives on real ABI changes — unlike the previous mtime heuristic,
//! which compared the artifact's modification time against the newest file in
//! the dependency tree and could not tell a re-touch from a real change.

// Each integration test binary compiles this module independently and uses
// only the readers its guests need; per-binary, some readers are dead code.
#![allow(
    dead_code,
    reason = "shared test-support module compiled per test binary; readers are used across binaries"
)]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

/// Serialises this test binary's nested `cargo build` calls; parallel tests
/// within the binary don't thresh the target directory. Across test binaries,
/// cargo's own target-directory lock does the same job.
static BUILD_LOCK: Mutex<()> = Mutex::new(());
/// The target the guest wasm artifacts are built for.
const WASM_TARGET: &str = "wasm32-unknown-unknown";

#[derive(Clone, Copy)]
enum Profile {
    Debug,
    Release,
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Profile::Debug => f.write_str("debug"),
            Profile::Release => f.write_str("release"),
        }
    }
}

/// Reads a guest's wasm module, preferring (and building) the release profile.
///
/// Release is the recommended build for the interpreted-wasm spine tests that
/// are too slow at debug optimization (e.g. the TLS handshake in quic_spine),
/// and building it here also guarantees the artifact is fresh.
pub fn read_guest_wasm(crate_name: &str, wasm_file: &str) -> Vec<u8> {
    build_guest(crate_name, Profile::Release);
    read_artifact(crate_name, "release", wasm_file)
}

/// Reads a guest's wasm module from the debug profile exactly, building it
/// first.
///
/// For tests whose documented build recipe produces a debug artifact; using
/// the release-preferred reader would build and load the wrong profile.
pub fn read_guest_wasm_debug(crate_name: &str, wasm_file: &str) -> Vec<u8> {
    build_guest(crate_name, Profile::Debug);
    read_artifact(crate_name, "debug", wasm_file)
}

/// Ensures the guest is built for `WASM_TARGET` in the requested profile by
/// delegating to cargo. Cargo rebuilds only when the guest's inputs have
/// actually changed; otherwise this is a cheap no-op and the existing artifact
/// is already exactly what current sources produce.
#[expect(
    clippy::panic,
    reason = "a missing toolchain or failed guest build is a hard test failure"
)]
fn build_guest(crate_name: &str, profile: Profile) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut command = Command::new(cargo);
    command
        .current_dir(workspace_root())
        .arg("build")
        .args(["--target", WASM_TARGET])
        .args(["--package", crate_name]);
    if matches!(profile, Profile::Release) {
        command.arg("--release");
    }

    let output = command.output().unwrap_or_else(|error| {
        panic!("failed to run cargo to build the {crate_name} guest: {error}");
    });
    if !output.status.success() {
        panic!(
            "failed to build the {crate_name} guest ({profile}) for {WASM_TARGET}:\n\
             {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// Reads the built artifact, panicking (rather than silently mis-testing)
/// when cargo succeeded but the expected wasm file is still absent — a sign
/// the guest does not emit the `cdylib` this test expects.
#[expect(
    clippy::panic,
    reason = "a missing build artifact is a hard test failure"
)]
fn read_artifact(crate_name: &str, profile: &str, wasm_file: &str) -> Vec<u8> {
    let path = target_dir().join(WASM_TARGET).join(profile).join(wasm_file);
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "{crate_name} guest not found at {} after a successful build: {error}.\
             \nExpected the crate to emit this `cdylib` artifact for {WASM_TARGET}.",
            path.display()
        )
    })
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("target"))
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <workspace>/crates/runtime.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
