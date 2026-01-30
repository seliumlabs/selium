//! Demonstrates how to ingest, transform and egress realtime data.

use std::{future::ready, time::Duration};

use anyhow::Result;
use futures::{SinkExt, StreamExt, TryStreamExt};
use selium_atlas::{Atlas, PublisherAtlasExt, SubscriberAtlasExt, Uri};
use selium_switchboard::{Publisher, Subscriber, Switchboard};
use selium_userland::{Context, entrypoint, time};
use tracing::info;

const INGRESS: &str = "sel://example.org/data-pipelines/ingress";
const EVEN: &str = "sel://example.org/data-pipelines/even";

/// Generates data to be transformed by the pipeline
#[entrypoint]
async fn generator(ctx: Context) -> Result<()> {
    let switchboard: Switchboard = ctx.require().await;
    let atlas: Atlas = ctx.require().await;

    let mut publisher = Publisher::<i32>::create(&switchboard).await?;
    atlas
        .insert(Uri::parse(INGRESS).unwrap(), publisher.endpoint_id() as u64)
        .await?;

    // Emit a value every 0.5 seconds.
    let mut state: i32 = -1;
    loop {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        publisher.send(state as i32).await?;
        time::sleep(Duration::from_millis(500)).await?;
    }
}

/// Doubles the input value
#[entrypoint]
async fn double(ctx: Context) -> Result<()> {
    let switchboard: Switchboard = ctx.require().await;
    let atlas: Atlas = ctx.require().await;

    let ingress = Subscriber::<i32>::create(&switchboard).await?;
    SubscriberAtlasExt::connect(&ingress, &atlas, &switchboard, INGRESS).await?;

    let egress = Publisher::<i32>::create(&switchboard).await?;
    PublisherAtlasExt::matching(&egress, &atlas, &switchboard, EVEN).await?;

    ingress.map_ok(|item| item * 2).forward(egress).await?;

    Ok(())
}

/// Adds 5u32 to the input value
#[entrypoint]
async fn add_five(ctx: Context) -> Result<()> {
    let switchboard: Switchboard = ctx.require().await;
    let atlas: Atlas = ctx.require().await;

    let ingress = Subscriber::<i32>::create(&switchboard).await?;
    SubscriberAtlasExt::connect(&ingress, &atlas, &switchboard, INGRESS).await?;

    let egress = Publisher::<i32>::create(&switchboard).await?;
    PublisherAtlasExt::matching(&egress, &atlas, &switchboard, EVEN).await?;

    ingress.map_ok(|item| item + 5).forward(egress).await?;

    Ok(())
}

/// Filters by even numbers
#[entrypoint]
async fn even(ctx: Context) -> Result<()> {
    let switchboard: Switchboard = ctx.require().await;
    let atlas: Atlas = ctx.require().await;

    let ingress = Subscriber::<i32>::create(&switchboard).await?;
    atlas
        .insert(Uri::parse(EVEN).unwrap(), ingress.endpoint_id() as u64)
        .await?;

    ingress
        .filter_map(|item| ready(item.ok()))
        .filter(|item| ready(item % 2 == 0))
        .for_each(|item| {
            info!("Found even number: {item}");
            ready(())
        })
        .await;

    Ok(())
}
