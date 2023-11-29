use std::time::Duration;
use anyhow::Result;
use fake::faker::name::en::Name;
use fake::Fake;
use futures::future::try_join_all;
use selium::prelude::*;
use selium::std::codecs::BincodeCodec;
use serde::{Deserialize, Serialize};

const NUM_OF_REQUESTS: usize = 10;

#[derive(Debug, Serialize, Deserialize, Clone)]
enum Request {
    HelloWorld(Option<String>),
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
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

    let requestor = connection
        .requestor("/some/endpoint")
        .with_request_encoder(BincodeCodec::default())
        .with_reply_decoder(BincodeCodec::<Response>::default())
        .with_request_timeout(Duration::from_secs(3))?
        .open()
        .await?;

    let mut tasks = vec![];

    for _ in 0..NUM_OF_REQUESTS {
        let task = tokio::spawn({
            let name: String = Name().fake();
            let request: Request = Request::HelloWorld(Some(name.clone()));
            let expected = format!("Hello, {name}!");
            let mut requestor = requestor.clone();

            async move {
                let res = requestor.request(request).await.unwrap();
                assert_eq!(res, Response::HelloWorld(expected));
                println!("Response: {res:?}");
            }
        });

        tasks.push(task);
    }

    try_join_all(tasks).await?;

    Ok(())
}
