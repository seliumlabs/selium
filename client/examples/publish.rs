use std::time::Duration;

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
    let publisher = selium::publisher("/acmeco/stocks")
        .map("/acmeco/forge_numbers.wasm")
        .keep_alive(Duration::from_secs(5))?
        .retain(Duration::from_secs(600))?
        .with_certificate_authority("certs/ca.crt")?
        .with_encoder(BincodeCodec::default())
        .connect("127.0.0.1:7001")
        .await?;

    tokio::spawn({
        let mut stream = publisher.stream().await.unwrap();
        async move {
            stream.send(StockEvent::new("MSFT", 12.75)).await.unwrap();
            stream.finish().await.unwrap();
        }
    });

    let mut stream = publisher.stream().await?;

    stream.send(StockEvent::new("APPL", 3.5)).await?;
    stream.send(StockEvent::new("INTC", -9.0)).await?;
    stream.finish().await?;

    Ok(())
}
