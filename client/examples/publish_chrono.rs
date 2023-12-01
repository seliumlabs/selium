use anyhow::Result;
use futures::SinkExt;
use selium::prelude::*;
use selium::std::codecs::StringCodec;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::custom()
        .keep_alive(chrono::Duration::seconds(5))?
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

    publisher.send("Hello, world!".to_owned()).await?;
    publisher.finish().await?;

    Ok(())
}
