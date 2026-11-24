//! Cross-guest wake probe fixture guest.
//!
//! Used by the `selium-runtime` `cross_guest_wake` integration test to verify
//! how a parked guest task behaves: that a *write from another guest process*
//! wakes it, that polling it costs it nothing, and that a yield-only pass does
//! not re-enter it.
//!
//! Four roles, selected by the integer entrypoint argument:
//!
//! - [`MODE_PUBLISHER`] creates a channel, announces its region id, then
//!   publishes one frame per interval forever (staying resident so its writer
//!   registration keeps `writer_count` above zero — a reader sees EOF, not a
//!   park, once the last writer withdraws).
//! - [`MODE_PARKER`] creates a channel and parks on it without ever
//!   publishing, so an attached watcher has nothing that will ever wake it.
//! - [`MODE_WATCHER`] attaches to an announced region id and parks on a read.
//!   Its reader starts at the live tail, so it can only be woken by a frame
//!   published *after* it attached. It logs once a frame arrives, so the log
//!   line is proof that the wake landed.
//! - [`MODE_YIELDER`] yields forever and counts its yields, so the test can
//!   measure how much of the guest's execution a host poll drives.

use anyhow::{Context as _, bail};
use selium_guest::{
    AttachDeclarationScope, Instant, ResourceClass, Timer, declare_attachable_by_resolution,
    entrypoint, info, process_id, record_resolved_resource_for,
};
use selium_shm::{Channel, ChannelBackpressure, ShmTransport};
use selium_wire::framed::{FramedRead, FramedWrite};

/// Ring data capacity for the probe channel.
const CHANNEL_CAPACITY: u64 = 4096;
/// The parker role: create the channel and park on it without ever
/// publishing. A watcher attached to a parker's channel therefore parks
/// forever, which is what a host-polling-budget test needs: nothing will
/// legitimately wake it, so any budget it spends is spent purely on being
/// polled.
const MODE_PARKER: u64 = 2;
/// The publisher role: create the channel and publish frames.
const MODE_PUBLISHER: u64 = 0;
/// The watcher role: attach to the publisher's channel and park on a read.
const MODE_WATCHER: u64 = 1;
/// The yielder role: a task that only ever yields, forever. Used to pin the
/// host-side yield accounting — a pass that merely re-arms a yielded task is
/// not forward progress, so a polling host must not re-enter on it.
const MODE_YIELDER: u64 = 3;
/// Publish cadence of the publisher role.
const PUBLISH_INTERVAL_NANOS: u64 = 100_000_000;
/// How often the yielder reports progress, in yields.
const YIELD_LOG_INTERVAL: u64 = 100;

/// Creates a channel, announces its region id, and declares it attachable so
/// a [`MODE_WATCHER`] peer can record a resolve and attach — a peer attach is
/// authorised, not privileged.
fn announced_channel() -> anyhow::Result<Channel> {
    let channel = Channel::create(CHANNEL_CAPACITY, ChannelBackpressure::Park)
        .context("wake-probe: create")?;
    let region_id = channel.region_id();
    info!("wake-probe: channel {region_id}");
    declare_attachable_by_resolution(
        ResourceClass::SharedRegion,
        region_id,
        AttachDeclarationScope::Any,
    )
    .context("wake-probe: declare attachable")?;
    Ok(channel)
}

/// Creates a channel nobody writes to and parks on it forever.
async fn parker() -> anyhow::Result<()> {
    let channel = announced_channel()?;
    let transport = ShmTransport::new(&channel, &channel).context("wake-probe: transport")?;
    let mut read = FramedRead::new(transport);
    let (_payload, _tag, _flags) = read
        .read_frame_async()
        .await
        .context("wake-probe: parker read")?;
    info!("wake-probe: woke");
    Ok(())
}

/// Creates the channel, announces its region id, and publishes forever.
async fn publisher() -> anyhow::Result<()> {
    let channel = announced_channel()?;
    let transport = ShmTransport::new(&channel, &channel).context("wake-probe: transport")?;
    let mut write = FramedWrite::new(transport);
    let mut sequence: u32 = 0;
    loop {
        let payload = format!("tick-{sequence}");
        write
            .write_frame_with_flags_async(payload.as_bytes(), sequence, 0)
            .await
            .context("wake-probe: publish")?;
        // One log line per publish: the test uses these as the publisher's
        // liveness signal and to time the consumer's wake against the publish
        // that caused it.
        info!("wake-probe: tick {sequence}");
        sequence = sequence.wrapping_add(1);

        let now = Instant::now().context("wake-probe: clock")?;
        let deadline = now
            .checked_add(std::time::Duration::from_nanos(PUBLISH_INTERVAL_NANOS))
            .unwrap_or(Instant::MAX);
        Timer::new(deadline).await?;
    }
}

#[entrypoint]
async fn wake_probe(mode: u64, region: (u64, u64)) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    match mode {
        MODE_PUBLISHER => publisher().await,
        MODE_PARKER => parker().await,
        MODE_YIELDER => yielder().await,
        MODE_WATCHER => {
            // SAFETY: the runtime wrote these bytes into this guest's linear
            // memory for this entrypoint invocation.
            let region_id = unsafe { selium_guest::args::str(region.0, region.1) }
                .context("wake-probe: region id argument")?
                .parse::<u64>()
                .context("wake-probe: region id is not a u64")?;
            watcher(region_id).await
        }
        other => bail!("wake-probe: unknown mode {other}"),
    }
}

/// Attaches to `region_id` and parks until a frame arrives, then logs it.
async fn watcher(region_id: u64) -> anyhow::Result<()> {
    record_resolved_resource_for(ResourceClass::SharedRegion, region_id, process_id())
        .context("wake-probe: record resolve")?;
    let channel = Channel::attach(region_id).context("wake-probe: attach")?;
    let transport = ShmTransport::new(&channel, &channel).context("wake-probe: transport")?;
    let mut read = FramedRead::new(transport);
    info!("wake-probe: watching");

    let (payload, _tag, _flags) = read
        .read_frame_async()
        .await
        .context("wake-probe: read frame")?;
    info!(
        "wake-probe: woke {}",
        String::from_utf8_lossy(&payload).trim()
    );
    Ok(())
}

/// Yields forever, logging every [`YIELD_LOG_INTERVAL`] yields.
async fn yielder() -> anyhow::Result<()> {
    info!("wake-probe: yielding");
    let mut yields: u64 = 0;
    loop {
        selium_guest::yield_now().await;
        yields += 1;
        if yields.is_multiple_of(YIELD_LOG_INTERVAL) {
            info!("wake-probe: yielded {yields}");
        }
    }
}
