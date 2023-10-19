use anyhow::Result;
use clap::Parser;
use selium_benchmarks::{args::Args, runner::BenchmarkRunner};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let runner = BenchmarkRunner::init().await?;
    let results = runner.run(args).await?;

    println!("{results}");

    Ok(())
}
