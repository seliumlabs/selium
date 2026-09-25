//! Discovery probe test fixture guest.
//!
//! Minimal application guest used by the discovery integration test.
//! Receives its bootstrap discovery `Context` (built by the entrypoint
//! macro, exercising the discovery rendezvous), allocates a shared-memory
//! region, logs its progress, and marks ready.
//!
//! Cross-guest shared-memory RPC wake is not yet implemented, so the probe
//! does not perform Tier-2 register/lookup through discovery. Those paths
//! are exercised by the existing `shm_transport` RPC tests.

use anyhow::Result;
use selium_guest::{Context, entrypoint};
use selium_shm::{Channel, ChannelBackpressure};

/// Channel capacity for the probe region.
const PROBE_CHANNEL_CAPACITY: u64 = 4096;

#[entrypoint]
async fn discovery_probe(mut _ctx: Context) -> Result<()> {
    drop(selium_guest::log::init());
    selium_guest::info!(guest = "discovery-probe", "booting");

    // Allocate a shared-memory channel — the runtime publishes Tier-1
    // registration events on the discovery feed for this region.
    let channel = Channel::create(PROBE_CHANNEL_CAPACITY, ChannelBackpressure::Park)?;
    selium_guest::info!(region_id = channel.region_id(), "probe: region allocated");

    selium_guest::info!("guest ready");
    selium_guest::mark_ready();

    Ok(())
}
