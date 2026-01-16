//! End-to-end request/reply test for Selium.

use std::{
    env,
    ffi::OsStr,
    fs,
    io,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use cargo::{
    core::{Shell, Workspace, compiler::CompileKind, compiler::UserIntent},
    ops::{self, CompileFilter, CompileOptions, Packages},
    util::{GlobalContext, homedir},
};
use futures::StreamExt;
use selium_remote_client::{
    AbiParam, AbiScalarType, AbiSignature, Capability, Channel, Client, ClientConfigBuilder,
    ClientError, Process, ProcessBuilder, Subscriber,
};
use selium_userland::fbs::selium::logging as log_fb;
use tokio::time::{sleep, timeout};

const REQUEST_REPLY_MODULE: &str = "selium_test_request_reply.wasm";
const REMOTE_CLIENT_MODULE: &str = "selium_remote_client_server.wasm";
const RUNTIME_BIN: &str = "selium-runtime";
const RUNTIME_URL: &str =
    "https://github.com/seliumlabs/selium/releases/latest/download/selium-runtime-x86_64-unknown-linux-gnu.tar.gz";
const REMOTE_CLIENT_URL: &str = "https://github.com/seliumlabs/selium-modules/releases/latest/download/selium-remote-client-server-wasm32-unknown-unknown.tar.gz";
const ARTIFACTS_DIR: &str = "request-reply-artifacts";
const ARCHIVE_NAME: &str = "artifact.tar.gz";
const SINGLETON_ENTRYPOINT: &str = "singleton_stub";
const LOG_CHUNK_SIZE: u32 = 64 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_TIMEOUT: Duration = Duration::from_secs(10);

struct RuntimeGuard {
    child: Child,
}

impl RuntimeGuard {
    fn start(runtime_path: &Path, work_dir: &Path, module_spec: &str) -> Result<Self> {
        let mut command = Command::new(runtime_path);
        command
            .arg("--work-dir")
            .arg(".")
            .arg("--module")
            .arg(module_spec)
            .current_dir(work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let child = command.spawn().context("start selium-runtime")?;
        Ok(Self { child })
    }
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        if let Err(err) = self.child.kill() {
            eprintln!("failed to terminate selium-runtime: {err}");
        }
        if let Err(err) = self.child.wait() {
            eprintln!("failed to reap selium-runtime: {err}");
        }
    }
}

struct WorkDir {
    path: PathBuf,
}

impl WorkDir {
    fn new() -> Result<Self> {
        let base = env::temp_dir();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("fetch timestamp")?
            .as_millis();
        let path = base.join(format!("selium-request-reply-{timestamp}-{}", process_id()));
        fs::create_dir_all(&path).with_context(|| format!("create work dir {path:?}"))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_dir_all(&self.path) {
            eprintln!("failed to remove work dir {:?}: {err}", self.path);
        }
    }
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("resolve workspace root"))
}

fn process_id() -> u32 {
    std::process::id()
}

fn find_available_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    let port = listener
        .local_addr()
        .context("resolve local address")?
        .port();
    Ok(port)
}

fn generate_certs(runtime_path: &Path, work_dir: &Path) -> Result<()> {
    let certs_dir = work_dir.join("certs");
    let mut command = Command::new(runtime_path);
    command
        .arg("generate-certs")
        .arg("--output-dir")
        .arg(&certs_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    run_command(&mut command, "generate certs")
}

fn build_runtime(workspace_root: &Path) -> Result<PathBuf> {
    download_artifact(
        workspace_root,
        RUNTIME_URL,
        "runtime",
        RUNTIME_BIN,
    )
    .context("download selium-runtime")
}

fn build_remote_client() -> Result<PathBuf> {
    let workspace_root = workspace_root()?;
    download_artifact(
        &workspace_root,
        REMOTE_CLIENT_URL,
        "remote-client",
        REMOTE_CLIENT_MODULE,
    )
    .context("download selium-remote-client-server")
}

fn build_request_reply_module(workspace_root: &Path) -> Result<PathBuf> {
    cargo_compile(
        workspace_root,
        "selium-test-request-reply",
        Some("wasm32-unknown-unknown"),
        CompileFilter::lib_only(),
    )
    .context("compile selium-test-request-reply")?;
    Ok(wasm_module_path(workspace_root, REQUEST_REPLY_MODULE))
}

fn run_command(command: &mut Command, label: &str) -> Result<()> {
    let status = command.status().with_context(|| format!("run {label}"))?;
    if !status.success() {
        bail!("{label} failed with status {status}");
    }
    Ok(())
}

fn download_artifact(
    workspace_root: &Path,
    url: &str,
    subdir: &str,
    expected_filename: &str,
) -> Result<PathBuf> {
    let artifacts_root = workspace_root.join("target").join(ARTIFACTS_DIR);
    let artifact_root = artifacts_root.join(subdir);
    if let Some(path) = find_artifact(&artifact_root, expected_filename)? {
        return Ok(path);
    }
    if artifact_root.exists() {
        fs::remove_dir_all(&artifact_root)
            .with_context(|| format!("clean artifact dir {}", artifact_root.display()))?;
    }
    fs::create_dir_all(&artifact_root)
        .with_context(|| format!("create artifact dir {}", artifact_root.display()))?;
    let archive_path = artifact_root.join(ARCHIVE_NAME);
    download_file(url, &archive_path)
        .with_context(|| format!("download artifact from {url}"))?;
    extract_archive(&archive_path, &artifact_root).context("extract artifact")?;
    fs::remove_file(&archive_path)
        .with_context(|| format!("remove archive {}", archive_path.display()))?;
    find_artifact(&artifact_root, expected_filename)?.ok_or_else(|| {
        anyhow!(
            "locate artifact {expected_filename} in {}",
            artifact_root.display()
        )
    })
}

fn download_file(url: &str, destination: &Path) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .build()
        .context("build http client")?;
    let response = client
        .get(url)
        .send()
        .context("send download request")?;
    let mut response = response
        .error_for_status()
        .context("download response status")?;
    let mut output =
        fs::File::create(destination).with_context(|| format!("create {}", destination.display()))?;
    io::copy(&mut response, &mut output).context("write download payload")?;
    Ok(())
}

fn extract_archive(archive_path: &Path, destination: &Path) -> Result<()> {
    let mut command = Command::new("tar");
    command
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(destination);
    run_command(&mut command, "extract artifact")
}

fn find_artifact(root: &Path, expected_filename: &str) -> Result<Option<PathBuf>> {
    if !root.exists() {
        return Ok(None);
    }
    let entries = fs::read_dir(root).with_context(|| format!("read dir {}", root.display()))?;
    for entry in entries {
        let entry = entry.context("read dir entry")?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_artifact(&path, expected_filename)? {
                return Ok(Some(found));
            }
        } else if path.file_name() == Some(OsStr::new(expected_filename)) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn cargo_compile(
    workspace_root: &Path,
    package: &str,
    target: Option<&str>,
    filter: CompileFilter,
) -> Result<()> {
    let gctx = cargo_context(workspace_root)?;
    let manifest_path = workspace_root.join("Cargo.toml");
    let ws = Workspace::new(&manifest_path, &gctx).context("load cargo workspace")?;
    let mut options =
        CompileOptions::new(&gctx, UserIntent::Build).context("build cargo compile options")?;
    options.spec = Packages::Packages(vec![package.to_string()]);
    options.filter = filter;
    if let Some(target) = target {
        options.build_config.requested_kinds =
            CompileKind::from_requested_targets(&gctx, &[target.to_string()])
                .context("configure build target")?;
    } else {
        options.build_config.requested_kinds = vec![CompileKind::Host];
    }
    ops::compile(&ws, &options).context("compile workspace")?;
    Ok(())
}

fn cargo_context(workspace_root: &Path) -> Result<GlobalContext> {
    let home = homedir(workspace_root).ok_or_else(|| anyhow!("resolve cargo home"))?;
    let shell = Shell::new();
    let mut gctx = GlobalContext::new(shell, workspace_root.to_path_buf(), home);
    gctx.configure(0, false, None, false, false, false, &None, &[], &[])
        .context("configure cargo")?;
    Ok(gctx)
}

fn wasm_module_path(workspace_root: &Path, module_name: &str) -> PathBuf {
    workspace_root
        .join("target/wasm32-unknown-unknown/debug")
        .join(module_name)
}

fn copy_module(source: PathBuf, modules_dir: &Path) -> Result<PathBuf> {
    let filename = source
        .file_name()
        .ok_or_else(|| anyhow!("resolve module filename"))?;
    let destination = modules_dir.join(filename);
    fs::copy(&source, &destination)
        .with_context(|| format!("copy module from {}", source.display()))?;
    Ok(destination)
}

async fn connect_client(port: u16, work_dir: &Path) -> Result<Client> {
    let certs_dir = work_dir.join("certs");
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut last_error = None;

    while Instant::now() < deadline {
        let client = ClientConfigBuilder::default()
            .domain("localhost")
            .port(port)
            .certificate_directory(&certs_dir)
            .connect()
            .await;

        match client {
            Ok(client) => return Ok(client),
            Err(err) => {
                last_error = Some(err);
                sleep(Duration::from_millis(100)).await;
            }
        }
    }

    if let Some(err) = last_error {
        return Err(err).context("connect to runtime");
    }

    Err(anyhow!("connect to runtime"))
}

async fn start_server(client: &Client) -> Result<Process> {
    let builder = ProcessBuilder::new(REQUEST_REPLY_MODULE, "request_reply_server")
        .capability(Capability::ChannelReader)
        .capability(Capability::ChannelWriter)
        .capability(Capability::ChannelLifecycle);

    Process::start(client, builder)
        .await
        .context("launch request-reply server")
}

async fn start_client(client: &Client, request_id: u64) -> Result<Process> {
    let signature = AbiSignature::new(vec![AbiParam::Scalar(AbiScalarType::U64)], Vec::new());
    let builder = ProcessBuilder::new(REQUEST_REPLY_MODULE, "request_reply_client")
        .capability(Capability::ChannelReader)
        .capability(Capability::ChannelWriter)
        .capability(Capability::ChannelLifecycle)
        .signature(signature)
        .arg_resource(request_id);

    Process::start(client, builder)
        .await
        .context("launch request-reply client")
}

async fn start_singleton_stub(client: &Client) -> Result<Process> {
    let builder = ProcessBuilder::new(REQUEST_REPLY_MODULE, SINGLETON_ENTRYPOINT)
        .capability(Capability::ChannelLifecycle)
        .capability(Capability::SingletonRegistry)
        .capability(Capability::SingletonLookup);

    Process::start(client, builder)
        .await
        .context("launch singleton stub")
}

async fn wait_for_log_channel(process: &Process, timeout_window: Duration) -> Result<Channel> {
    let deadline = Instant::now() + timeout_window;
    loop {
        match process.log_channel().await {
            Ok(channel) => return Ok(channel),
            Err(ClientError::Remote(message))
                if message.to_ascii_lowercase().contains("not found")
                    && Instant::now() < deadline =>
            {
                sleep(Duration::from_millis(50)).await;
            }
            Err(err) => return Err(err).context("fetch process log channel"),
        }
    }
}

async fn wait_for_log_field(
    subscriber: &mut Subscriber,
    field_key: &str,
    timeout_window: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout_window;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            bail!("timed out waiting for log field {field_key}");
        }
        let next_frame = timeout(remaining, subscriber.next()).await;

        match next_frame {
            Ok(Some(Ok(bytes))) => {
                let record =
                    log_fb::root_as_log_record(&bytes).map_err(|_| anyhow!("decode log record"))?;
                if let Some(value) = extract_log_field(record, field_key) {
                    return Ok(value);
                }
            }
            Ok(Some(Err(err))) => return Err(err).context("read log frame"),
            Ok(None) => bail!("log channel closed before finding {field_key}"),
            Err(_) => bail!("timed out waiting for log field {field_key}"),
        }
    }
}

async fn subscribe_log_channel(channel: &Channel) -> Result<Subscriber> {
    channel
        .subscribe_shared(LOG_CHUNK_SIZE)
        .await
        .context("subscribe to log channel")
}

async fn drain_log_frames(subscriber: &mut Subscriber, timeout_window: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout_window;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            return Ok(());
        }
        match timeout(remaining, subscriber.next()).await {
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(err))) => return Err(err).context("read log frame"),
            Ok(None) => return Ok(()),
            Err(_) => return Ok(()),
        }
    }
}

fn extract_log_field(record: log_fb::LogRecord<'_>, field_key: &str) -> Option<String> {
    record.fields().and_then(|fields| {
        fields.iter().find_map(|field| {
            let key = field.key()?;
            let value = field.value()?;
            if key == field_key {
                Some(value.to_string())
            } else {
                None
            }
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "end-to-end tests run separately"]
async fn request_reply_end_to_end() -> Result<()> {
    let workspace_root = workspace_root()?;
    let work_dir = WorkDir::new()?;
    let modules_dir = work_dir.path().join("modules");
    fs::create_dir_all(&modules_dir).context("create modules directory")?;

    let runtime_path = build_runtime(&workspace_root).context("build runtime")?;
    generate_certs(&runtime_path, work_dir.path()).context("generate certificates")?;
    let remote_client_path = build_remote_client().context("build remote-client")?;
    let request_reply_path =
        build_request_reply_module(&workspace_root).context("build request-reply module")?;

    copy_module(remote_client_path, &modules_dir).context("install remote-client module")?;

    copy_module(request_reply_path, &modules_dir).context("install request-reply module")?;

    let port = find_available_port().context("select available port")?;
    let module_spec = format!(
        "path={REMOTE_CLIENT_MODULE};capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,ProcessLifecycle,NetQuicBind,NetQuicAccept,NetQuicRead,NetQuicWrite;args=utf8:localhost,u16:{port}"
    );
    let _runtime = RuntimeGuard::start(&runtime_path, work_dir.path(), &module_spec)
        .context("start selium-runtime")?;

    let client = connect_client(port, work_dir.path()).await?;
    let server = start_server(&client).await.context("start server")?;

    let server_log_channel = wait_for_log_channel(&server, LOG_TIMEOUT).await?;
    let mut server_logs = subscribe_log_channel(&server_log_channel).await?;
    let request_id = wait_for_log_field(&mut server_logs, "request_id", LOG_TIMEOUT).await?;
    let request_id = request_id.parse::<u64>().context("parse request id")?;

    let client_process = start_client(&client, request_id)
        .await
        .context("start client")?;
    let client_log_channel = wait_for_log_channel(&client_process, LOG_TIMEOUT).await?;
    let mut client_logs = subscribe_log_channel(&client_log_channel).await?;
    let reply = wait_for_log_field(&mut client_logs, "reply", LOG_TIMEOUT).await?;

    if reply != "ping" {
        bail!("unexpected reply payload: {reply}");
    }

    client_process
        .stop()
        .await
        .context("stop request-reply client")?;
    server.stop().await.context("stop request-reply server")?;
    drain_log_frames(&mut client_logs, Duration::from_millis(500)).await?;
    drain_log_frames(&mut server_logs, Duration::from_millis(500)).await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "end-to-end tests run separately"]
async fn singleton_round_trip_end_to_end() -> Result<()> {
    let workspace_root = workspace_root()?;
    let work_dir = WorkDir::new()?;
    let modules_dir = work_dir.path().join("modules");
    fs::create_dir_all(&modules_dir).context("create modules directory")?;

    let runtime_path = build_runtime(&workspace_root).context("build runtime")?;
    generate_certs(&runtime_path, work_dir.path()).context("generate certificates")?;
    let remote_client_path = build_remote_client().context("build remote-client")?;
    let request_reply_path =
        build_request_reply_module(&workspace_root).context("build request-reply module")?;

    copy_module(remote_client_path, &modules_dir).context("install remote-client module")?;

    copy_module(request_reply_path, &modules_dir).context("install request-reply module")?;

    let port = find_available_port().context("select available port")?;
    let module_spec = format!(
        "path={REMOTE_CLIENT_MODULE};capabilities=ChannelLifecycle,ChannelReader,ChannelWriter,ProcessLifecycle,NetQuicBind,NetQuicAccept,NetQuicRead,NetQuicWrite;args=utf8:localhost,u16:{port}"
    );
    let _runtime = RuntimeGuard::start(&runtime_path, work_dir.path(), &module_spec)
        .context("start selium-runtime")?;

    let client = connect_client(port, work_dir.path()).await?;
    let singleton_process = start_singleton_stub(&client)
        .await
        .context("start singleton stub")?;

    let log_channel = wait_for_log_channel(&singleton_process, LOG_TIMEOUT).await?;
    let mut logs = subscribe_log_channel(&log_channel).await?;
    let status = wait_for_log_field(&mut logs, "singleton_status", LOG_TIMEOUT).await?;

    if status != "ok" {
        bail!("unexpected singleton status: {status}");
    }

    singleton_process
        .stop()
        .await
        .context("stop singleton stub")?;
    drain_log_frames(&mut logs, Duration::from_millis(500)).await?;

    Ok(())
}
