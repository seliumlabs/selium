use std::time::Duration;

use anyhow::Result;
use futures::future::join_all;
use futures::SinkExt;
use selium::prelude::*;
use selium::std::codecs::StringCodec;

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("../certs/client/ca.der")?
        .with_cert_and_key(
            "../certs/client/localhost.der",
            "../certs/client/localhost.key.der",
        )?
        .connect("127.0.0.1:7001")
        .await?;

    let mut tasks = vec![];

    for _ in 0..1 {
        let mut publisher = connection
            .publisher("/acmeco/stocks")
            .with_encoder(StringCodec)
            .open()
            .await?;

        let task = tokio::spawn(async move {
            for i in 0..100 {
                let message = format!("Hello - {i}");
                publisher.send(message).await.unwrap();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });

        tasks.push(task);
    }

    join_all(tasks).await;

    Ok(())
}
