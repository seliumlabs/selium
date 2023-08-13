use std::time::Instant;
use anyhow::Result;
use futures::{SinkExt, StreamExt};
use selium::codecs::StringCodec;
use selium::prelude::*;

const NUM_STREAMS: u64 = 10;
const NUM_MESSAGES: u64 = 1_000_000;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    let instant = Instant::now();

    let mut subscriber = connection
        .subscriber("/acmeco/stocks")
        .with_decoder(StringCodec)
        .open()
        .await
        .unwrap();

    for _ in 0..NUM_STREAMS { 
        tokio::spawn({
            let mut publisher = connection
                .publisher("/acmeco/stocks")
                .with_encoder(StringCodec)
                .open()
                .await
                .unwrap();

            async move {
                for _ in 0..NUM_MESSAGES / NUM_STREAMS {
                    publisher.send("Hello, world!").await.unwrap();
                }

                publisher.finish().await.unwrap();
            }
        });
    }

    let subscribe_task = tokio::spawn(async move {
        for _ in 0..NUM_MESSAGES {
            let _ = subscriber.next().await.unwrap();
        }
    });

    subscribe_task.await.unwrap();

    println!("Time elapsed: {:?}", instant.elapsed());

    Ok(())
}
