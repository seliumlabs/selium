use std::{
    error::Error,
    process::{Child, Command},
};

use futures::{stream::iter, FutureExt, SinkExt, StreamExt, TryStreamExt};
use selium::std::codecs::StringCodec;
use selium::{prelude::*, Subscriber};

const SERVER_ADDR: &'static str = "127.0.0.1:7001";

#[tokio::test]
async fn test_pub_sub() {
    let mut handle = start_server();

    let result = run().await;

    handle.kill().unwrap();

    let messages = result.unwrap();
    assert_eq!(messages[0], Some("foo".to_owned()));
    assert_eq!(messages[1], Some("bar".to_owned()));
    assert_eq!(messages[2], Some("foo".to_owned()));
    assert_eq!(messages[3], Some("bar".to_owned()));
    assert_eq!(messages[4], Some("foo".to_owned()));
    assert_eq!(messages[5], Some("bar".to_owned()));
    assert_eq!(messages[6], Some("foo".to_owned()));
    assert_eq!(messages[7], Some("foo".to_owned()));
    assert_eq!(messages[8], Some("bar".to_owned()));
    assert_eq!(messages[9], Some("foo".to_owned()));
    assert_eq!(messages[10], Some("bar".to_owned()));
    assert_eq!(messages[11], Some("foo".to_owned()));
    assert_eq!(messages[12], Some("bar".to_owned()));
    assert_eq!(messages[13], Some("foo".to_owned()));
    assert!(messages[14].is_none());
    assert!(messages[15].is_none());
}

async fn run() -> Result<[Option<String>; 16], Box<dyn Error>> {
    let mut subscriber1 = start_subscriber("/acmeco/stocks").await?;
    let mut subscriber2 = start_subscriber("/acmeco/stocks").await?;
    let subscriber3 = start_subscriber("/acmeco/something_else").await?;
    let subscriber4 = start_subscriber("/bluthco/stocks").await?;

    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca/first/ca.crt")?
        .with_cert_and_key("certs/client/first/client.crt", "certs/client/first/client.key")?
        .connect(SERVER_ADDR)
        .await?;

    let mut publisher = connection
        .publisher("/acmeco/stocks")
        // .map("/acmeco/forge_numbers.wasm")
        .with_encoder(StringCodec)
        .open()
        .await?;

    publisher
        .send_all(&mut iter(vec![
            Ok("foo".to_owned()),
            Ok("bar".to_owned()),
            Ok("foo".to_owned()),
            Ok("bar".to_owned()),
            Ok("foo".to_owned()),
            Ok("bar".to_owned()),
            Ok("foo".to_owned()),
        ]))
        .await?;

    publisher.finish().await?;

    let message1 = subscriber1.try_next().await?;
    let message2 = subscriber1.try_next().await?;
    let message3 = subscriber1.try_next().await?;
    let message4 = subscriber1.try_next().await?;
    let message5 = subscriber1.try_next().await?;
    let message6 = subscriber1.try_next().await?;
    let message7 = subscriber1.try_next().await?;
    let message8 = subscriber2.try_next().await?;
    let message9 = subscriber2.try_next().await?;
    let message10 = subscriber2.try_next().await?;
    let message11 = subscriber2.try_next().await?;
    let message12 = subscriber2.try_next().await?;
    let message13 = subscriber2.try_next().await?;
    let message14 = subscriber2.try_next().await?;
    let message15 = subscriber3
        .into_future()
        .map(|_| String::new())
        .now_or_never();
    let message16 = subscriber4
        .into_future()
        .map(|_| String::new())
        .now_or_never();

    Ok([
        message1, message2, message3, message4, message5, message6, message7, message8, message9,
        message10, message11, message12, message13, message14, message15, message16,
    ])
}

async fn start_subscriber(topic: &str) -> Result<Subscriber<StringCodec, String>, Box<dyn Error>> {
    let connection = selium::client()
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca/first/ca.crt")?
        .with_cert_and_key("certs/client/first/client.crt", "certs/client/first/client.key")?
        .connect(SERVER_ADDR)
        .await?;

    Ok(connection
        .subscriber(topic)
        // .map("/selium/bonanza.wasm")
        // .filter("/selium/dodgy_stuff.wasm")
        .with_decoder(StringCodec)
        .open()
        .await?)
}

fn start_server() -> Child {
    Command::new(env!("CARGO"))
        .args([
            "run",
            "--bin",
            "selium-server",
            "--",
            "--bind-addr",
            SERVER_ADDR,
            "--cert",
            "tests/certs/ca.crt",
            "--key",
            "tests/certs/ca.key",
            "-vvvv",
        ])
        .current_dir("..")
        .spawn()
        .expect("Failed to start server")
}
