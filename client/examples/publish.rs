use anyhow::Result;
use selium::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let mut publisher = selium::publisher("/acmeco/stocks")
        .map("/acmeco/forge_numbers.wasm")
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    publisher.publish("{{\"APL\":\"+53.5\"}}").await?;

    Ok(())
}
