use anyhow::Result;
use selium::prelude::*;
use selium::std::codecs::BincodeCodec;
use selium::std::errors::SeliumError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
enum Request {
    HelloWorld(Option<String>),
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
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

    let replier = connection
        .replier("/some/endpoint")
        .with_request_decoder(BincodeCodec::default())
        .with_reply_encoder(BincodeCodec::default())
        .with_handler(handler)
        .open()
        .await
        .unwrap();

    replier.listen().await.unwrap();

    Ok(())
}

async fn handler(req: Request) -> Result<Response, SeliumError> {
    match req {
        Request::HelloWorld(mut name) => {
            let name = name.take().unwrap_or_else(|| "World".to_owned());
            let response = format!("Hello, {name}!");
            Ok(Response::HelloWorld(response))
        }
    }
}
