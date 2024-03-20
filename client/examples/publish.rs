use anyhow::Result;
use futures::SinkExt;
use selium::prelude::*;
use selium::pubsub::DeliveryGuarantee;
use selium::std::codecs::StringCodec;
use std::time::Duration;
use tokio_retry::strategy::{jitter, ExponentialBackoff, FixedInterval};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

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
        .with_delivery_guarantee(DeliveryGuarantee::AtMostOnce)
        .open()
        .await?;

    publisher.send("Hello, world!".to_owned()).await.unwrap();
    publisher.finish().await?;

    Ok(())
}
