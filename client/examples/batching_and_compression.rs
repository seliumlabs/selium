use std::time::Duration;
use anyhow::Result;
use futures::{SinkExt, StreamExt};
use selium::batching::BatchConfig;
use selium::prelude::*;
use selium::pubsub::DeliveryGuarantee;
use selium::std::codecs::StringCodec;
use selium::std::compression::deflate::DeflateComp;
use selium::std::compression::deflate::DeflateDecomp;
use selium::std::traits::compression::CompressionLevel;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::custom()
        .keep_alive(5_000)?
        .endpoint("127.0.0.1:7001")
        .with_certificate_authority("../certs/client/ca.der")?
        .with_cert_and_key(
            "../certs/client/localhost.der",
            "../certs/client/localhost.key.der",
        )?
        .connect()
        .await?;

    let mut publisher = connection
        .publisher("/acmeco/stocks")
        .with_encoder(StringCodec)
        .with_compression(DeflateComp::gzip().fastest())
        .with_delivery_guarantee(DeliveryGuarantee::AtMostOnce)
        .with_batching(BatchConfig::high_throughput())
        .open()
        .await?;

    let mut subscriber = connection
        .subscriber("/acmeco/stocks")
        .with_decoder(StringCodec)
        .with_decompression(DeflateDecomp::gzip())
        .open()
        .await?;

    let subscribe_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = subscriber.next().await {
            println!("Received message: {msg}");
        }
    });

    let publish_task = tokio::spawn(async move {
        for _ in 0..=1000 {
            publisher.send("Hello, world!".to_owned()).await.unwrap();
        }

        publisher.finish().await.unwrap();
    });

    let _ = tokio::join!(subscribe_task, publish_task);

    Ok(())
}
