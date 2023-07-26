use std::{
    error::Error,
    process::{Child, Command},
};

use futures::{stream::iter, FutureExt, SinkExt, StreamExt, TryStreamExt};
use selium::{prelude::*, Subscriber, codecs::StringCodec};

const SERVER_ADDR: &'static str = "127.0.0.1:7001";

#[tokio::test]
async fn test_pub_sub() {
    let mut handle = start_server();

    let result = run().await;

    handle.kill().unwrap();

    let messages = result.unwrap();
    // @TODO - Message ordering isn't yet guaranteed. This is a workaround.
    assert!(messages[0..=1].contains(&Some("foo".to_owned())));
    assert!(messages[0..=1].contains(&Some("bar".to_owned())));
    assert!(messages[2..=3].contains(&Some("foo".to_owned())));
    assert!(messages[2..=3].contains(&Some("bar".to_owned())));
    assert!(messages[4].is_none());
    assert!(messages[5].is_none());
}

async fn run() -> Result<[Option<String>; 6], Box<dyn Error>> {
    let mut subscriber1 = start_subscriber("/acmeco/stocks").await?;
    let mut subscriber2 = start_subscriber("/acmeco/stocks").await?;
    let subscriber3 = start_subscriber("/acmeco/something_else").await?;
    let subscriber4 = start_subscriber("/bluthco/stocks").await?;

    let mut publisher = selium::publisher("/acmeco/stocks")
        // .map("/acmeco/forge_numbers.wasm")
        .keep_alive(5_000)?
        .with_certificate_authority("certs/ca.crt")?
        .with_encoder(StringCodec)
        .connect("127.0.0.1:7001")
        .await?;

    publisher
        .send_all(&mut iter(vec![Ok("foo"), Ok("bar")]))
        .await?;
    publisher.finish().await?;

    let message1 = subscriber1.try_next().await?;
    let message2 = subscriber1.try_next().await?;
    let message3 = subscriber2.try_next().await?;
    let message4 = subscriber2.try_next().await?;
    let message5 = subscriber3
        .into_future()
        .map(|_| String::new())
        .now_or_never();
    let message6 = subscriber4
        .into_future()
        .map(|_| String::new())
        .now_or_never();
    subscriber1.finish().await?;

    Ok([message1, message2, message3, message4, message5, message6])
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
