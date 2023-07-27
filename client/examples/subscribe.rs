use anyhow::Result;
use futures::StreamExt;
use selium::codecs::BincodeCodec;
use selium::prelude::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
struct StockEvent {
    ticker: String,
    change: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut subscriber = selium::subscriber("/acmeco/stocks")
        .map("/selium/bonanza.wasm")
        .filter("/selium/dodgy_stuff.wasm")
        .retain(Duration::from_secs(600))?
        .with_certificate_authority("certs/ca.crt")?
        .with_decoder(BincodeCodec::default())
        .connect("127.0.0.1:7001")
        .await?;

    while let Some(Ok(StockEvent { ticker, change })) = subscriber.next().await {
        println!("NEW STOCK EVENT: \"{ticker} - {change}\"");
    }

    subscriber.finish().await?;

    Ok(())
}
