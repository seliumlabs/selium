use anyhow::Result;
use clap::Parser;
use env_logger::Builder;
use log::error;
use selium_server::{args::UserArgs, server::Server};

#[tokio::main]
async fn main() -> Result<()> {
    let args = UserArgs::parse();

    let mut logger = Builder::new();
    logger
        .filter_module(
            &env!("CARGO_PKG_NAME").replace('-', "_"),
            args.verbose.log_level_filter(),
        )
        .init();

    let server = Server::try_from(args)?;

    if let Err(e) = server.listen().await {
        error!("Error occurred while accepting connections: {:?}", e);
    }

    Ok(())
}
