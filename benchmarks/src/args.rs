use clap::Parser;

#[derive(Debug, Parser)]
pub struct Args {
    /// The number of messages to exchange
    #[arg(long, default_value_t = 1_000_000)]
    pub num_of_messages: u64,

    /// The number of streams to use with multiplexing
    #[arg(long, default_value_t = 10)]
    pub num_of_streams: u64,

    /// Size (in bytes) of the message payload
    #[arg(long, default_value_t = 32)]
    pub message_size: u64,
}
