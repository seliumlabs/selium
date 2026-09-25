//! The `sel` command-line client.
//!
//! A thin native wrapper over `selium-client` driving the control plane's
//! typed control surface over QUIC through the bridge. Each invocation opens
//! one connection, issues exactly one [`ControlRequest`], renders the
//! [`ControlResponse`] to a line and an exit code, and exits.
//!
//! The command-mapping seams (`build_request`, `render`) are pure and
//! unit-tested natively. A full external end-to-end test against the WASM
//! guests — the CLI half of [`crates/runtime/tests/control_plane_bridge.rs`]
//! minus its test harness — is deferred to a follow-up: this crate stays
//! connection-agnostic and is exercised here against the typed seams only.

use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::Parser;
use selium_client::selium_service::{ControlRequest, ControlResponse};

mod cli;
mod commands;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Establishes one connection, performs one request/reply, renders it, and
/// returns the resulting exit code.
#[tokio::main]
async fn run(cli: cli::Cli) -> Result<ExitCode> {
    let request = commands::build_request(&cli.command)?;

    let options = commands::build_connect_options(&cli)?;
    let address: std::net::SocketAddr = cli
        .connector
        .parse()
        .with_context(|| format!("invalid connector address {}", cli.connector))?;
    let route = cli.control_route();

    let client = selium_client::connect(address, options).await?;
    let mut rpc = client
        .rpc::<ControlRequest, ControlResponse>(&route)
        .await
        .with_context(|| format!("opening control channel {route}"))?;
    let response = rpc.request(request).await?;
    let (line, code) = commands::render(&cli.command, response);

    // Drop the handles to tear down the channel and the connection along the
    // bridge's normal teardown path before the process exits.
    drop(rpc);
    drop(client);

    println!("{line}");
    Ok(code)
}
