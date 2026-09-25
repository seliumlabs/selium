//! The poll-owner entrypoint's completion-code contract.
//!
//! The generated `__selium_guest_poll` export is `#[cfg(target_family =
//! "wasm")]`-gated (its unmangled symbol would collide were two entrypoint
//! guest crates linked into one native binary), so the export itself is
//! exercised by the guest wasm builds (CI's wasm jobs and
//! `scripts/build-all.sh`) rather than by a native test. This test exercises
//! the exact contract the generated export implements: it delegates to
//! [`selium_guest::poll_safely`], which returns the reactor's completion code
//! — `0` while the poll-owner entrypoint is running, `1` once it completes.

use selium_guest::{entrypoint, poll_safely};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{0}")]
struct TestError(String);

#[test]
fn poll_export_reports_completion_code() {
    // With no poll owner installed yet, the reactor reports "running".
    assert_eq!(poll_safely(), 0);

    // The entrypoint export returns the synchronous exit code: 0 on `Ok(())`
    // (the `Err` exit code remains unchanged by the poll-completion work).
    // Running it installs a poll-owner entrypoint that completes immediately.
    assert_eq!(__selium_guest_entrypoint_poll_probe(), 0);

    // The (just-completed) poll-owner entrypoint is now reported as done on
    // the next poll — the host's reap signal for a returned guest entrypoint.
    assert_eq!(poll_safely(), 1);
}

/// A poll-owner entrypoint: its expansion emits the `__selium_guest_poll`
/// export with an `extern "C" fn() -> i32` signature on wasm.
#[entrypoint]
async fn poll_probe() -> Result<(), TestError> {
    Ok(())
}
