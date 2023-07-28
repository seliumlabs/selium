use anyhow::Result;
use futures::SinkExt;
use selium::codecs::StringCodec;
use selium::prelude::*;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    let mut publisher = connection
        .publisher("/acmeco/stocks")
        .map("/acmeco/forge_numbers.wasm")
        .retain(Duration::from_secs(600))?
        .with_encoder(StringCodec)
        .open()
        .await?;

    publisher.send("Hello, world!").await?;
    publisher.finish().await?;

    Ok(())
}
