use crate::{args::Args, results::BenchmarkResults};
use anyhow::Result;
use futures::{future::join_all, SinkExt, StreamExt};
use selium::{codecs::StringCodec, prelude::*, Client};
use std::{
    process::{Child, Command},
    time::Instant,
};

const SERVER_ADDR: &str = "127.0.0.1:7001";

fn start_server() -> Child {
    Command::new(env!("CARGO"))
        .args([
            "run",
            "--release",
            "--",
            "--bind-addr",
            SERVER_ADDR,
            "--cert",
            "benchmarks/certs/ca.crt",
            "--key",
            "benchmarks/certs/ca.key",
        ])
        .current_dir("..")
        .spawn()
        .expect("Failed to start server")
}

fn generate_message(message_size: usize) -> String {
    (0..message_size)
        .map(|i| (i % 25 + 97) as u8 as char)
        .collect()
}

pub struct BenchmarkRunner {
    server_handle: Child,
    connection: Client,
}

impl BenchmarkRunner {
    pub async fn init() -> Result<Self> {
        let server_handle = start_server();

        let connection = selium::client()
            .with_certificate_authority("certs/ca.crt")?
            .connect(SERVER_ADDR)
            .await?;

        Ok(Self {
            server_handle,
            connection,
        })
    }

    pub async fn run(&self, args: Args) -> Result<BenchmarkResults> {
        let mut tasks = Vec::with_capacity((args.num_of_streams + 1) as usize);
        let message = generate_message(args.message_size as usize);
        let start = Instant::now();

        let mut subscriber = self
            .connection
            .subscriber("/acmeco/stocks")
            .with_decoder(StringCodec)
            .open()
            .await?;

        for _ in 0..args.num_of_streams {
            let mut publisher = self
                .connection
                .publisher("/acmeco/stocks")
                .with_encoder(StringCodec)
                .open()
                .await?;

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
                let _ = subscriber.next().await.unwrap();
            }
        });

        tasks.push(handle);
        join_all(tasks).await;
        let elapsed = start.elapsed();

        Ok(BenchmarkResults::calculate(elapsed, args))
    }
}

impl Drop for BenchmarkRunner {
    fn drop(&mut self) {
        self.server_handle.kill().unwrap();
    }
}
