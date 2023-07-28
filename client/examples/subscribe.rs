use anyhow::Result;
use futures::StreamExt;
use selium::{codecs::StringCodec, prelude::*};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
struct StockEvent {
    ticker: String,
    change: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    let mut subscriber = connection
        .subscriber("/acmeco/stocks")
        .map("/selium/bonanza.wasm")
        .filter("/selium/dodgy_stuff.wasm")
        .retain(Duration::from_secs(600))?
        .with_decoder(StringCodec)
        .open()
        .await?;

    while let Some(Ok(message)) = subscriber.next().await {
        println!("NEW MESSAGE: \"{message}\"");
    }

    Ok(())
}
