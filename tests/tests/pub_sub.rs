use std::{
    error::Error,
    process::{Child, Command},
};

use futures::{stream::iter, SinkExt, TryStreamExt};
use selium::{prelude::*, Subscriber};

const SERVER_ADDR: &'static str = "127.0.0.1:7001";

#[tokio::test]
async fn test_pub_sub() {
    let mut handle = start_server();

    let result = run().await;

    handle.kill().unwrap();

    let messages = result.unwrap();
    assert_eq!(messages[0], "foo");
    assert_eq!(messages[1], "bar");
}

async fn run() -> Result<[String; 2], Box<dyn Error>> {
    let mut subscriber1 = start_subscriber("/acmeco/stocks").await?;
    // let subscriber2 = start_subscriber("/acmeco/stocks").await?;
    // let subscriber3 = start_subscriber("/acmeco/something_else").await?;
    // let subscriber4 = start_subscriber("/bluthco/stocks").await?;

    let mut publisher = selium::publisher("/acmeco/stocks")
        // .map("/acmeco/forge_numbers.wasm")
        .keep_alive(5_000)
        .with_certificate_authority("certs/ca.crt")?
        .connect("127.0.0.1:7001")
        .await?;

    publisher
        .send_all(&mut iter(vec![Ok("foo"), Ok("bar")]))
        .await?;
    publisher.finish().await?;

    let message1: String = subscriber1.try_next().await?.unwrap_or("".to_owned());
    let message2: String = subscriber1.try_next().await?.unwrap_or("".to_owned());
    subscriber1.finish().await?;

    Ok([message1, message2])
}

async fn start_subscriber(topic: &str) -> Result<Subscriber, Box<dyn Error>> {
    Ok(selium::subscriber(topic)
        // .map("/selium/bonanza.wasm")
        // .filter("/selium/dodgy_stuff.wasm")
        .keep_alive(5_000)
        .with_certificate_authority("certs/ca.crt")?
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
