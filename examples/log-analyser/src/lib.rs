use std::future::ready;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{Context, Result};
use futures::{Sink, StreamExt, stream::SelectAll, task::noop_waker_ref};
use selium_atlas::{Atlas, Uri};
use selium_switchboard::{Backpressure, Publisher, Switchboard, SwitchboardError};
use selium_userland::{
    encoding::FlatMsg,
    entrypoint,
    io::{Channel, SharedChannel},
    logging::{LogLevel, LogRecord},
    schema, time,
};
use tracing::{error, info, warn};

#[allow(warnings)]
#[rustfmt::skip]
mod fbs;

const ERR_THRESHOLD: usize = 10;
const WARN_THRESHOLD: usize = 50;
const LOG_CHUNK_SIZE: u32 = 64 * 1024;

#[schema(
    path = "schemas/log.fbs",
    ty = "examples.log_analyser.Warning",
    binding = "crate::fbs::examples::log_analyser::Warning"
)]
struct Warning {
    message: String,
}

enum AlertSend {
    Sent,
    Dropped,
}

struct WindowState {
    start_ms: Option<u64>,
    window_ms: u64,
    window_ticks: u64,
    use_ticks: Option<bool>,
    window: Vec<LogRecord>,
    errors: usize,
    warnings: usize,
}

impl WindowState {
    fn new(window_ms: u64, window_ticks: u64) -> Self {
        Self {
            start_ms: None,
            window_ms,
            window_ticks,
            use_ticks: None,
            window: Vec::new(),
            errors: 0,
            warnings: 0,
        }
    }
}

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

    let sources: Vec<SharedChannel> = atlas
        .lookup(pattern)
        .await?
        .into_iter()
        .map(|id| unsafe { SharedChannel::from_raw(id) })
        .collect();

    info!(count = sources.len(), "log sources discovered");
    if sources.is_empty() {
        warn!(
            pattern,
            "no log sources matched; start the log producer first or check the log URI"
        );
    }

    let r = async {
        let mut readers = SelectAll::new();
        for source in sources {
            let channel = Channel::attach_shared(source).await?;
            let reader = channel.subscribe_weak(LOG_CHUNK_SIZE).await?;
            readers.push(reader);
        }

        let mut alerts =
            Publisher::create_with_backpressure(&switchboard, Backpressure::Drop).await?;
        let alert_uri = Uri::parse(alert_uri).context("Alert URI is invalid")?;
        atlas.insert(alert_uri, alerts.endpoint_id() as u64).await?;
        let mut dropped_alerts = 0usize;

        if window_time == 0 {
            return Err(anyhow::anyhow!("Window time must be at least 1 second"));
        }

        let window_ms = u64::from(window_time) * 1000;
        let window_ticks = u64::from(window_time);
        let state = WindowState::new(window_ms, window_ticks);

        readers
            .filter_map(|frame| {
                ready(match frame {
                    Ok(frame) => match LogRecord::decode(&frame.payload) {
                        Ok(record) => Some(record),
                        Err(err) => {
                            warn!(?err, "invalid log frame");
                            None
                        }
                    },
                    Err(err) => {
                        warn!(?err, "log channel read failed");
                        None
                    }
                })
            })
            .scan(state, |state, record| {
                let warnings = drain_windows(state, record);
                ready(Some(warnings))
            })
            .flat_map(futures::stream::iter)
            .for_each(|warning| {
                match try_publish_alert(&mut alerts, warning) {
                    Ok(AlertSend::Sent) => {}
                    Ok(AlertSend::Dropped) => {
                        dropped_alerts = dropped_alerts.saturating_add(1);
                        if dropped_alerts == 1 {
                            warn!("alert route missing; dropping alerts until connected");
                        }
                    }
                    Err(err) => {
                        warn!(?err, "failed to publish alert");
                    }
                }
                ready(())
            })
            .await;

        Ok(())
    };

    match r.await {
        Ok(_) => info!("ok"),
        Err(e) => error!(?e, "error bang!"),
    }

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

fn drain_windows(state: &mut WindowState, record: LogRecord) -> Vec<Warning> {
    let record_ts = record.timestamp_ms;
    let mut warnings = Vec::new();

    if state.use_ticks.is_none() {
        state.use_ticks = Some(record_ts < 1_000_000_000_000);
    }

    let window = if state.use_ticks.unwrap_or(false) {
        state.window_ticks
    } else {
        state.window_ms
    };

    let start_ms = state.start_ms.get_or_insert(record_ts);
    while record_ts.saturating_sub(*start_ms) >= window {
        if let Some(warning) = analyse(std::mem::take(&mut state.window)) {
            warnings.push(warning);
        }
        *start_ms = start_ms.saturating_add(window);
        state.errors = 0;
        state.warnings = 0;
    }

    match record.level {
        LogLevel::Error => state.errors += 1,
        LogLevel::Warn => state.warnings += 1,
        _ => {}
    }
    state.window.push(record);

    if state.errors > ERR_THRESHOLD || state.warnings > WARN_THRESHOLD {
        if let Some(warning) = analyse(std::mem::take(&mut state.window)) {
            warnings.push(warning);
        }
        state.errors = 0;
        state.warnings = 0;
        state.start_ms = Some(record_ts);
    }
    warnings
}

fn try_publish_alert(
    alerts: &mut Publisher<Warning>,
    warning: Warning,
) -> Result<AlertSend, SwitchboardError> {
    let waker = noop_waker_ref();
    let mut cx = TaskContext::from_waker(waker);
    let mut alerts = Pin::new(alerts);

    match Sink::poll_ready(alerts.as_mut(), &mut cx) {
        Poll::Ready(Ok(())) => {
            Sink::start_send(alerts.as_mut(), warning)?;
            match Sink::poll_flush(alerts, &mut cx) {
                Poll::Ready(Ok(())) | Poll::Pending => Ok(AlertSend::Sent),
                Poll::Ready(Err(err)) => Err(err),
            }
        }
        Poll::Ready(Err(err)) => Err(err),
        Poll::Pending => Ok(AlertSend::Dropped),
    }
}

fn analyse(window: Vec<LogRecord>) -> Option<Warning> {
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
