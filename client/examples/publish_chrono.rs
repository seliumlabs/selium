use anyhow::Result;
use futures::SinkExt;
use selium::codecs::StringCodec;
use selium::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(chrono::Duration::seconds(5))?
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    let mut publisher = connection
        .publisher("/acmeco/stocks")
        .with_encoder(StringCodec)
        // Coming soon...
        // .map("/acmeco/forge_numbers.wasm")
        // .retain(chrono::Duration::minutes(25))?
        .open()
        .await?;

    publisher.send("Hello, world!".to_owned()).await?;
    publisher.finish().await?;

    Ok(())
}
