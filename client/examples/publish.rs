use anyhow::Result;
use futures::SinkExt;
use selium::prelude::*;
use selium::std::codecs::StringCodec;

const NUM_OF_MESSAGES: usize = 50_000;

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
        .open()
        .await?;

    for i in 0..NUM_OF_MESSAGES {
        let message = format!("Hello, world - {i}!");
        publisher.send(message).await.unwrap();
    }

    publisher.finish().await?;

    Ok(())
}
