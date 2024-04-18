use clap::{Args, Parser};
use clap_verbosity_flag::Verbosity;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct UserArgs {
    /// Address to bind this server to
    #[clap(short = 'a', long = "bind-addr", default_value = "127.0.0.1:7001")]
    pub bind_addr: SocketAddr,

    #[clap(flatten)]
    pub cert: CertGroup,

    #[clap(flatten)]
    pub log: LogArgs,

    /// Enable stateless retries
    #[clap(long = "stateless-retry")]
    pub stateless_retry: bool,

    /// File to log TLS keys to for debugging
    #[clap(long = "keylog")]
    pub keylog: bool,

    /// Maximum time in ms a client can idle waiting for data - default to 15 seconds
    #[clap(long = "max-idle-timeout", default_value_t = 15000, value_parser = clap::value_parser!(u32))]
    pub max_idle_timeout: u32,

    /// Can be called multiple times to increase output
    #[clap(flatten)]
    pub verbose: Verbosity,
}

#[derive(Args, Debug)]
pub struct CertGroup {
    /// CA certificate
    #[clap(long, default_value = "certs/server/ca.der")]
    pub ca: PathBuf,
    /// TLS private key
    #[clap(
        short = 'k',
        long = "key",
        default_value = "certs/server/localhost.key.der"
    )]
    pub key: PathBuf,
    /// TLS certificate
    #[clap(
        short = 'c',
        long = "cert",
        default_value = "certs/server/localhost.der"
    )]
    pub cert: PathBuf,
}

#[derive(Args, Debug)]
pub struct LogArgs {
    /// Path to directory to store log segments.
    #[clap(long, default_value = "logs/")]
    pub log_segments_directory: PathBuf,

    /// Interval in seconds to poll log cleaner task - default to 5 minutes.
    #[clap(long, default_value_t = 300_000)]
    pub log_cleaner_interval: u64,

    /// Maximum number of entries per log segment.
    #[clap(long, default_value_t = 100_000)]
    pub log_maximum_entries: u32,

    /// Number of writes before flushing log to filesystem.
    #[clap(long)]
    pub flush_policy_num_writes: Option<u64>,

    /// Interval in millis to asynchronously flush log to filesystem.
    #[clap(long, default_value_t = 3000)]
    pub flush_policy_interval: u64,
}
