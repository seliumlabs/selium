use anyhow::Result;
use futures::SinkExt;
use selium::codecs::BincodeCodec;
use selium::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct StockEvent {
    ticker: String,
    change: f64,
}

impl StockEvent {
    pub fn new(ticker: &str, change: f64) -> Self {
        Self {
            ticker: ticker.to_owned(),
            change,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut publisher = selium::publisher("/acmeco/stocks")
        .map("/acmeco/forge_numbers.wasm")
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .with_encoder(BincodeCodec::default())
        .connect("127.0.0.1:7001")
        .await?;

    publisher.send(StockEvent::new("APPL", 3.5)).await?;
    publisher.send(StockEvent::new("INTC", -9.0)).await?;
    publisher.finish().await?;

    Ok(())
}
