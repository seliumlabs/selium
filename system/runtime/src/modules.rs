use std::{
    io::{self, ErrorKind, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use selium_abi::{
    AbiParam, AbiScalarType, AbiScalarValue, AbiSignature, Capability, EntrypointArg,
    EntrypointInvocation, GuestResourceId,
};
use selium_kernel::{
    Kernel, KernelError,
    drivers::process::ProcessLifecycleCapability,
    registry::{Registry, ResourceHandle, ResourceId, ResourceType},
};
use selium_messaging::Channel;
use selium_userland::fbs::selium::logging::{self as log_fb, LogLevel};
use selium_wasmtime::{Error as WasmtimeError, WasmtimeDriver};
use tokio::time::sleep;
use tracing::{Level, Span, info, instrument, warn};

type ProcessHandle = <WasmtimeDriver as ProcessLifecycleCapability>::Process;

const LOG_FRAME_CAPACITY: usize = 512 * 1024;
const LOG_CHANNEL_WAIT: Duration = Duration::from_secs(5);
const LOG_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_ENTRYPOINT: &str = "start";
const GUEST_LOG_TARGET: &str = "selium.guest";

#[derive(Default)]
struct ModuleArgs {
    params: Vec<AbiParam>,
    args: Vec<EntrypointArg>,
}

struct ModuleSpec {
    module_label: String,
    module_path: PathBuf,
    entrypoint: String,
    capabilities: Vec<Capability>,
    params: Vec<AbiParam>,
    args: Vec<EntrypointArg>,
}

#[derive(Default)]
struct ModuleSpecBuilder {
    path: Option<String>,
    entrypoint: Option<String>,
    capabilities: Option<Vec<Capability>>,
    params: Option<Vec<ParamKind>>,
    args: Option<Vec<Argument>>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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

impl ModuleSpecBuilder {
    fn is_empty(&self) -> bool {
        self.path.is_none()
            && self.entrypoint.is_none()
            && self.capabilities.is_none()
            && self.params.is_none()
            && self.args.is_none()
    }
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

macro_rules! emit_guest_log_event {
    ($level:expr, $target:expr, $message:expr, $span_path:expr, $field_list:expr) => {
        match ($span_path, $field_list) {
            (Some(span_path), Some(field_list)) => {
                tracing::event!(
                    target: GUEST_LOG_TARGET,
                    $level,
                    guest_target = %$target,
                    guest_spans = %span_path,
                    guest_fields = %field_list,
                    message = %$message
                );
            }
            (Some(span_path), None) => {
                tracing::event!(
                    target: GUEST_LOG_TARGET,
                    $level,
                    guest_target = %$target,
                    guest_spans = %span_path,
                    message = %$message
                );
            }
            (None, Some(field_list)) => {
                tracing::event!(
                    target: GUEST_LOG_TARGET,
                    $level,
                    guest_target = %$target,
                    guest_fields = %field_list,
                    message = %$message
                );
            }
            (None, None) => {
                tracing::event!(
                    target: GUEST_LOG_TARGET,
                    $level,
                    guest_target = %$target,
                    message = %$message
                );
            }
        }
    };
}

/// Read module specifications from stdin and start each module with log forwarding.
///
/// Input format: each module is a block of `key=value` lines separated by blank lines.
/// Required keys are `path`, `capabilities`, and `args`. Optional keys are `entrypoint`
/// (defaults to `start`) and `params`. The `args` value is a comma-separated list of values
/// that may be prefixed with `TYPE:` to infer parameter kinds. When `params` is omitted,
/// every arg must be typed. The `path` must be relative to `work_dir`.
///
/// Supported argument types: `i8`, `u8`, `i16`, `u16`, `i32`, `u32`, `i64`, `u64`, `f32`,
/// `f64`, `buffer`, `utf8`, `resource`. Buffer values support a `hex:` prefix to pass raw
/// bytes.
pub async fn spawn_from_stdin(
    kernel: &Kernel,
    registry: &Arc<Registry>,
    work_dir: impl AsRef<Path>,
) -> Result<Vec<ResourceId>> {
    let specs = read_module_specs(io::stdin(), work_dir.as_ref())?;
    let runtime = kernel.get::<WasmtimeDriver>().ok_or_else(|| {
        WasmtimeError::Kernel(KernelError::Driver(
            "missing Wasmtime driver in kernel".to_string(),
        ))
    })?;

    let mut processes = Vec::with_capacity(specs.len());
    for spec in specs {
        let process_id = spawn_module(runtime, registry, spec).await?;
        processes.push(process_id);
    }

    Ok(processes)
}

fn read_module_specs(mut reader: impl Read, work_dir: &Path) -> Result<Vec<ModuleSpec>> {
    let mut input = String::new();
    reader
        .read_to_string(&mut input)
        .context("read module specifications from stdin")?;
    parse_module_specs(&input, work_dir)
}

fn parse_module_specs(input: &str, work_dir: &Path) -> Result<Vec<ModuleSpec>> {
    let mut specs = Vec::new();
    let mut builder = ModuleSpecBuilder::default();

    for (index, raw_line) in input.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();

        if line.is_empty() {
            if !builder.is_empty() {
                let spec = build_module_spec(builder, work_dir).with_context(|| {
                    format!("parse module specification ending at line {line_no}")
                })?;
                specs.push(spec);
                builder = ModuleSpecBuilder::default();
            }
            continue;
        }

        if line.starts_with('#') {
            continue;
        }

        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| anyhow!("line {line_no}: expected key=value"))?;
        let key = key.trim();
        let value = value.trim();

        match key {
            "path" => {
                if builder.path.is_some() {
                    return Err(anyhow!("line {line_no}: duplicate path"));
                }
                builder.path = Some(value.to_string());
            }
            "entrypoint" => {
                if builder.entrypoint.is_some() {
                    return Err(anyhow!("line {line_no}: duplicate entrypoint"));
                }
                builder.entrypoint = Some(value.to_string());
            }
            "capabilities" => {
                if builder.capabilities.is_some() {
                    return Err(anyhow!("line {line_no}: duplicate capabilities"));
                }
                builder.capabilities = Some(parse_capabilities(value)?);
            }
            "params" | "param" => {
                if builder.params.is_some() {
                    return Err(anyhow!("line {line_no}: duplicate params"));
                }
                builder.params = Some(parse_params(value)?);
            }
            "args" => {
                if builder.args.is_some() {
                    return Err(anyhow!("line {line_no}: duplicate args"));
                }
                builder.args = Some(parse_args(value)?);
            }
            _ => return Err(anyhow!("line {line_no}: unknown key `{key}`")),
        }
    }

    if !builder.is_empty() {
        let line_no = input.lines().count();
        let spec = build_module_spec(builder, work_dir)
            .with_context(|| format!("parse module specification ending at line {line_no}"))?;
        specs.push(spec);
    }

    if specs.is_empty() {
        return Err(anyhow!("no module specifications provided on stdin"));
    }

    Ok(specs)
}

fn build_module_spec(builder: ModuleSpecBuilder, work_dir: &Path) -> Result<ModuleSpec> {
    let path = builder
        .path
        .ok_or_else(|| anyhow!("module specification missing path"))?;
    let entrypoint = builder
        .entrypoint
        .unwrap_or_else(|| DEFAULT_ENTRYPOINT.to_string());
    let capabilities = builder
        .capabilities
        .ok_or_else(|| anyhow!("module specification missing capabilities"))?;
    let args = builder
        .args
        .ok_or_else(|| anyhow!("module specification missing args"))?;
    let params = builder.params.unwrap_or_default();
    let (params, values) = resolve_arguments(params, args)?;
    let ModuleArgs { params, args } = build_module_args(params, values)?;

    if path.trim().is_empty() {
        return Err(anyhow!("module path must not be empty"));
    }
    if entrypoint.trim().is_empty() {
        return Err(anyhow!("entrypoint must not be empty"));
    }
    if capabilities.is_empty() {
        return Err(anyhow!("capabilities list must not be empty"));
    }

    let module_path = work_dir.join(parse_relative_path(&path)?);

    Ok(ModuleSpec {
        module_label: path,
        module_path,
        entrypoint,
        capabilities,
        params,
        args,
    })
}

fn parse_relative_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(anyhow!("module path must be relative"));
    }

    if path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return Err(anyhow!("module path must not contain parent segments"));
    }

    Ok(path.to_path_buf())
}

fn parse_capabilities(raw: &str) -> Result<Vec<Capability>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("capabilities list must not be empty"));
    }

    let mut caps = Vec::new();
    for item in trimmed.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(anyhow!("capability entry must not be empty"));
        }
        let capability = match item.to_ascii_lowercase().as_str() {
            "sessionlifecycle" | "session_lifecycle" | "session-lifecycle" => {
                Capability::SessionLifecycle
            }
            "channellifecycle" | "channel_lifecycle" | "channel-lifecycle" => {
                Capability::ChannelLifecycle
            }
            "channelreader" | "channel_reader" | "channel-reader" => Capability::ChannelReader,
            "channelwriter" | "channel_writer" | "channel-writer" => Capability::ChannelWriter,
            "processlifecycle" | "process_lifecycle" | "process-lifecycle" => {
                Capability::ProcessLifecycle
            }
            "netbind" | "net_bind" | "net-bind" => Capability::NetBind,
            "netaccept" | "net_accept" | "net-accept" => Capability::NetAccept,
            "netconnect" | "net_connect" | "net-connect" => Capability::NetConnect,
            "netread" | "net_read" | "net-read" => Capability::NetRead,
            "netwrite" | "net_write" | "net-write" => Capability::NetWrite,
            _ => return Err(anyhow!("unknown capability `{item}`")),
        };

        if !caps.contains(&capability) {
            caps.push(capability);
        }
    }

    Ok(caps)
}

fn parse_params(raw: &str) -> Result<Vec<ParamKind>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("params list must not be empty"));
    }

    let mut params = Vec::new();
    for (index, item) in trimmed.split(',').enumerate() {
        let item = item.trim();
        if item.is_empty() {
            return Err(anyhow!("param {} must not be empty", index + 1));
        }
        let kind = ParamKind::from_label(item)
            .ok_or_else(|| anyhow!("unknown param kind `{item}`"))?;
        params.push(kind);
    }

    Ok(params)
}

fn parse_args(raw: &str) -> Result<Vec<Argument>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = Vec::new();
    for (index, item) in trimmed.split(',').enumerate() {
        let item = item.trim();
        let arg_index = index + 1;
        if item.is_empty() {
            return Err(anyhow!("arg {arg_index} must not be empty"));
        }
        args.push(parse_argument(item));
    }

    Ok(args)
}

fn parse_argument(raw: &str) -> Argument {
    if let Some((label, value)) = raw.split_once(':')
        && let Some(kind) = ParamKind::from_label(label)
    {
        return Argument::Typed {
            kind,
            value: value.to_owned(),
        };
    }

    Argument::Untyped(raw.to_owned())
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
                        "argument {} is missing a type. Supply params or prefix the value with TYPE:, e.g. string:{value}",
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
                        "argument {} declared as {:?} but params expects {:?}",
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

fn build_module_args(params: Vec<ParamKind>, values: Vec<String>) -> Result<ModuleArgs> {
    let mut abi_params = Vec::with_capacity(params.len());
    let mut args = Vec::with_capacity(params.len());

    for (index, (kind, value)) in params.iter().zip(values.iter()).enumerate() {
        abi_params.push(map_param(kind));
        let arg = parse_entrypoint_arg(kind, value).with_context(|| {
            format!("parse argument {}", index + 1)
        })?;
        args.push(arg);
    }

    Ok(ModuleArgs {
        params: abi_params,
        args,
    })
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

fn parse_entrypoint_arg(kind: &ParamKind, raw: &str) -> Result<EntrypointArg> {
    match kind {
        ParamKind::I8 => {
            let value = raw.parse::<i8>().context("parse i8 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::I8(value)))
        }
        ParamKind::U8 => {
            let value = raw.parse::<u8>().context("parse u8 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::U8(value)))
        }
        ParamKind::I16 => {
            let value = raw.parse::<i16>().context("parse i16 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::I16(value)))
        }
        ParamKind::U16 => {
            let value = raw.parse::<u16>().context("parse u16 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::U16(value)))
        }
        ParamKind::I32 => {
            let value = raw.parse::<i32>().context("parse i32 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::I32(value)))
        }
        ParamKind::U32 => {
            let value = raw.parse::<u32>().context("parse u32 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::U32(value)))
        }
        ParamKind::I64 => {
            let value = raw.parse::<i64>().context("parse i64 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::I64(value)))
        }
        ParamKind::U64 => {
            let value = raw.parse::<u64>().context("parse u64 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::U64(value)))
        }
        ParamKind::F32 => {
            let value = raw.parse::<f32>().context("parse f32 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::F32(value)))
        }
        ParamKind::F64 => {
            let value = raw.parse::<f64>().context("parse f64 argument")?;
            Ok(EntrypointArg::Scalar(AbiScalarValue::F64(value)))
        }
        ParamKind::Buffer => {
            let bytes = parse_buffer_bytes(raw).context("parse buffer argument")?;
            Ok(EntrypointArg::Buffer(bytes))
        }
        ParamKind::Utf8 => Ok(EntrypointArg::Buffer(raw.as_bytes().to_vec())),
        ParamKind::Resource => {
            let value = raw.parse::<u64>().context("parse resource handle")?;
            Ok(EntrypointArg::Resource(value))
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

async fn spawn_module(
    runtime: &WasmtimeDriver,
    registry: &Arc<Registry>,
    spec: ModuleSpec,
) -> Result<ResourceId> {
    let process_id = registry
        .reserve(None, ResourceType::Process)
        .map_err(KernelError::from)
        .context("reserve process id")?;

    let ModuleSpec {
        module_label,
        module_path,
        entrypoint,
        capabilities,
        params,
        args,
    } = spec;

    let entrypoint_invocation =
        EntrypointInvocation::new(AbiSignature::new(params, Vec::new()), args).with_context(
            || format!("build entrypoint invocation for {module_label}"),
        )?;

    let module_id = module_path.to_str().ok_or_else(|| {
        WasmtimeError::Kernel(KernelError::Driver(format!(
            "module path for {module_label} is not valid UTF-8"
        )))
    })?;

    if let Err(err) = runtime
        .start(
            registry,
            process_id,
            module_id,
            &entrypoint,
            capabilities,
            entrypoint_invocation,
        )
        .await
    {
        registry.discard(process_id);
        return Err(err).with_context(|| format!("start module {module_label}"));
    }

    let registry_clone = Arc::clone(registry);
    tokio::spawn({
        let module_label = module_label.clone();
        async move {
            if let Err(err) = subscribe_module_logs(registry_clone, process_id, &module_label).await
            {
                warn!(
                    process_id,
                    err = err.to_string(),
                    module = %module_label,
                    "module log subscriber terminated"
                );
            }
        }
    });

    Ok(process_id)
}

async fn subscribe_module_logs(
    registry: Arc<Registry>,
    process_id: ResourceId,
    module_label: &str,
) -> Result<()> {
    let channel = wait_for_log_channel(&registry, process_id, module_label).await?;
    info!(process_id, module = %module_label, "subscribing to module logs");
    forward_log_stream(channel, module_label).await
}

async fn wait_for_log_channel(
    registry: &Arc<Registry>,
    process_id: ResourceId,
    module_label: &str,
) -> Result<Arc<Channel>> {
    let deadline = Instant::now() + LOG_CHANNEL_WAIT;

    loop {
        if let Some(handle) = lookup_log_handle(registry, process_id)
            && let Some(channel) = resolve_channel(registry, handle)
        {
            return Ok(channel);
        }

        if Instant::now() >= deadline {
            break;
        }

        sleep(LOG_POLL_INTERVAL).await;
    }

    Err(anyhow!("{module_label} did not register a log channel"))
}

fn lookup_log_handle(registry: &Arc<Registry>, process_id: ResourceId) -> Option<GuestResourceId> {
    registry
        .with::<ProcessHandle, _>(ResourceHandle::new(process_id), |process| {
            process.log_channel()
        })
        .flatten()
}

fn resolve_channel(registry: &Arc<Registry>, handle: GuestResourceId) -> Option<Arc<Channel>> {
    let channel_id = registry.resolve_shared(handle)?;
    registry.with(
        ResourceHandle::new(channel_id),
        |channel: &mut Arc<Channel>| channel.clone(),
    )
}

#[instrument(skip_all, fields(channel_id = format_args!("{:p}", channel.as_ref() as *const _)))]
async fn forward_log_stream(channel: Arc<Channel>, module_label: &str) -> Result<()> {
    let mut reader = channel.new_strong_reader();
    let span = Span::current();

    loop {
        match reader.read_frame(LOG_FRAME_CAPACITY).await {
            Ok((_, payload)) => render_log_frame(&span, module_label, &payload),
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionAborted | ErrorKind::Interrupted
                ) =>
            {
                break;
            }
            Err(err) => return Err(anyhow!("read logs from {module_label}: {err}")),
        }
    }

    Ok(())
}

fn render_log_frame(span: &Span, module_label: &str, payload: &[u8]) {
    match log_fb::root_as_log_record(payload) {
        Ok(record) => render_log_record(span, record),
        Err(err) => warn!(err = %err, module = %module_label, "invalid module log frame"),
    }
}

fn render_log_record(span: &Span, record: log_fb::LogRecord<'_>) {
    let target = record.target().unwrap_or_default();
    let message = record.message().unwrap_or_default();
    let span_path = record.spans().and_then(|span_vec| {
        let mut path = String::new();
        for span in span_vec.iter() {
            let Some(name) = span.name() else {
                continue;
            };
            if !path.is_empty() {
                path.push_str("::");
            }
            path.push_str(name);
        }
        if path.is_empty() { None } else { Some(path) }
    });
    let field_list = record.fields().and_then(|field_vec| {
        let mut list = String::new();
        for field in field_vec.iter() {
            let Some(key) = field.key() else {
                continue;
            };
            let Some(value) = field.value() else {
                continue;
            };
            if !list.is_empty() {
                list.push(' ');
            }
            list.push_str(key);
            list.push('=');
            list.push_str(value);
        }
        if list.is_empty() { None } else { Some(list) }
    });
    let span_path = span_path.as_deref();
    let field_list = field_list.as_deref();

    span.in_scope(|| match record.level() {
        LogLevel::Trace => {
            emit_guest_log_event!(Level::TRACE, target, message, span_path, field_list);
        }
        LogLevel::Debug => {
            emit_guest_log_event!(Level::DEBUG, target, message, span_path, field_list);
        }
        LogLevel::Info => {
            emit_guest_log_event!(Level::INFO, target, message, span_path, field_list);
        }
        LogLevel::Warn => {
            emit_guest_log_event!(Level::WARN, target, message, span_path, field_list);
        }
        LogLevel::Error => {
            emit_guest_log_event!(Level::ERROR, target, message, span_path, field_list);
        }
        _ => {
            emit_guest_log_event!(Level::INFO, target, message, span_path, field_list);
        }
    });
}
