use std::{
    io::ErrorKind,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
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
const GUEST_LOG_TARGET: &str = "selium.guest";

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

pub async fn remote_client(
    kernel: &Kernel,
    registry: &Arc<Registry>,
    domain: &str,
    port: u16,
    work_dir: impl AsRef<Path>,
) -> Result<ResourceId, <WasmtimeDriver as ProcessLifecycleCapability>::Error> {
    let entrypoint = EntrypointInvocation::new(
        AbiSignature::new(
            vec![AbiParam::Buffer, AbiParam::Scalar(AbiScalarType::U16)],
            Vec::new(),
        ),
        vec![
            EntrypointArg::Buffer(domain.as_bytes().into()),
            // Use a non-privileged port for local development to avoid bind failures.
            EntrypointArg::Scalar(AbiScalarValue::U16(port)),
        ],
    )
    .context("failed to construct entrypoint invocation")?;

    let runtime = kernel.get::<WasmtimeDriver>().ok_or_else(|| {
        WasmtimeError::Kernel(KernelError::Driver(
            "missing Wasmtime driver in kernel".to_string(),
        ))
    })?;
    let process_id = registry
        .reserve(None, ResourceType::Process)
        .map_err(KernelError::from)
        .map_err(<WasmtimeDriver as ProcessLifecycleCapability>::Error::from)?;

    let mut mod_path = work_dir.as_ref().to_owned();
    mod_path.push("selium_module_remote_client.wasm");

    let mod_path = mod_path.to_str().ok_or_else(|| {
        WasmtimeError::Kernel(KernelError::Driver(
            "switchboard module path is not valid UTF-8".to_string(),
        ))
    })?;

    if let Err(err) = runtime
        .start(
            registry,
            process_id,
            mod_path,
            "start",
            vec![
                Capability::ChannelLifecycle,
                Capability::ChannelReader,
                Capability::ChannelWriter,
                Capability::ProcessLifecycle,
                Capability::NetBind,
                Capability::NetAccept,
                Capability::NetRead,
                Capability::NetWrite,
            ],
            entrypoint,
        )
        .await
    {
        registry.discard(process_id);
        return Err(err);
    }

    let registry_clone = Arc::clone(registry);
    tokio::spawn(async move {
        if let Err(err) = subscribe_remote_client_logs(registry_clone, process_id).await {
            warn!(
                process_id,
                err = err.to_string(),
                "remote client log subscriber terminated"
            );
        }
    });

    Ok(process_id)
}

pub async fn switchboard(
    kernel: &Kernel,
    registry: &Arc<Registry>,
    work_dir: impl AsRef<Path>,
) -> Result<ResourceId, <WasmtimeDriver as ProcessLifecycleCapability>::Error> {
    let runtime = kernel.get::<WasmtimeDriver>().ok_or_else(|| {
        WasmtimeError::Kernel(KernelError::Driver(
            "missing Wasmtime driver in kernel".to_string(),
        ))
    })?;
    let process_id = registry
        .reserve(None, ResourceType::Process)
        .map_err(KernelError::from)
        .map_err(<WasmtimeDriver as ProcessLifecycleCapability>::Error::from)?;

    let entrypoint =
        EntrypointInvocation::new(AbiSignature::new(Vec::new(), Vec::new()), Vec::new())
            .context("failed to construct entrypoint invocation")?;

    let mut mod_path = work_dir.as_ref().to_owned();
    mod_path.push("selium_module_switchboard.wasm");

    if let Err(err) = runtime
        .start(
            registry,
            process_id,
            mod_path.to_str().expect("non-UTF8 path"),
            "start",
            vec![
                Capability::ChannelLifecycle,
                Capability::ChannelReader,
                Capability::ChannelWriter,
            ],
            entrypoint,
        )
        .await
    {
        registry.discard(process_id);
        return Err(err);
    }

    let registry_clone = Arc::clone(registry);
    tokio::spawn(async move {
        if let Err(err) = subscribe_switchboard_logs(registry_clone, process_id).await {
            warn!(
                process_id,
                err = err.to_string(),
                "switchboard log subscriber terminated"
            );
        }
    });

    Ok(process_id)
}

async fn subscribe_remote_client_logs(
    registry: Arc<Registry>,
    process_id: ResourceId,
) -> Result<()> {
    let channel = wait_for_log_channel(&registry, process_id).await?;
    info!(
        process_id,
        "subscribing to selium-module-remote-client logs"
    );
    forward_log_stream(channel).await
}

async fn subscribe_switchboard_logs(registry: Arc<Registry>, process_id: ResourceId) -> Result<()> {
    let channel = wait_for_log_channel(&registry, process_id).await?;
    info!(process_id, "subscribing to selium-module-switchboard logs");
    forward_log_stream(channel).await
}

async fn wait_for_log_channel(
    registry: &Arc<Registry>,
    process_id: ResourceId,
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

    Err(anyhow!(
        "selium-module-remote-client did not register a log channel"
    ))
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
async fn forward_log_stream(channel: Arc<Channel>) -> Result<()> {
    let mut reader = channel.new_strong_reader();
    let span = Span::current();

    loop {
        match reader.read_frame(LOG_FRAME_CAPACITY).await {
            Ok((_, payload)) => render_log_frame(&span, &payload),
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionAborted | ErrorKind::Interrupted
                ) =>
            {
                break;
            }
            Err(err) => return Err(anyhow!("read remote client logs: {err}")),
        }
    }

    Ok(())
}

fn render_log_frame(span: &Span, payload: &[u8]) {
    match log_fb::root_as_log_record(payload) {
        Ok(record) => render_log_record(span, record),
        Err(err) => warn!(err = %err, "invalid remote client log frame"),
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
