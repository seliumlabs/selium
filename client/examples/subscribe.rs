use anyhow::Result;
use selium::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let mut subscriber = selium::subscriber("/acmeco/stocks")
        .map("/selium/bonanza.wasm")
        .filter("/selium/dodgy_stuff.wasm")
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    subscriber.subscribe().await?;

    Ok(())
}
