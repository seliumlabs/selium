use anyhow::Result;
use futures::StreamExt;
use selium::codecs::StringCodec;
use selium::prelude::*;
use selium::std::compression::deflate::DeflateDecomp;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
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
