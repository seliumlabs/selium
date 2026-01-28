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
use futures::{SinkExt, future::BoxFuture};
use selium_userland_macros::schema;
use thiserror::Error;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    layer::{Context, Layer},
    prelude::__tracing_subscriber_SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

#[cfg(target_arch = "wasm32")]
use crate::time;
use crate::{
    r#async,
    fbs::selium::logging as fb,
    io::{Channel, ChannelBackpressure, Writer},
    process,
};

const MAX_RECORD_FIELDS: usize = 32;

static LOGGING: OnceLock<Result<LoggingState, InitError>> = OnceLock::new();
static LOG_URI_REGISTRAR: OnceLock<Box<dyn LogUriRegistrar + Send + Sync>> = OnceLock::new();

/// Registers log channels with an external service using a URI.
pub trait LogUriRegistrar: Send + Sync {
    /// Register a shared log channel against the provided URI.
    fn register<'a>(
        &'a self,
        log_uri: &'a str,
        shared: crate::io::SharedChannel,
    ) -> BoxFuture<'a, Result<(), InitError>>;
}

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

struct PendingLogUri {
    uri: String,
    shared: crate::io::SharedChannel,
}

struct LoggingState {
    channel: Channel,
    publisher: Mutex<Writer>,
    last_error: Mutex<Option<InitError>>,
    pending_log_uri: Mutex<Option<PendingLogUri>>,
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
    /// Registering the shared log channel failed.
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
    init_with_log_uri(None)
}

/// Install the tracing layer, optionally registering the log channel with a URI registrar.
pub fn init_with_log_uri(log_uri: Option<&str>) -> Result<(), InitError> {
    logging_state_with_uri(log_uri).map(|_| ())
}

/// Access the logging channel if initialisation succeeded.
pub fn channel() -> Option<Channel> {
    logging_state().ok().map(|state| state.channel.clone())
}

/// Install the registrar that handles log URI registration.
pub fn set_log_uri_registrar(
    registrar: Box<dyn LogUriRegistrar + Send + Sync>,
) -> Result<(), InitError> {
    LOG_URI_REGISTRAR
        .set(registrar)
        .map_err(|_| InitError::Register("log URI registrar already set".to_string()))?;
    if let Some(state) = LOGGING.get()
        && let Ok(state) = state.as_ref()
        && let Some(pending) = take_pending_log_uri(state)?
    {
        r#async::block_on(async { register_log_uri(pending.uri.as_str(), pending.shared).await })?;
    }
    Ok(())
}

/// Report whether a log URI registrar is already configured.
pub fn log_uri_registrar_installed() -> bool {
    LOG_URI_REGISTRAR.get().is_some()
}

fn logging_state() -> Result<&'static LoggingState, InitError> {
    logging_state_with_uri(None)
}

fn logging_state_with_uri(log_uri: Option<&str>) -> Result<&'static LoggingState, InitError> {
    LOGGING
        .get_or_init(|| init_logging(log_uri))
        .as_ref()
        .map_err(Clone::clone)
}

fn init_logging(log_uri: Option<&str>) -> Result<LoggingState, InitError> {
    let state = r#async::block_on(async {
        let channel = Channel::create_with_backpressure(512 * 1024, ChannelBackpressure::Drop) // 512kb
            .await
            .map_err(|err| InitError::Channel(err.to_string()))?;
        let shared = channel
            .share()
            .await
            .map_err(|err| InitError::Channel(err.to_string()))?;
        process::register_log_channel(shared)
            .await
            .map_err(|err| InitError::Register(err.to_string()))?;
        let pending_log_uri = match log_uri {
            Some(log_uri) => register_log_uri_or_defer(log_uri, shared).await?,
            None => None,
        };
        let publisher = channel
            .publish_weak()
            .await
            .map_err(|err| InitError::Publisher(err.to_string()))?;

        Ok(LoggingState {
            channel,
            publisher: Mutex::new(publisher),
            last_error: Mutex::new(None),
            pending_log_uri: Mutex::new(pending_log_uri),
        })
    })?;

    tracing_subscriber::registry()
        .with(LogLayer)
        .try_init()
        .map_err(|err| InitError::Subscriber(err.to_string()))?;

    Ok(state)
}

async fn register_log_uri_or_defer(
    log_uri: &str,
    shared: crate::io::SharedChannel,
) -> Result<Option<PendingLogUri>, InitError> {
    match LOG_URI_REGISTRAR.get() {
        Some(registrar) => {
            registrar.register(log_uri, shared).await?;
            Ok(None)
        }
        None => Ok(Some(PendingLogUri {
            uri: log_uri.to_string(),
            shared,
        })),
    }
}

async fn register_log_uri(
    log_uri: &str,
    shared: crate::io::SharedChannel,
) -> Result<(), InitError> {
    let registrar = LOG_URI_REGISTRAR.get().ok_or_else(|| {
        InitError::Register(
            "log URI registrar missing; call set_log_uri_registrar before init".to_string(),
        )
    })?;
    registrar.register(log_uri, shared).await
}

fn take_pending_log_uri(state: &LoggingState) -> Result<Option<PendingLogUri>, InitError> {
    let mut pending = state
        .pending_log_uri
        .lock()
        .map_err(|_| InitError::Poisoned)?;
    Ok(pending.take())
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
    // Fallback to a counter if the host time capability is unavailable.
    match r#async::block_on(time::now()) {
        Ok(now) => now.unix_ms,
        Err(_) => COUNTER.fetch_add(1, Ordering::Relaxed),
    }
}
