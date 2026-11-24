//! Timer probe test fixture guest.
//!
//! Used by the `selium-runtime` `timer` integration test to verify that a
//! real `wasm32` guest awaiting [`selium_guest::Timer`] (the `Sleep` hostcall)
//! is resumed at its deadline when nothing else happens in the platform: the
//! runtime's timer driver owns the deadline and delivers the wake through the
//! mailbox, so the guest is never re-entered by an external poll.
//!
//! The entrypoint *is* the poll owner, so a successful run proves the wake
//! landed: the process only exits once the timer fires and the entrypoint
//! returns.

use anyhow::Result;
use selium_guest::{Instant, Timer, entrypoint};

/// How long the probe sleeps before its timer must have fired.
const PROBE_NANOS: u64 = 250_000_000;

#[entrypoint]
async fn timer_probe() -> Result<()> {
    drop(selium_guest::log::init());
    selium_guest::info!("timer-probe: booted");

    let start = Instant::now()?;
    let deadline = start
        .checked_add(std::time::Duration::from_nanos(PROBE_NANOS))
        .ok_or_else(|| anyhow::anyhow!("timer-probe: deadline overflow"))?;
    Timer::new(deadline).await?;

    // Logged only after the deadline: a host that polls the guest would also
    // get here, so the test asserts the *wake latency* as well (see the test).
    selium_guest::info!("timer-probe: timer fired");
    Ok(())
}
