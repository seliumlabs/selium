use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use selium_kernel::{Kernel, drivers::Capability, registry::Registry, session::Session};
use tokio::{signal, sync::Notify};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt::time::SystemTime};

mod certs;
mod kernel;
mod modules;

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
enum LogFormat {
    /// Human-friendly text logs suitable for local development.
    Text,
    /// JSON logs for ingestion into systems such as Loki or OTLP collectors.
    Json,
}

#[derive(Parser, Debug)]
#[command(version, about = "Selium host runtime")]
struct ServerOptions {
    /// Log output format (text or JSON) for tracing events.
    #[arg(long, env = "SELIUM_LOG_FORMAT", default_value = "text")]
    log_format: LogFormat,
    /// Domain name (SNI) that the control-plane listener routes for.
    #[arg(long, env = "SELIUM_DOMAIN", default_value = "localhost")]
    domain: String,
    /// Port that the control-plane listener binds to.
    #[arg(long, env = "SELIUM_PORT", default_value_t = 7000)]
    port: u16,
    #[command(subcommand)]
    command: Option<ServerCommand>,
    /// Base directory where certificates and WASM modules are stored.
    #[arg(short, long, env = "SELIUM_WORK_DIR", default_value_os = ".")]
    work_dir: PathBuf,
}

#[derive(Subcommand, Debug)]
enum ServerCommand {
    /// Generate a local CA plus server and client certificate pairs.
    GenerateCerts(GenerateCertsArgs),
}

#[derive(Args, Debug)]
struct GenerateCertsArgs {
    /// Directory to write certificate and key files to.
    #[arg(long, default_value = "certs")]
    output_dir: PathBuf,
    /// Common Name to embed in the generated CA.
    #[arg(long, default_value = "Selium Local CA")]
    ca_common_name: String,
    /// DNS name to embed in the server certificate.
    #[arg(long, default_value = "localhost")]
    server_name: String,
    /// DNS name to embed in the client certificate.
    #[arg(long, default_value = "client.localhost")]
    client_name: String,
}

async fn run(
    kernel: Kernel,
    registry: Arc<Registry>,
    shutdown: Arc<Notify>,
    domain: &str,
    port: u16,
    work_dir: impl AsRef<Path>,
) -> Result<()> {
    info!("kernel initialised; starting host bridge");

    // This would normally be done by the Orchestrator, however during bootstrap we
    // have a chicken-and-egg problem, so we construct the session manually.
    let entitlements = vec![
        Capability::SessionLifecycle,
        Capability::ChannelLifecycle,
        Capability::ChannelReader,
        Capability::ChannelWriter,
        Capability::ProcessLifecycle,
        Capability::NetBind,
        Capability::NetAccept,
        Capability::NetConnect,
        Capability::NetRead,
        Capability::NetWrite,
    ];
    let _session = Session::bootstrap(entitlements, [0; 32]);
    // @todo Store session in Registry, then pass FuncParam::Resource(id) to host bridge

    modules::switchboard(&kernel, &registry, &work_dir).await?;
    modules::remote_client(&kernel, &registry, domain, port, work_dir).await?;

    signal::ctrl_c().await?;

    shutdown.notify_waiters();

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

#[tokio::main]
async fn main() -> Result<()> {
    // Parse stdin
    let args = ServerOptions::parse();

    // Initialise logging
    initialise_tracing(args.log_format)?;

    if let Some(ServerCommand::GenerateCerts(cert_args)) = &args.command {
        certs::generate_certificates(
            &cert_args.output_dir,
            &cert_args.ca_common_name,
            &cert_args.server_name,
            &cert_args.client_name,
        )?;
        return Ok(());
    }

    let (kernel, shutdown) = kernel::build(&args.work_dir).context("build runtime kernel")?;
    let registry = Registry::new();
    run(
        kernel,
        registry,
        shutdown,
        &args.domain,
        args.port,
        &args.work_dir,
    )
    .await
}
