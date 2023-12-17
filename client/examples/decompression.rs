use anyhow::Result;
use futures::StreamExt;
use selium::prelude::*;
use selium::std::codecs::StringCodec;
use selium::std::compression::deflate::DeflateDecomp;

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

    let mut subscriber = connection
        .subscriber("/acmeco/stocks")
        .with_decoder(StringCodec)
        .with_decompression(DeflateDecomp::gzip())
        .open()
        .await?;

    while let Some(Ok(message)) = subscriber.next().await {
        println!("NEW MESSAGE: \"{message}\"");
    }

    Ok(())
}
