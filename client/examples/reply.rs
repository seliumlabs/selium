use anyhow::Result;
use selium::{prelude::*, keep_alive::BackoffStrategy};
use selium::std::codecs::BincodeCodec;
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
    let connection = selium::custom()
        .keep_alive(5_000)?
        .backoff_strategy(
            BackoffStrategy::constant()
                .with_max_attempts(1)
        )
        .endpoint("127.0.0.1:7001")
        .with_certificate_authority("../certs/client/ca.der")?
        .with_cert_and_key(
            "../certs/client/localhost.der",
            "../certs/client/localhost.key.der",
        )?
        .connect()
        .await?;

    let mut replier = connection
        .replier("/some/endpoint")
        .with_request_decoder(BincodeCodec::default())
        .with_reply_encoder(BincodeCodec::default())
        .with_handler(|req| async move { handler(req).await })
        .open()
        .await?;

    replier.listen().await?;

    Ok(())
}

async fn handler(req: Request) -> Result<Response> {
    match req {
        Request::HelloWorld(mut name) => {
            let name = name.take().unwrap_or_else(|| "World".to_owned());
            let response = format!("Hello, {name}!");
            Ok(Response::HelloWorld(response))
        }
    }
}
