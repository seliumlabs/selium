use std::{
    error::Error,
    process::{Child, Command},
};

use futures::{stream::iter, FutureExt, SinkExt, StreamExt, TryStreamExt};
use selium::{codecs::StringCodec, prelude::*, Subscriber};

const SERVER_ADDR: &'static str = "127.0.0.1:7001";

#[tokio::test]
async fn test_pub_sub() {
    let mut handle = start_server();

    let result = run().await;

    handle.kill().unwrap();

    let messages = result.unwrap();
    // @TODO - Message ordering isn't yet guaranteed. This is a workaround.
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

    let publisher = selium::publisher("/acmeco/stocks")
        // .map("/acmeco/forge_numbers.wasm")
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .with_encoder(StringCodec)
        .connect("127.0.0.1:7001")
        .await?;

    let mut stream = publisher.stream().await?;

    stream
        .send_all(&mut iter(vec![
            Ok("foo"),
            Ok("bar"),
            Ok("foo"),
            Ok("bar"),
            Ok("foo"),
            Ok("bar"),
            Ok("foo"),
        ]))
        .await?;

    stream.finish().await?;

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

    subscriber1.finish().await?;
    subscriber2.finish().await?;

    Ok([
        message1, message2, message3, message4, message5, message6, message7, message8, message9,
        message10, message11, message12, message13, message14, message15, message16,
    ])
}

async fn start_subscriber(topic: &str) -> Result<Subscriber<StringCodec, String>, Box<dyn Error>> {
    Ok(selium::subscriber(topic)
        // .map("/selium/bonanza.wasm")
        // .filter("/selium/dodgy_stuff.wasm")
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .with_decoder(StringCodec)
        .connect(SERVER_ADDR)
        .await?)
}

fn start_server() -> Child {
    Command::new(env!("CARGO"))
        .args([
            "run",
            "--",
            "--bind-addr",
            SERVER_ADDR,
            "--cert",
            "tests/certs/ca.crt",
            "--key",
            "tests/certs/ca.key",
        ])
        .current_dir("..")
        .spawn()
        .expect("Failed to start server")
}
