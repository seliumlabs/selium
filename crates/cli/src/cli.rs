//! The clap command-line surface: global connection flags plus one
//! subcommand per control-plane verb.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// One subcommand per control-plane verb, mapping one-to-one onto
/// [`ControlRequest`](selium_client::selium_service::ControlRequest)
/// variants.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Record a deployment's desired state.
    Deploy {
        /// Workload identifier.
        workload: String,

        /// Desired replica count.
        #[arg(long)]
        replicas: u32,

        /// Module reference (manifest name or blob identity).
        #[arg(long)]
        module: String,
    },

    /// Scale a workload's desired state.
    Scale {
        /// Workload identifier.
        workload: String,

        /// New desired replica count.
        #[arg(long)]
        replicas: u32,
    },

    /// Stop a workload.
    Stop {
        /// Workload identifier.
        workload: String,
    },

    /// Read the last accepted desired state for a workload.
    Status {
        /// Workload identifier.
        workload: String,
    },

    /// Resolve a URI through discovery.
    Resolve {
        /// URI to resolve.
        uri: String,
    },

    /// Upload module bytes to the blob store and record a manifest.
    Upload {
        /// Manifest name for the stored module.
        #[arg(long)]
        manifest: String,

        /// Path to the module file whose bytes are stored.
        #[arg(long)]
        file: PathBuf,
    },
}

/// The Selium control-plane CLI.
///
/// Each invocation opens one QUIC connection, issues exactly one typed
/// control request, renders the result as one output line, and exits.
#[derive(Debug, Parser)]
#[command(
    name = "sel",
    version,
    about = "Drive the Selium control plane from the command line"
)]
pub struct Cli {
    /// QUIC connector address (`host:port`).
    #[arg(long)]
    pub connector: String,

    /// Server root certificate PEM to trust.
    #[arg(long)]
    pub ca: PathBuf,

    /// Client certificate PEM presented for mutual TLS.
    #[arg(long, requires = "client_key")]
    pub client_cert: PathBuf,

    /// Client private key PEM presented for mutual TLS.
    #[arg(long, requires = "client_cert")]
    pub client_key: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// The bridge-route server name (TLS SNI + certificate verification
    /// name): the bare root wire name of the single per-platform
    /// bridge-server. The requestor's tenant is established by the presented
    /// client certificate, not by the name dialled.
    pub fn server_name(&self) -> String {
        "bridge".to_string()
    }

    /// The control route named in the bridge channel handshake: the single
    /// per-platform control plane's root route. The session's tenant is the
    /// bridge-channel's process tenant, derived from the presented leaf.
    pub fn control_route(&self) -> String {
        "sel:///control".to_string()
    }
}
