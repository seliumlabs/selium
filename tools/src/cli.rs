use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about)]
pub struct ToolsCli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Generate self-signed CA and keypairs for use with testing and development.
    GenCerts(GenCertsArgs),
}

#[derive(Args)]
pub struct GenCertsArgs {
    /// Output path for server certs
    #[clap(short = 's', default_value = "../certs/server/")]
    pub server_out_path: PathBuf,

    /// Output path for client certs
    #[clap(short = 'c', default_value = "../certs/client/")]
    pub client_out_path: PathBuf,
}
