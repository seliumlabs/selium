use anyhow::Result;
use futures::SinkExt;
use selium::prelude::*;
use selium::std::codecs::BincodeCodec;
use selium_std::{
    compression::deflate::{DeflateComp, DeflateDecomp},
    traits::compression::CompressionLevel,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
enum Request {
    HelloWorld(Option<String>),
}

#[derive(Debug, Serialize)]
enum Response {
    HelloWorld(String),
}

#[tokio::main]
async fn main() -> Result<()> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("../certs/client/ca.der")?
        .with_cert_and_key(
            "../certs/client/localhost.der",
            "../certs/client/localhost.key.der",
        )?
        .connect("127.0.0.1:7001")
        .await?;

    let client = connection
        .replier("/some/endpoint")
        .with_request_decoder(BincodeCodec::default())
        .with_request_decompression(DeflateDecomp::gzip())
        .with_reply_encoder(BincodeCodec::default())
        .with_reply_compression(DeflateComp::gzip().fastest())
        .with_handler(handler)
        .open()
        .await?;

    Ok(())
}

async fn handler(req: Request) -> Response {
    match req {
        Request::HelloWorld(mut name) => {
            let name = name.take().unwrap_or_else(|| "World".to_owned());
            let response = format!("{name}!");
            Response::HelloWorld(response)
        }
    }
}
