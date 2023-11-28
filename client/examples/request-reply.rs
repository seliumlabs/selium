use anyhow::Result;
use selium::prelude::*;
use selium::std::codecs::BincodeCodec;
use selium::std::errors::SeliumError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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

    tokio::spawn({
        let connection = connection.clone();
        async move {
            let replier = connection
                .replier("/some/endpoint")
                .with_request_decoder(BincodeCodec::default())
                .with_reply_encoder(BincodeCodec::default())
                .with_handler(handler)
                .open()
                .await
                .unwrap();

            replier.listen().await.unwrap();
        }
    });

    let mut requestor = connection
        .requestor("/some/endpoint")
        .with_request_encoder(BincodeCodec::default())
        .with_reply_decoder(BincodeCodec::<Response>::default())
        .open()
        .await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let res = requestor.request(Request::HelloWorld(None)).await?;
    assert_eq!(res, Response::HelloWorld("Hello, World!".to_owned()));

    let res = requestor
        .request(Request::HelloWorld(Some("Bobby".to_owned())))
        .await?;

    assert_eq!(res, Response::HelloWorld("Hello, Bobby!".to_owned()));

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
