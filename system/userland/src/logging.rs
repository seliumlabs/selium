//! Guest-side tracing integration that forwards events onto a dedicated logging channel.
//!
//! # Examples
//! ```no_run
//! fn main() -> Result<(), selium_userland::logging::InitError> {
//!     selium_userland::logging::init()?;
//!     tracing::info!("guest logging is ready");
//!     Ok(())
//! }
//! ```

use core::{cell::Cell, fmt};
use std::sync::{Mutex, OnceLock};

use flatbuffers::FlatBufferBuilder;
use futures::SinkExt;
use selium_userland_macros::schema;
use thiserror::Error;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    layer::{Context, Layer},
    prelude::__tracing_subscriber_SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

use crate::{
    r#async,
    fbs::selium::logging as fb,
    io::{Channel, Writer},
    process,
};

const MAX_RECORD_FIELDS: usize = 32;

static LOGGING: OnceLock<Result<LoggingState, InitError>> = OnceLock::new();

/// Severity levels carried by log records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[schema(
    path = "schemas/logging.fbs",
    ty = "selium.logging.LogLevel",
    binding = "crate::fbs::selium::logging::LogLevel"
)]
pub enum LogLevel {
    /// Trace-level diagnostics.
    Trace,
    /// Debug-level diagnostics.
    Debug,
    /// Informational records.
    Info,
    /// Warning records.
    Warn,
    /// Error records.
    Error,
    /// Unknown or future log level values.
    Unknown(i8),
}

/// Structured key/value pair included with a log record.
#[derive(Clone, Debug)]
#[schema(
    path = "schemas/logging.fbs",
    ty = "selium.logging.Field",
    binding = "crate::fbs::selium::logging::Field"
)]
pub struct LogField {
    /// Field name.
    pub key: String,
    /// Field value.
    pub value: String,
}

/// Span metadata attached to a log record.
#[derive(Clone, Debug)]
#[schema(
    path = "schemas/logging.fbs",
    ty = "selium.logging.Span",
    binding = "crate::fbs::selium::logging::Span"
)]
pub struct LogSpan {
    /// Span name.
    pub name: String,
    /// Span fields captured at emission time.
    pub fields: Vec<LogField>,
}

/// Log record emitted by Selium guests.
#[derive(Clone, Debug)]
#[schema(
    path = "schemas/logging.fbs",
    ty = "selium.logging.LogRecord",
    binding = "crate::fbs::selium::logging::LogRecord"
)]
pub struct LogRecord {
    /// Record severity.
    pub level: LogLevel,
    /// Target that emitted the record.
    pub target: String,
    /// Human-readable message.
    pub message: String,
    /// Structured fields attached to the record.
    pub fields: Vec<LogField>,
    /// Active span stack at the time of emission.
    pub spans: Vec<LogSpan>,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: u64,
}

/// Drops re-entrant logging events triggered while forwarding an earlier record.
struct ForwardingGuard;

#[derive(Default)]
struct LogLayer;

#[derive(Default)]
struct EventVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

struct LoggingState {
    channel: Channel,
    publisher: Mutex<Writer>,
    last_error: Mutex<Option<InitError>>,
}

#[derive(Debug, Error, Clone)]
/// Errors returned when initialising guest-side logging.
pub enum InitError {
    /// The `tracing` subscriber could not be installed.
    #[error("tracing subscriber initialisation failed: {0}")]
    Subscriber(String),
    /// Creating the logging channel failed.
    #[error("failed to create logging channel: {0}")]
    Channel(String),
    /// Registering the shared log channel with the process failed.
    #[error("failed to register logging channel: {0}")]
    Register(String),
    /// Creating the logging publisher failed.
    #[error("failed to create logging publisher: {0}")]
    Publisher(String),
    /// The logging publisher mutex was poisoned.
    #[error("logging state poisoned")]
    Poisoned,
}

impl ForwardingGuard {
    fn enter() -> Option<Self> {
        IS_FORWARDING.with(|flag| {
            if flag.get() {
                None
            } else {
                flag.set(true);
                Some(Self)
            }
        })
    }
}

impl Drop for ForwardingGuard {
    fn drop(&mut self) {
        IS_FORWARDING.with(|flag| flag.set(false));
    }
}

impl LoggingState {
    fn record_error(&self, err: InitError) {
        let mut slot = match self.last_error.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if slot.is_none() {
            *slot = Some(err);
        }
    }
}

impl<S> Layer<S> for LogLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        forward_event(event, &ctx);
    }
}

impl tracing::field::Visit for EventVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{value:?}"));
        } else {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }
    }
}

thread_local! {
    static IS_FORWARDING: Cell<bool> = const { Cell::new(false) };
}

/// Install the tracing layer that forwards events to the logging channel.
pub fn init() -> Result<(), InitError> {
    logging_state().map(|_| ())
}

/// Access the logging channel if initialisation succeeded.
pub fn channel() -> Option<Channel> {
    logging_state().ok().map(|state| state.channel.clone())
}

fn logging_state() -> Result<&'static LoggingState, InitError> {
    LOGGING
        .get_or_init(init_logging)
        .as_ref()
        .map_err(Clone::clone)
}

fn init_logging() -> Result<LoggingState, InitError> {
    let state = r#async::block_on(async {
        let channel = Channel::create(512 * 1024) // 512kb
            .await
            .map_err(|err| InitError::Channel(err.to_string()))?;
        let shared = channel
            .share()
            .await
            .map_err(|err| InitError::Channel(err.to_string()))?;
        process::register_log_channel(shared)
            .await
            .map_err(|err| InitError::Register(err.to_string()))?;
        let publisher = channel
            .publish()
            .await
            .map_err(|err| InitError::Publisher(err.to_string()))?;

        Ok(LoggingState {
            channel,
            publisher: Mutex::new(publisher),
            last_error: Mutex::new(None),
        })
    })?;

    tracing_subscriber::registry()
        .with(LogLayer)
        .try_init()
        .map_err(|err| InitError::Subscriber(err.to_string()))?;

    Ok(state)
}

fn forward_event<S>(event: &Event<'_>, ctx: &Context<'_, S>)
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    let Some(_guard) = ForwardingGuard::enter() else {
        return;
    };

    let Ok(state) = logging_state() else {
        return;
    };

    let mut visitor = EventVisitor::default();
    event.record(&mut visitor);
    let spans = collect_spans(ctx, event);
    let record = encode_record(
        event.metadata().target(),
        event.metadata().level(),
        visitor,
        spans,
    );

    if let Err(err) = publish_record(state, record) {
        state.record_error(err);
    }
}

fn collect_spans<S>(ctx: &Context<'_, S>, event: &Event<'_>) -> Vec<String>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    ctx.event_scope(event)
        .map(|scope| scope.map(|span| span.name().to_string()).collect())
        .unwrap_or_default()
}

fn publish_record(state: &LoggingState, record: Vec<u8>) -> Result<(), InitError> {
    let mut writer = state.publisher.lock().map_err(|_| InitError::Poisoned)?;
    r#async::block_on(async {
        writer
            .send(record)
            .await
            .map_err(|err| InitError::Publisher(err.to_string()))
    })
}

fn encode_record(
    target: &str,
    level: &Level,
    visitor: EventVisitor,
    spans: Vec<String>,
) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let fields = build_fields(&mut builder, &visitor.fields);
    let spans = build_spans(&mut builder, &spans);
    let target_offset = builder.create_string(target);
    let message_offset = builder.create_string(visitor.message.as_deref().unwrap_or_default());
    let record = fb::LogRecord::create(
        &mut builder,
        &fb::LogRecordArgs {
            level: map_level(level),
            target: Some(target_offset),
            message: Some(message_offset),
            fields,
            spans,
            timestamp_ms: now_ms(),
        },
    );
    builder.finish(record, None);
    builder.finished_data().to_vec()
}

fn build_fields<'bldr>(
    builder: &mut FlatBufferBuilder<'bldr>,
    fields: &[(String, String)],
) -> Option<
    flatbuffers::WIPOffset<
        flatbuffers::Vector<'bldr, flatbuffers::ForwardsUOffset<fb::Field<'bldr>>>,
    >,
> {
    if fields.is_empty() {
        return None;
    }

    let count = fields.len().min(MAX_RECORD_FIELDS);
    let mut offsets = Vec::with_capacity(count);
    for (key, value) in fields.iter().take(count) {
        let key_offset = builder.create_string(key.as_str());
        let value_offset = builder.create_string(value.as_str());
        offsets.push(fb::Field::create(
            builder,
            &fb::FieldArgs {
                key: Some(key_offset),
                value: Some(value_offset),
            },
        ));
    }
    Some(builder.create_vector(&offsets))
}

fn build_spans<'bldr>(
    builder: &mut FlatBufferBuilder<'bldr>,
    spans: &[String],
) -> Option<
    flatbuffers::WIPOffset<
        flatbuffers::Vector<'bldr, flatbuffers::ForwardsUOffset<fb::Span<'bldr>>>,
    >,
> {
    if spans.is_empty() {
        return None;
    }

    let mut offsets = Vec::with_capacity(spans.len());
    for span in spans {
        let name = builder.create_string(span.as_str());
        offsets.push(fb::Span::create(
            builder,
            &fb::SpanArgs {
                name: Some(name),
                fields: None,
            },
        ));
    }
    Some(builder.create_vector(&offsets))
}

fn map_level(level: &Level) -> fb::LogLevel {
    match *level {
        Level::TRACE => fb::LogLevel::Trace,
        Level::DEBUG => fb::LogLevel::Debug,
        Level::INFO => fb::LogLevel::Info,
        Level::WARN => fb::LogLevel::Warn,
        Level::ERROR => fb::LogLevel::Error,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    let now = std::time::SystemTime::now();
    now.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::from_secs(0))
        .as_millis() as u64
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    use core::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
