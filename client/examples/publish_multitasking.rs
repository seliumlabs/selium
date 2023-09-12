use anyhow::Result;
use futures::SinkExt;
use selium::codecs::StringCodec;
use selium::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    let mut publisher = connection
        .publisher("/acmeco/stocks")
        .with_encoder(StringCodec)
        .open()
        .await?;

    tokio::spawn({
        let mut publisher = publisher.duplicate().await.unwrap();
        async move {
            publisher
                .send("Hello from spawned task!".to_owned())
                .await
                .unwrap();
            publisher.finish().await.unwrap();
        }
    });

    publisher.send("Hello from main!".to_owned()).await?;
    publisher.finish().await?;

    Ok(())
}
