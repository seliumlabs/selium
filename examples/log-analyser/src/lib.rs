use std::future::ready;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::{StreamExt, TryFutureExt, future::try_join_all};
use selium_atlas::{Atlas, Uri};
use selium_switchboard::{AdoptMode, Backpressure, Publisher, Subscriber, Switchboard};
use selium_userland::{
    entrypoint,
    io::SharedChannel,
    logging::{LogLevel, LogRecord},
    schema, time,
};
use tracing::{error, info, warn};
use window::WindowExt;

#[allow(warnings)]
#[rustfmt::skip]
mod fbs;
mod window;

#[schema(
    path = "schemas/log.fbs",
    ty = "examples.log_analyser.Warning",
    binding = "crate::fbs::examples::log_analyser::Warning"
)]
struct Warning {
    message: String,
}

const ERR_THRESHOLD: usize = 10;
const WARN_THRESHOLD: usize = 50;

/// Aggregates log traffic matching source pattern
#[entrypoint]
async fn analyser(
    ctx: selium_userland::Context,
    pattern: &str,
    alert_uri: &str,
    window_time: u32,
) -> Result<()> {
    let switchboard: Switchboard = ctx.require().await;
    let atlas: Atlas = ctx.require().await;

    let subscriber = Subscriber::<LogRecord>::create(&switchboard).await?;

    // Connect subscriber to log publishers
    try_join_all(atlas.lookup(pattern).await?.into_iter().map(|id| {
        let chan = unsafe { SharedChannel::from_raw(id) };
        switchboard
            .adopt_output_channel::<LogRecord>(chan, Backpressure::Drop, AdoptMode::Tap)
            .and_then(|id| subscriber.connect(&switchboard, id))
    }))
    .await?;

    let alerts = Publisher::create_with_backpressure(&switchboard, Backpressure::Drop).await?;
    let alert_uri = Uri::parse(alert_uri).context("Alert URI is invalid")?;
    atlas.insert(alert_uri, alerts.endpoint_id() as u64).await?;

    if window_time == 0 {
        return Err(anyhow::anyhow!("Window time must be at least 1 second"));
    }

    subscriber
        .filter_map(|res| ready(res.ok()))
        .window(Duration::from_secs(u64::from(window_time)))
        .filter_map(|window| ready(analyse_window(window)))
        .map(Ok)
        .forward(alerts)
        .await?;

    Ok(())
}

#[entrypoint]
async fn test_warnings() {
    loop {
        for _ in 0..60 {
            warn!("Test Warning");
        }

        if let Err(err) = time::sleep(Duration::from_secs(5)).await {
            error!(?err, "sleep failed");
            break;
        }
    }
}

#[entrypoint]
async fn test_errors() {
    loop {
        for _ in 0..20 {
            error!("Test Error");
        }

        if let Err(err) = time::sleep(Duration::from_secs(2)).await {
            error!(?err, "sleep failed");
            break;
        }
    }
}

fn analyse_window(window: Vec<LogRecord>) -> Option<Warning> {
    let mut errors = 0;
    let mut warnings = 0;
    for record in window {
        match record.level {
            LogLevel::Error => errors += 1,
            LogLevel::Warn => warnings += 1,
            _ => (),
        }
    }

    if errors > ERR_THRESHOLD {
        info!("{errors} errors observed in window");
        Some(Warning::new(format!("{errors} errors observed in window")))
    } else if warnings > WARN_THRESHOLD {
        info!("{warnings} warnings observed in window");
        Some(Warning::new(format!(
            "{warnings} warnings observed in window"
        )))
    } else {
        None
    }
}
