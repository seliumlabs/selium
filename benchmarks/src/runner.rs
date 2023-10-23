use crate::{args::Args, results::BenchmarkResults};
use anyhow::Result;
use clap::Parser;
use futures::SinkExt;
use futures::{future::join_all, StreamExt};
use selium::batching::BatchConfig;
use selium::std::codecs::StringCodec;
use selium::std::compression::lz4::{Lz4Comp, Lz4Decomp};
use selium::{prelude::*, Client};
use selium_server::args::UserArgs;
use selium_server::server::Server;
use std::time::Instant;

const SERVER_ADDR: &str = "127.0.0.1:7001";

fn start_server() -> Result<()> {
    let args = UserArgs::parse_from([
        "",
        "--cert",
        "certs/server/localhost.der",
        "--key",
        "certs/server/localhost.key.der",
        "--ca",
        "certs/server/ca.der",
        "-vvvv",
    ]);

    let server = Server::try_from(args)?;

    tokio::spawn(async move {
        server.listen().await.expect("Failed to spawn server");
    });

    Ok(())
}

fn generate_message(message_size: usize) -> String {
    (0..message_size)
        .map(|i| (i % 25 + 97) as u8 as char)
        .collect()
}

pub struct BenchmarkRunner {
    connection: Client,
}

impl BenchmarkRunner {
    pub async fn init() -> Result<Self> {
        start_server()?;

        let connection = selium::client()
            .with_certificate_authority("certs/client/ca.der")?
            .with_cert_and_key(
                "certs/client/localhost.der",
                "certs/client/localhost.key.der",
            )?
            .connect(SERVER_ADDR)
            .await?;

        Ok(Self { connection })
    }

    pub async fn run(&self, args: Args) -> Result<BenchmarkResults> {
        let mut tasks = Vec::with_capacity(args.num_of_streams as usize);
        let message = generate_message(args.message_size as usize);
        let start = Instant::now();

        let mut subscriber = self
            .connection
            .subscriber("/acmeco/stocks")
            .with_decoder(StringCodec);

        if args.enable_compression {
            subscriber = subscriber.with_decompression(Lz4Decomp);
        }

        let mut subscriber = subscriber.open().await?;

        for _ in 0..args.num_of_streams {
            let mut publisher = self
                .connection
                .publisher("/acmeco/stocks")
                .with_encoder(StringCodec);

            if args.enable_batching {
                publisher = publisher.with_batching(BatchConfig::high_throughput());
            }

            if args.enable_compression {
                publisher = publisher.with_compression(Lz4Comp);
            }

            let mut publisher = publisher.open().await?;
            let message = message.clone();

            let handle = tokio::spawn(async move {
                for _ in 0..args.num_of_messages / args.num_of_streams {
                    publisher.send(message.to_owned()).await.unwrap();
                }

                publisher.finish().await.unwrap();
            });

            tasks.push(handle);
        }

        let handle = tokio::spawn(async move {
            for _ in 0..args.num_of_messages {
                let _ = subscriber.next().await;
            }
        });

        tasks.push(handle);
        join_all(tasks).await;
        let elapsed = start.elapsed();

        Ok(BenchmarkResults::calculate(elapsed, args))
    }
}
