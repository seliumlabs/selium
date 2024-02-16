use anyhow::Result;
use futures::StreamExt;
use selium::prelude::*;
use selium::std::codecs::StringCodec;
use selium_protocol::Offset;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

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
        .seek(Offset::FromEnd(100))
        .open()
        .await?;

    while let Some(Ok(message)) = subscriber.next().await {
        println!("NEW MESSAGE: \"{message}\"");
    }

    Ok(())
}
