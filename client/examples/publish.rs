use anyhow::Result;
use futures::SinkExt;
use selium::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let mut publisher = selium::publisher("/acmeco/stocks")
        .map("/acmeco/forge_numbers.wasm")
        .keep_alive(5_000)
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    publisher.send("{{\"APL\":\"+53.5\"}}").await?;
    publisher.finish().await?;

    Ok(())
}
