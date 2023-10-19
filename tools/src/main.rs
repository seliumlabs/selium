use anyhow::Result;
use clap::Parser;
use selium_dev_tools::cli::{Commands, ToolsCli};
use selium_dev_tools::commands::gen_certs::GenCertsRunner;
use selium_dev_tools::traits::CommandRunner;

fn main() -> Result<()> {
    let cli = ToolsCli::parse();

    match cli.command {
        Commands::GenCerts(args) => GenCertsRunner::from(args).run()?,
    };

    Ok(())
}
