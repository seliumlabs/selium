use std::env;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use futures::{StreamExt, pin_mut};
use selium_abi::{AbiParam, AbiScalarType, AbiScalarValue, Capability};
use selium_client::{ClientConfigBuilder, Process, ProcessBuilder};
use selium_userland::fbs::selium::logging::{self as log_fb, LogLevel};
use tracing::field::Empty;
use tracing::{Span, debug, error, instrument};
use tracing_subscriber::{EnvFilter, fmt::time::SystemTime};

#[derive(Parser, Debug)]
#[command(version, about = None)]
struct Cli {
    /// Domain name (SNI) that the control-plane endpoint routes for.
    #[arg(long, env = "SELIUM_DOMAIN", default_value = "localhost")]
    domain: String,
    /// Port that the control-plane endpoint listens on.
    #[arg(long, env = "SELIUM_PORT", default_value_t = 7000)]
    port: u16,
    /// Directory containing the client key/cert pair and CA certificate.
    #[arg(long, env = "SELIUM_CERT_DIR", default_value_os = "certs")]
    cert_dir: PathBuf,
    /// Log output format (text or JSON) for tracing events.
    #[arg(long, env = "SELIUM_LOG_FORMAT", default_value = "text")]
    log_format: LogFormat,
    #[command(subcommand)]
    command: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
enum LogFormat {
    /// Human-friendly text logs suitable for local development.
    Text,
    /// JSON logs for ingestion into systems such as Loki or OTLP collectors.
    Json,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start a new process
    Start(StartArgs),

    /// Terminate a running process
    Stop {
        /// Id of the process
        id: u64,
    },

    /// Show all running processes
    Ps,
}

#[derive(Args, Debug)]
struct StartArgs {
    /// Path to the WASM executable
    path: String,
    /// Entrypoint function to call within the module
    #[arg(default_value = "start")]
    entrypoint: String,
    /// Additional capabilities to grant the process
    #[arg(short, long, value_enum)]
    capability: Vec<CapabilityArg>,
    /// Ordered parameter types describing the entrypoint signature. Optional when `--arg` supplies typed values.
    #[arg(short, long, value_enum)]
    param: Vec<ParamKind>,
    /// Ordered result types describing the entrypoint signature
    #[arg(short, long, value_enum)]
    result: Vec<ParamKind>,
    /// Arguments matching the declared params, supplied in order. Accepts `TYPE:VALUE` to infer the param list.
    /// For `buffer` parameters, prefix the value with `hex:` to pass arbitrary bytes (useful for rkyv payloads).
    #[arg(short, long, value_parser = parse_argument, value_name = "VALUE|TYPE:VALUE")]
    arg: Vec<Argument>,
    /// Attach to the process logging stream after launch.
    #[arg(long)]
    attach: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum CapabilityArg {
    SessionLifecycle,
    ChannelLifecycle,
    ChannelReader,
    ChannelWriter,
    ProcessLifecycle,
    NetBind,
    NetAccept,
    NetConnect,
    NetRead,
    NetWrite,
}

impl From<CapabilityArg> for Capability {
    fn from(value: CapabilityArg) -> Self {
        match value {
            CapabilityArg::SessionLifecycle => Capability::SessionLifecycle,
            CapabilityArg::ChannelLifecycle => Capability::ChannelLifecycle,
            CapabilityArg::ChannelReader => Capability::ChannelReader,
            CapabilityArg::ChannelWriter => Capability::ChannelWriter,
            CapabilityArg::ProcessLifecycle => Capability::ProcessLifecycle,
            CapabilityArg::NetBind => Capability::NetBind,
            CapabilityArg::NetAccept => Capability::NetAccept,
            CapabilityArg::NetConnect => Capability::NetConnect,
            CapabilityArg::NetRead => Capability::NetRead,
            CapabilityArg::NetWrite => Capability::NetWrite,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum ParamKind {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Buffer,
    Utf8,
    Resource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Argument {
    Typed { kind: ParamKind, value: String },
    Untyped(String),
}

impl ParamKind {
    fn from_label(label: &str) -> Option<Self> {
        match label.to_ascii_lowercase().as_str() {
            "i8" => Some(Self::I8),
            "u8" => Some(Self::U8),
            "i16" => Some(Self::I16),
            "u16" => Some(Self::U16),
            "i32" => Some(Self::I32),
            "u32" => Some(Self::U32),
            "i64" => Some(Self::I64),
            "u64" => Some(Self::U64),
            "f32" => Some(Self::F32),
            "f64" => Some(Self::F64),
            "buffer" | "bytes" | "byte" | "data" => Some(Self::Buffer),
            "utf8" | "utf-8" | "string" | "str" | "text" => Some(Self::Utf8),
            "resource" | "handle" => Some(Self::Resource),
            _ => None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        domain,
        port,
        cert_dir,
        log_format,
        command,
    } = Cli::parse();

    // Initialise logging
    initialise_tracing(log_format)?;

    match command {
        Command::Start(args) => start(&domain, port, cert_dir.as_path(), args).await?,
        Command::Stop { id } => stop(&domain, port, cert_dir.as_path(), id).await?,
        Command::Ps => bail!("ps command is not implemented yet"),
    }

    Ok(())
}

fn initialise_tracing(format: LogFormat) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(env::var("RUST_LOG").unwrap_or_else(|_| "info".into())))?;

    match format {
        LogFormat::Text => {
            tracing_subscriber::fmt()
                .with_env_filter(filter.clone())
                .with_target(false)
                .with_timer(SystemTime)
                .init();
        }
        LogFormat::Json => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(filter)
                .with_target(false)
                .with_current_span(true)
                .with_span_list(true)
                .init();
        }
    }

    Ok(())
}

#[instrument(skip(cert_dir, args))]
async fn start(domain: &str, port: u16, cert_dir: &Path, args: StartArgs) -> Result<()> {
    debug!("connecting to remote client");

    let StartArgs {
        path,
        entrypoint,
        capability,
        param,
        result,
        arg,
        attach,
    } = args;
    let client = ClientConfigBuilder::default()
        .domain(domain)
        .port(port)
        .certificate_directory(cert_dir)
        .connect()
        .await
        .context("connect to remote client")?;

    debug!("connected to remote client");

    let (params, arg_values) = resolve_arguments(param, arg)?;
    let signature = build_signature(&params, &result);

    let builder = capability.iter().fold(
        ProcessBuilder::new(&path, &entrypoint).signature(signature),
        |builder, capability| builder.capability((*capability).into()),
    );
    let builder = params
        .iter()
        .zip(arg_values.iter())
        .try_fold(builder, |builder, (kind, raw)| {
            apply_argument(builder, kind, raw)
        })?;

    let process = Process::start(&client, builder)
        .await
        .context("start process")?;

    println!("Started process (id={})", process.handle());

    if attach {
        attach_logs(&process).await?;
    }

    Ok(())
}

async fn stop(domain: &str, port: u16, cert_dir: &Path, id: u64) -> Result<()> {
    let client = ClientConfigBuilder::default()
        .domain(domain)
        .port(port)
        .certificate_directory(cert_dir)
        .connect()
        .await
        .context("connect to remote client")?;
    // Safe because the process id is supplied by the operator and will be validated by the host.
    Process::new(&client, id)
        .stop()
        .await
        .context("stop process")?;

    println!("Stopped process {id}");
    Ok(())
}

fn build_signature(params: &[ParamKind], results: &[ParamKind]) -> selium_abi::AbiSignature {
    let params = params.iter().map(map_param).collect();
    let results = results.iter().map(map_param).collect();
    selium_abi::AbiSignature::new(params, results)
}

fn map_param(kind: &ParamKind) -> AbiParam {
    match kind {
        ParamKind::I8 => AbiParam::Scalar(AbiScalarType::I8),
        ParamKind::U8 => AbiParam::Scalar(AbiScalarType::U8),
        ParamKind::I16 => AbiParam::Scalar(AbiScalarType::I16),
        ParamKind::U16 => AbiParam::Scalar(AbiScalarType::U16),
        ParamKind::I32 => AbiParam::Scalar(AbiScalarType::I32),
        ParamKind::U32 => AbiParam::Scalar(AbiScalarType::U32),
        ParamKind::I64 => AbiParam::Scalar(AbiScalarType::I64),
        ParamKind::U64 | ParamKind::Resource => AbiParam::Scalar(AbiScalarType::U64),
        ParamKind::F32 => AbiParam::Scalar(AbiScalarType::F32),
        ParamKind::F64 => AbiParam::Scalar(AbiScalarType::F64),
        ParamKind::Buffer | ParamKind::Utf8 => AbiParam::Buffer,
    }
}

fn apply_argument(builder: ProcessBuilder, kind: &ParamKind, raw: &str) -> Result<ProcessBuilder> {
    match kind {
        ParamKind::I8 => {
            let value = raw.parse::<i8>().context("parse i8 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::I8(value)))
        }
        ParamKind::U8 => {
            let value = raw.parse::<u8>().context("parse u8 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::U8(value)))
        }
        ParamKind::I16 => {
            let value = raw.parse::<i16>().context("parse i16 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::I16(value)))
        }
        ParamKind::U16 => {
            let value = raw.parse::<u16>().context("parse u16 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::U16(value)))
        }
        ParamKind::I32 => {
            let value = raw.parse::<i32>().context("parse i32 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::I32(value)))
        }
        ParamKind::U32 => {
            let value = raw.parse::<u32>().context("parse u32 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::U32(value)))
        }
        ParamKind::I64 => {
            let value = raw.parse::<i64>().context("parse i64 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::I64(value)))
        }
        ParamKind::U64 => {
            let value = raw.parse::<u64>().context("parse u64 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::U64(value)))
        }
        ParamKind::F32 => {
            let value = raw.parse::<f32>().context("parse f32 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::F32(value)))
        }
        ParamKind::F64 => {
            let value = raw.parse::<f64>().context("parse f64 argument")?;
            Ok(builder.arg_scalar(AbiScalarValue::F64(value)))
        }
        ParamKind::Buffer => {
            let bytes = parse_buffer_bytes(raw).context("parse buffer argument")?;
            Ok(builder.arg_buffer(bytes))
        }
        ParamKind::Utf8 => Ok(builder.arg_utf8(raw)),
        ParamKind::Resource => {
            let value = raw.parse::<u64>().context("parse resource handle")?;
            Ok(builder.arg_resource(value))
        }
    }
}

fn parse_buffer_bytes(raw: &str) -> Result<Vec<u8>> {
    let Some(hex) = raw.strip_prefix("hex:") else {
        return Ok(raw.as_bytes().to_vec());
    };

    decode_hex(hex)
}

fn decode_hex(mut raw: &str) -> Result<Vec<u8>> {
    if let Some(trimmed) = raw.strip_prefix("0x") {
        raw = trimmed;
    }

    let mut cleaned = Vec::with_capacity(raw.len());
    for ch in raw.bytes() {
        if ch.is_ascii_whitespace() || ch == b'_' {
            continue;
        }
        cleaned.push(ch);
    }

    if cleaned.len() % 2 != 0 {
        bail!("hex payload must have an even number of digits");
    }

    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for pair in cleaned.chunks_exact(2) {
        let hi = hex_digit(pair[0])?;
        let lo = hex_digit(pair[1])?;
        out.push((hi << 4) | lo);
    }

    Ok(out)
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex digit {byte:?}"),
    }
}

fn resolve_arguments(
    params: Vec<ParamKind>,
    args: Vec<Argument>,
) -> Result<(Vec<ParamKind>, Vec<String>)> {
    if params.is_empty() {
        if args.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut inferred_params = Vec::with_capacity(args.len());
        let mut values = Vec::with_capacity(args.len());
        for (index, arg) in args.into_iter().enumerate() {
            match arg {
                Argument::Typed { kind, value } => {
                    inferred_params.push(kind);
                    values.push(value);
                }
                Argument::Untyped(value) => {
                    bail!(
                        "argument {} is missing a type. Supply --param or prefix the value with TYPE:, e.g. string:{value}",
                        index + 1
                    );
                }
            }
        }
        return Ok((inferred_params, values));
    }

    if args.len() != params.len() {
        bail!(
            "argument count mismatch: expected {} arguments, got {}",
            params.len(),
            args.len()
        );
    }

    let mut values = Vec::with_capacity(args.len());
    for (index, (expected, arg)) in params.iter().zip(args.into_iter()).enumerate() {
        match arg {
            Argument::Typed { kind, value } => {
                if *expected != kind {
                    bail!(
                        "argument {} declared as {:?} but --param expects {:?}",
                        index + 1,
                        kind,
                        expected
                    );
                }
                values.push(value);
            }
            Argument::Untyped(value) => values.push(value),
        }
    }

    Ok((params, values))
}

fn parse_argument(raw: &str) -> Result<Argument, String> {
    if let Some((label, value)) = raw.split_once(':')
        && let Some(kind) = ParamKind::from_label(label)
    {
        return Ok(Argument::Typed {
            kind,
            value: value.to_owned(),
        });
    }

    Ok(Argument::Untyped(raw.to_owned()))
}

#[instrument(skip_all, fields(process = process.handle(), log_handle = Empty))]
async fn attach_logs(process: &Process) -> Result<()> {
    let channel = {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match process.log_channel().await {
                Ok(channel) => break channel,
                Err(selium_client::ClientError::Remote(message))
                    if message.to_ascii_lowercase().contains("not found")
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(err) => return Err(err).context("fetch process log channel"),
            }
        }
    };

    Span::current().record("log_handle", channel.handle());
    debug!("listening for logs");

    let chunk_size = 64 * 1024;
    let subscriber = channel
        .subscribe_shared(chunk_size)
        .await
        .context("subscribe to process log stream")?;
    pin_mut!(subscriber);
    while let Some(frame) = subscriber.next().await {
        let bytes = frame.context("read log frame")?;
        match log_fb::root_as_log_record(&bytes) {
            Ok(record) => render_log(record),
            Err(err) => error!(?err, "invalid log frame"),
        }
    }

    Ok(())
}

fn render_log(record: log_fb::LogRecord<'_>) {
    let level = match record.level() {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
        _ => "UNKNOWN",
    };
    let target = record.target().unwrap_or_default();
    let message = record.message().unwrap_or_default();
    let spans: Vec<String> = record
        .spans()
        .map(|spans| {
            spans
                .iter()
                .filter_map(|span| span.name().map(String::from))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    let fields: Vec<String> = record
        .fields()
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| Some((field.key()?, field.value()?)))
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    let mut line = format!("[{level}] {target}");
    if !spans.is_empty() {
        line.push(' ');
        line.push_str(&spans.join("::"));
    }
    if !message.is_empty() {
        line.push_str(": ");
        line.push_str(message);
    }
    if !fields.is_empty() {
        line.push(' ');
        line.push_str(&fields.join(" "));
    }

    println!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::{AbiParam, AbiScalarType};

    #[test]
    fn parse_argument_recognises_string_prefix() {
        let argument = parse_argument("string:hello").expect("parse string argument");
        assert!(matches!(
            argument,
            Argument::Typed {
                kind: ParamKind::Utf8,
                value
            } if value == "hello"
        ));
    }

    #[test]
    fn resolve_arguments_infers_types() {
        let args = vec![
            parse_argument("i32:5").expect("parse i32"),
            parse_argument("utf8:abc").expect("parse utf8"),
        ];
        let (params, values) = resolve_arguments(Vec::new(), args).expect("resolve arguments");

        assert_eq!(params, vec![ParamKind::I32, ParamKind::Utf8]);
        assert_eq!(values, vec![String::from("5"), String::from("abc")]);
    }

    #[test]
    fn resolve_arguments_detects_type_mismatch() {
        let args = vec![Argument::Typed {
            kind: ParamKind::Utf8,
            value: String::from("hello"),
        }];
        let error = resolve_arguments(vec![ParamKind::I32], args).unwrap_err();

        assert!(
            error.to_string().contains("declared as") && error.to_string().contains("expects"),
            "error was: {error}"
        );
    }

    #[test]
    fn parse_buffer_bytes_decodes_hex() {
        let bytes = parse_buffer_bytes("hex:deadbeef").expect("decode hex");
        assert_eq!(bytes, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn resource_params_use_u64_signature() {
        let signature = build_signature(&[ParamKind::Resource], &[]);
        assert_eq!(signature.params(), &[AbiParam::Scalar(AbiScalarType::U64)]);
    }
}
