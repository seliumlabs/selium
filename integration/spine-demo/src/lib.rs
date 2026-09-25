//! Golden-path demo guest.
//!
//! Exercises the spine of the platform end-to-end inside a real WASM guest:
//! log transport initialisation, shared-memory channel creation, a typed
//! pub/sub round trip, and readiness signalling. The `selium-runtime`
//! `spine` integration test deploys this guest and asserts on its output.

use anyhow::Context as _;
use selium_guest::entrypoint;
use selium_shm::{Channel, ChannelBackpressure, transport::ShmTransport};
use selium_wire::{
    framed::{FramedRead, FramedWrite},
    pubsub::{Publisher, Subscriber},
};

/// Channel capacity for the pub/sub round trip.
const DEMO_CHANNEL_CAPACITY: u64 = 4096;

#[entrypoint]
async fn spine_demo() -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    selium_guest::info!("hello spine");

    let channel = Channel::create(DEMO_CHANNEL_CAPACITY, ChannelBackpressure::Park)
        .with_context(|| "spine: channel create failed")?;

    // Create the subscriber transport before publishing so its reader starts
    // at the current tail and observes the message.
    let subscriber_transport = ShmTransport::new(&channel, &channel)
        .with_context(|| "spine: subscriber transport failed")?;
    let publisher_transport = ShmTransport::new(&channel, &channel)
        .with_context(|| "spine: publisher transport failed")?;

    let mut subscriber: Subscriber<String, ShmTransport> =
        Subscriber::new(FramedRead::new(subscriber_transport), None);
    let mut publisher: Publisher<String, ShmTransport> =
        Publisher::new(FramedWrite::new(publisher_transport));

    publisher
        .publish(&"ping".to_string())
        .with_context(|| "spine: publish failed")?;

    match subscriber.read_with_tag() {
        Ok((message, _tag)) if message == "ping" => {
            selium_guest::info!("spine: pubsub ok");
        }
        Ok((message, _tag)) => {
            selium_guest::error!("spine: unexpected message: {message}");
        }
        Err(error) => {
            selium_guest::error!("spine: subscribe failed: {error}");
        }
    }

    selium_guest::mark_ready();
    Ok(())
}
