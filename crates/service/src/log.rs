//! Structured log record types and FlatBuffers encoding.
//!
//! This module is intentionally transport-agnostic: it defines the shape of
//! a log record and how to encode/decode it, but it does not know about
//! shared-memory channels, tracing subscribers, or guest hostcalls.

use flatbuffers::{FlatBufferBuilder, InvalidFlatbuffer, WIPOffset};

use crate::{
    FlatMsg, HasSchema, SchemaDescriptor,
    fbs::selium::logging::{
        Field, FieldArgs, LogLevel as FbsLogLevel, LogRecord as FbsLogRecord, LogRecordArgs, Span,
        SpanArgs,
    },
};

/// Log severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogLevel {
    /// Trace level.
    Trace,
    /// Debug level.
    Debug,
    /// Info level.
    Info,
    /// Warn level.
    Warn,
    /// Error level.
    Error,
}

/// A single key-value field attached to a log record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogField {
    /// Field key.
    pub key: String,
    /// Field value (stringified).
    pub value: String,
}

/// A span in the tracing span stack at the time the event was emitted.
#[derive(Debug, Clone, PartialEq)]
pub struct LogSpan {
    /// Span name.
    pub name: String,
    /// Fields attached to the span.
    pub fields: Vec<LogField>,
}

/// A structured log record published to the guest log channel.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    /// Severity level.
    pub level: LogLevel,
    /// Tracing target (typically the module path).
    pub target: String,
    /// Log message.
    pub message: String,
    /// Key-value fields attached to the event.
    pub fields: Vec<LogField>,
    /// Span stack at the time of the event.
    pub spans: Vec<LogSpan>,
    /// Timestamp in milliseconds since UNIX epoch.
    pub timestamp_ms: u64,
}

impl From<tracing::Level> for LogLevel {
    fn from(level: tracing::Level) -> Self {
        match level {
            tracing::Level::TRACE => LogLevel::Trace,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        }
    }
}

impl FlatMsg for LogRecord {
    fn encode(value: &Self) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();

        let target = fbb.create_string(&value.target);
        let message = fbb.create_string(&value.message);

        let fields_vec: Vec<WIPOffset<Field>> = value
            .fields
            .iter()
            .map(|f| encode_field(&mut fbb, f))
            .collect();
        let fields = fbb.create_vector(&fields_vec);

        let spans_vec: Vec<WIPOffset<Span>> = value
            .spans
            .iter()
            .map(|s| encode_span(&mut fbb, s))
            .collect();
        let spans = fbb.create_vector(&spans_vec);

        let record = FbsLogRecord::create(
            &mut fbb,
            &LogRecordArgs {
                level: to_fbs_level(value.level),
                target: Some(target),
                message: Some(message),
                fields: Some(fields),
                spans: Some(spans),
                timestamp_ms: value.timestamp_ms,
            },
        );

        fbb.finish(record, None);
        fbb.finished_data().to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        let record = flatbuffers::root::<FbsLogRecord>(bytes)?;

        let level = from_fbs_level(record.level());
        let target = record.target().unwrap_or("").to_string();
        let message = record.message().unwrap_or("").to_string();
        let timestamp_ms = record.timestamp_ms();

        let fields = record
            .fields()
            .map(|v| {
                (0..v.len())
                    .map(|i| {
                        let f = v.get(i);
                        LogField {
                            key: f.key().unwrap_or("").to_string(),
                            value: f.value().unwrap_or("").to_string(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let spans = record
            .spans()
            .map(|v| {
                (0..v.len())
                    .map(|i| {
                        let s = v.get(i);
                        let span_fields = s
                            .fields()
                            .map(|fv| {
                                (0..fv.len())
                                    .map(|j| {
                                        let f = fv.get(j);
                                        LogField {
                                            key: f.key().unwrap_or("").to_string(),
                                            value: f.value().unwrap_or("").to_string(),
                                        }
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        LogSpan {
                            name: s.name().unwrap_or("").to_string(),
                            fields: span_fields,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            level,
            target,
            message,
            fields,
            spans,
            timestamp_ms,
        })
    }
}

impl HasSchema for LogRecord {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "selium.logging.LogRecord",
        hash: [
            0x4C, 0x4F, 0x47, 0x52, 0x45, 0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
    };
}

impl From<LogLevel> for tracing::Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => tracing::Level::TRACE,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Error => tracing::Level::ERROR,
        }
    }
}

fn encode_field<'a>(fbb: &mut FlatBufferBuilder<'a>, field: &LogField) -> WIPOffset<Field<'a>> {
    let key = fbb.create_string(&field.key);
    let value = fbb.create_string(&field.value);
    Field::create(
        fbb,
        &FieldArgs {
            key: Some(key),
            value: Some(value),
        },
    )
}

fn encode_span<'a>(fbb: &mut FlatBufferBuilder<'a>, span: &LogSpan) -> WIPOffset<Span<'a>> {
    let name = fbb.create_string(&span.name);
    let fields_vec: Vec<WIPOffset<Field>> =
        span.fields.iter().map(|f| encode_field(fbb, f)).collect();
    let fields = fbb.create_vector(&fields_vec);
    Span::create(
        fbb,
        &SpanArgs {
            name: Some(name),
            fields: Some(fields),
        },
    )
}

fn from_fbs_level(level: FbsLogLevel) -> LogLevel {
    match level {
        FbsLogLevel::Trace => LogLevel::Trace,
        FbsLogLevel::Debug => LogLevel::Debug,
        FbsLogLevel::Info => LogLevel::Info,
        FbsLogLevel::Warn => LogLevel::Warn,
        FbsLogLevel::Error => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

fn to_fbs_level(level: LogLevel) -> FbsLogLevel {
    match level {
        LogLevel::Trace => FbsLogLevel::Trace,
        LogLevel::Debug => FbsLogLevel::Debug,
        LogLevel::Info => FbsLogLevel::Info,
        LogLevel::Warn => FbsLogLevel::Warn,
        LogLevel::Error => FbsLogLevel::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_from_tracing_level() {
        assert_eq!(LogLevel::from(tracing::Level::TRACE), LogLevel::Trace);
        assert_eq!(LogLevel::from(tracing::Level::DEBUG), LogLevel::Debug);
        assert_eq!(LogLevel::from(tracing::Level::INFO), LogLevel::Info);
        assert_eq!(LogLevel::from(tracing::Level::WARN), LogLevel::Warn);
        assert_eq!(LogLevel::from(tracing::Level::ERROR), LogLevel::Error);
    }

    #[test]
    fn tracing_level_from_log_level() {
        assert_eq!(tracing::Level::from(LogLevel::Trace), tracing::Level::TRACE);
        assert_eq!(tracing::Level::from(LogLevel::Debug), tracing::Level::DEBUG);
        assert_eq!(tracing::Level::from(LogLevel::Info), tracing::Level::INFO);
        assert_eq!(tracing::Level::from(LogLevel::Warn), tracing::Level::WARN);
        assert_eq!(tracing::Level::from(LogLevel::Error), tracing::Level::ERROR);
    }

    #[test]
    fn log_record_encode_decode_round_trip() {
        let record = LogRecord {
            level: LogLevel::Info,
            target: "my_module".to_string(),
            message: "hello world".to_string(),
            fields: vec![
                LogField {
                    key: "user_id".to_string(),
                    value: "42".to_string(),
                },
                LogField {
                    key: "action".to_string(),
                    value: "login".to_string(),
                },
            ],
            spans: vec![LogSpan {
                name: "request".to_string(),
                fields: vec![LogField {
                    key: "method".to_string(),
                    value: "GET".to_string(),
                }],
            }],
            timestamp_ms: 1700000000000,
        };

        let encoded = FlatMsg::encode(&record);
        let decoded: LogRecord = FlatMsg::decode(&encoded).expect("decode");

        assert_eq!(decoded.level, record.level);
        assert_eq!(decoded.target, record.target);
        assert_eq!(decoded.message, record.message);
        assert_eq!(decoded.fields.len(), record.fields.len());
        assert_eq!(decoded.fields[0].key, "user_id");
        assert_eq!(decoded.fields[0].value, "42");
        assert_eq!(decoded.spans.len(), 1);
        assert_eq!(decoded.spans[0].name, "request");
        assert_eq!(decoded.spans[0].fields[0].key, "method");
        assert_eq!(decoded.timestamp_ms, record.timestamp_ms);
    }

    #[test]
    fn log_record_minimal_round_trip() {
        let record = LogRecord {
            level: LogLevel::Error,
            target: "test".to_string(),
            message: "oops".to_string(),
            fields: vec![],
            spans: vec![],
            timestamp_ms: 0,
        };

        let encoded = FlatMsg::encode(&record);
        let decoded: LogRecord = FlatMsg::decode(&encoded).expect("decode");

        assert_eq!(decoded.level, LogLevel::Error);
        assert_eq!(decoded.message, "oops");
        assert!(decoded.fields.is_empty());
        assert!(decoded.spans.is_empty());
    }
}
