use anyhow::Result;
use futures::SinkExt;
use selium::prelude::*;
use selium::std::codecs::StringCodec;
use selium::std::compression::deflate::DeflateComp;
use selium::std::traits::compression::CompressionLevel;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("../certs/client/ca.der")?
        .with_cert_and_key("../certs/client/localhost.der", "../certs/client/localhost.key.der")?
        .connect("127.0.0.1:7001")
        .await?;

    let mut publisher = connection
        .publisher("/acmeco/stocks")
        .with_encoder(StringCodec)
        .with_compression(DeflateComp::gzip().fastest())
        .open()
        .await?;

    publisher.send("Hello, world!".to_owned()).await?;
    publisher.finish().await?;

    Ok(())
}
