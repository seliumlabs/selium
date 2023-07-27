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
    let connection = selium::client()
        .keep_alive(Duration::from_secs(5))?
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    let mut publisher = connection
        .publisher("/acmeco/stocks")
        .map("/acmeco/forge_numbers.wasm")
        .retain(Duration::from_secs(600))?
        .with_encoder(BincodeCodec::default())
        .open()
        .await?;

    tokio::spawn({
        let mut publisher = publisher.clone().await.unwrap();
        async move {
            publisher
                .send(StockEvent::new("MSFT", 12.75))
                .await
                .unwrap();
            publisher.finish().await.unwrap();
        }
    });

    publisher.send(StockEvent::new("APPL", 3.5)).await?;
    publisher.send(StockEvent::new("INTC", -9.0)).await?;
    publisher.finish().await?;

    Ok(())
}
