use anyhow::Result;
use selium::prelude::*;
use selium::pubsub::DeliveryGuarantee;
use selium::std::codecs::StringCodec;
use std::time::Duration;
use tokio_retry::strategy::FixedInterval;

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

    let retry_strategy: Vec<Duration> = FixedInterval::from_millis(1000).take(2).collect();

    let mut publisher = connection
        .publisher("/acmeco/stocks")
        .with_encoder(StringCodec)
        .with_delivery_guarantee(DeliveryGuarantee::AtLeastOnce(retry_strategy, 1000))
        .open()
        .await?;

    let res1 = publisher.send("Hello, world 1".to_owned()).await;
    println!("res1: {:?}", res1);

    let res2 = publisher.send("Hello, world 2".to_owned()).await;
    println!("res2: {:?}", res2);

    let res3 = publisher.send("Hello, world 3".to_owned()).await;
    println!("res3: {:?}", res3);

    publisher.finish().await?;

    Ok(())
}
