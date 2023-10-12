use anyhow::Result;
use futures::StreamExt;
use selium::prelude::*;
use selium::std::codecs::BincodeCodec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct StockEvent {
    ticker: String,
    change: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca/first/ca.crt")?
        .with_cert_and_key("certs/client/first/client.crt", "certs/client/first/client.key")?
        .connect("127.0.0.1:7001")
        .await?;

    let mut subscriber = connection
        .subscriber("/acmeco/stocks")
        .with_decoder(BincodeCodec::<StockEvent>::default())
        .open()
        .await?;

    while let Some(Ok(event)) = subscriber.next().await {
        println!("NEW STOCK EVENT: {event:#?}");
    }

    Ok(())
}
