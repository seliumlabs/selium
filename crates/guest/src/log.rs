//! Guest log transport module.
//!
//! Provides structured log transport over shared-memory channels with
//! tracing subscriber integration. Log records are encoded as FlatBuffers
//! and published to a Drop-backpressure channel as ready frames.

use std::{
    sync::OnceLock,
    sync::atomic::{AtomicBool, Ordering},
};

use selium_abi::{HostcallRequest, ResourceKind};
use selium_memory::FrameHeader;
use selium_service::FlatMsg;
use selium_shm::{
    RingBuf,
    channels::{Channel, ChannelBackpressure},
};
use thiserror::Error;
use tracing::field::{Field, Visit};
use tracing_subscriber::{
    Layer, layer::Context, prelude::__tracing_subscriber_SubscriberExt as SubscriberExt,
    util::SubscriberInitExt,
};

use crate::hostcall::hostcall_ready;

pub use selium_service::log::{LogField, LogLevel, LogRecord, LogSpan};

/// Default log channel capacity in bytes (512 KB, matching prior art).
const DEFAULT_LOG_CAPACITY: u64 = 512 * 1024;
/// Process-wide re-entrancy guard: suppresses log events triggered while
/// forwarding. Process-wide (an atomic, not `thread_local!`) so it also
/// serialises the single-producer log-ring write across a multithreaded
/// guest's workers — see [`ForwardingGuard::enter`].
static FORWARDING: AtomicBool = AtomicBool::new(false);
static LOGGING_STATE: OnceLock<LoggingState> = OnceLock::new();
/// Once-per-process guard for the panic hook's last-words record.
static PANIC_EMITTED: AtomicBool = AtomicBool::new(false);

/// Global logging state, initialised once via `init()`.
struct LoggingState {
    channel: Channel,
}

/// Guard that sets the forwarding flag on entry and clears it on drop.
struct ForwardingGuard;

/// Tracing subscriber layer that forwards events to the log channel.
pub(crate) struct LogLayer;

/// Visitor that extracts the message and fields from a tracing event.
struct EventVisitor {
    message: String,
    fields: Vec<LogField>,
}

/// Error type for log initialisation.
#[derive(Debug, Error)]
pub enum InitError {
    /// Log channel creation failed.
    #[error("channel creation failed: {0}")]
    Channel(String),
}

impl ForwardingGuard {
    /// Returns `Some(guard)` if not already forwarding, `None` if re-entrant.
    ///
    /// Process-wide, not per-thread: on `wasm32-unknown-unknown` a
    /// `thread_local!` lowers to shared linear memory, so a per-thread flag is
    /// shared across a multithreaded guest's workers anyway — and a plain
    /// `Cell` raced between workers is a data race. One process-wide atomic
    /// makes the guard correct and also serialises the log ring's single-
    /// producer write path across workers (a concurrent forwarder is
    /// suppressed, the same effective behaviour as before).
    fn enter() -> Option<Self> {
        if FORWARDING.swap(true, Ordering::AcqRel) {
            None
        } else {
            Some(ForwardingGuard)
        }
    }
}

impl Drop for ForwardingGuard {
    fn drop(&mut self) {
        FORWARDING.store(false, Ordering::Release);
    }
}

impl<S: tracing::Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        forward_event(event);
    }
}

impl EventVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Vec::new(),
        }
    }
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(LogField {
                key: field.name().to_string(),
                value: format!("{value:?}"),
            });
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(LogField {
                key: field.name().to_string(),
                value: value.to_string(),
            });
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.push(LogField {
            key: field.name().to_string(),
            value: value.to_string(),
        });
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.push(LogField {
            key: field.name().to_string(),
            value: value.to_string(),
        });
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.push(LogField {
            key: field.name().to_string(),
            value: value.to_string(),
        });
    }
}

/// Returns the log channel handle if initialised.
pub fn channel() -> Option<&'static Channel> {
    LOGGING_STATE.get().map(|s| &s.channel)
}

/// Initialises the guest log transport with default capacity.
///
/// Creates a Drop-backpressure channel with `ResourceKind::LogChannel`,
/// installs a tracing subscriber, and registers the channel with the kernel.
///
/// Subsequent calls return `Ok(())` without installing a second subscriber.
pub fn init() -> Result<(), InitError> {
    init_with_capacity(DEFAULT_LOG_CAPACITY)
}

/// Initialises the guest log transport with a custom channel capacity.
pub fn init_with_capacity(capacity: u64) -> Result<(), InitError> {
    if LOGGING_STATE.get().is_some() {
        return Ok(());
    }

    let channel = Channel::create_with_backpressure(
        capacity,
        ChannelBackpressure::Drop,
        ResourceKind::LogChannel,
    )
    .map_err(|e| InitError::Channel(e.to_string()))?;

    // Register the log channel with the kernel so it can attach as a reader.
    // In native mode (no WASM host), this hostcall will fail — that's expected
    // and we continue without kernel registration.
    let shared_id = channel.region_id();
    // Discard the hostcall result: kernel registration is a best-effort
    // optimisation. In native (non-WASM) mode the hostcall always fails,
    // which is expected and harmless.
    drop(hostcall_ready(HostcallRequest::GuestLogRegister {
        shared_id,
    }));

    let state = LoggingState { channel };

    // Atomically install state. If another thread won the race, discard ours.
    drop(LOGGING_STATE.set(state));

    // Emit a best-effort final log record before the guest aborts on panic
    // (panic=abort becomes an `unreachable` trap the host records as
    // `ProcessExited`); see `install_panic_hook`.
    install_panic_hook();

    // Install the subscriber. try_init returns Err if a subscriber is
    // already installed (harmless — the existing one is equivalent).
    let subscriber = tracing_subscriber::registry().with(LogLayer);
    drop(subscriber.try_init());

    Ok(())
}

/// Best-effort last-words record written by the panic hook.
///
/// Writes directly to the ring (bypassing the tracing layer) so it cannot
/// re-enter the forwarding path that may itself be mid-panic, and guards
/// against recursion with a once-per-process flag. A missing or full log
/// channel is silently ignored: logging must never stand in the way of the
/// abort that follows.
fn emit_panic_record(info: &std::panic::PanicHookInfo<'_>) {
    let already_emitted = PANIC_EMITTED.swap(true, Ordering::AcqRel);
    if already_emitted {
        return;
    }

    let Some(state) = LOGGING_STATE.get() else {
        return; // log transport never initialised
    };

    let message = match info.payload().downcast_ref::<&str>() {
        Some(message) => (*message).to_string(),
        None => match info.payload().downcast_ref::<String>() {
            Some(message) => message.clone(),
            None => "guest panicked (no message)".to_string(),
        },
    };

    let record = LogRecord {
        level: LogLevel::Error,
        target: "selium_guest::panic".to_string(),
        message,
        fields: Vec::new(),
        spans: Vec::new(),
        timestamp_ms: timestamp_ms(),
    };
    publish(state.channel.ring(), &FlatMsg::encode(&record));
}

/// Forwards a tracing event to the log channel as a framed FlatBuffer LogRecord.
fn forward_event(event: &tracing::Event<'_>) {
    let _guard = match ForwardingGuard::enter() {
        Some(g) => g,
        None => return, // re-entrant, suppress
    };

    let Some(state) = LOGGING_STATE.get() else {
        return; // not initialised
    };

    let mut visitor = EventVisitor::new();
    event.record(&mut visitor);

    let metadata = event.metadata();
    let level = LogLevel::from(*metadata.level());
    let target = metadata.target().to_string();

    // Collect span stack.
    let spans = Vec::new(); // TODO: walk span stack via event.parent()

    let record = LogRecord {
        level,
        target,
        message: visitor.message,
        fields: visitor.fields,
        spans,
        timestamp_ms: timestamp_ms(),
    };

    let encoded = FlatMsg::encode(&record);

    // Write a ready frame to the channel ring. Best-effort: a full ring
    // never blocks the caller (see `publish`); the host's weak reader means
    // the newest records always survive.
    publish(state.channel.ring(), &encoded);
}

/// Installs a panic hook that emits a best-effort final log record before
/// the guest aborts.
///
/// Guest panics become `unreachable` traps (panic=abort), which the host
/// runtime drains and records as `ProcessExited`. The hook writes one
/// `Error`-level record — including the panic message when available — into
/// the log ring first, so the guest's last words survive in the drained
/// logs. The previous hook is chained so native (non-WASM) panic output is
/// preserved.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &std::panic::PanicHookInfo<'_>| {
        emit_panic_record(info);
        previous(info);
    }));
}

/// Publishes one encoded frame to the log ring, best-effort.
///
/// The log channel is drained by a weak reader (no reader slot), so a full
/// ring never backpressures the guest: the oldest unread records are
/// overwritten while the newest always survive. A reservation failure (a
/// record larger than the ring) is dropped silently — logging must never
/// stall guest execution.
fn publish(ring: &RingBuf, encoded: &[u8]) {
    let required = FrameHeader::ENCODED_SIZE as u64 + encoded.len() as u64;
    if let Ok(pos) = ring.reserve(required) {
        drop(ring.write_frame(pos, encoded, 0, 0));
    }
}

/// Returns the current wall-clock time in milliseconds, using the host
/// clock (`std::time::SystemTime::now()` panics on
/// `wasm32-unknown-unknown`). Falls back to 0 when the host clock is
/// unavailable (native test contexts).
fn timestamp_ms() -> u64 {
    crate::time::now()
        .map(|nanos| nanos / 1_000_000)
        .unwrap_or(0)
}
