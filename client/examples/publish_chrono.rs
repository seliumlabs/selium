use anyhow::Result;
use futures::SinkExt;
use selium::prelude::*;
use selium::std::codecs::StringCodec;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(chrono::Duration::seconds(5))?
        .with_certificate_authority("certs/ca.crt")?
        .with_cert_and_key("certs/client.crt", "certs/client.key")?
        .connect("127.0.0.1:7001")
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
