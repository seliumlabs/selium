use crate::helpers::start_server;
use anyhow::Result;
use futures::{stream::iter, FutureExt, SinkExt, StreamExt, TryStreamExt};
use selium::keep_alive::pubsub::KeepAlive;
use selium::std::codecs::StringCodec;
use selium::{prelude::*, pubsub::Subscriber};
use tempfile::TempDir;

#[tokio::test]
async fn test_pub_sub() -> Result<()> {
    // TODO: These tests need to be reworked for message retention.
    let messages = run().await?;

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

    Ok(())
}

async fn run() -> Result<[Option<String>; 16]> {
    let tempdir = TempDir::new().unwrap();
    let addr = start_server(tempdir.path())?;
    let addr = addr.to_string();

    let mut subscriber1 = start_subscriber(&addr, "/acmeco/stocks").await?;
    let mut subscriber2 = start_subscriber(&addr, "/acmeco/stocks").await?;
    let subscriber3 = start_subscriber(&addr, "/acmeco/something_else").await?;
    let subscriber4 = start_subscriber(&addr, "/bluthco/stocks").await?;

    let connection = selium::custom()
        .keep_alive(5_000)?
        .endpoint(&addr)
        .with_certificate_authority("../certs/client/ca.der")?
        .with_cert_and_key(
            "../certs/client/localhost.der",
            "../certs/client/localhost.key.der",
        )?
        .connect()
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

async fn start_subscriber(addr: &str, topic: &str) -> Result<KeepAlive<Subscriber<StringCodec>>> {
    let connection = selium::custom()
        .keep_alive(5_000)?
        .endpoint(addr)
        .with_certificate_authority("../certs/client/ca.der")?
        .with_cert_and_key(
            "../certs/client/localhost.der",
            "../certs/client/localhost.key.der",
        )?
        .connect()
        .await?;

    Ok(connection
        .subscriber(topic)
        // .map("/selium/bonanza.wasm")
        // .filter("/selium/dodgy_stuff.wasm")
        .with_decoder(StringCodec)
        .open()
        .await?)
}
