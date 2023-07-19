use anyhow::Result;
use futures::StreamExt;
use selium::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let mut subscriber = selium::subscriber("/acmeco/stocks")
        .map("/selium/bonanza.wasm")
        .filter("/selium/dodgy_stuff.wasm")
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    while let Some(Ok(message)) = subscriber.next().await {
        println!("{message}");
    }

    subscriber.finish().await?;

    Ok(())
}
