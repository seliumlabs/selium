#![cfg(not(feature = "loom"))]

use std::pin::pin;

use futures::join;
use selium_messaging::{Channel, ChannelError, Writer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn test_readwrite() {
    let channel = Channel::new(4);
    let writer1 = channel.new_writer();
    let writer2 = channel.new_writer();
    let mut strong_r = pin!(channel.new_strong_reader());
    let mut weak_r = pin!(channel.new_weak_reader());

    let mut buf = [0; 120000];
    let (_, _, r) = join!(
        write(writer1, &[1, 1, 1]),
        write(writer2, &[2, 2, 2]),
        strong_r.read_exact(&mut buf)
    );
    r.unwrap();

    for line in buf.chunks(3) {
        if line != [1, 1, 1] && line != [2, 2, 2] {
            panic!("Writes out of order: {line:?}");
        }
    }

    let mut buf = Vec::new();
    let err: ChannelError = weak_r.read(&mut buf).await.unwrap_err().downcast().unwrap();
    assert_eq!(err, ChannelError::ReaderBehind(119996));
}

async fn write(mut writer: Writer, buf: &[u8]) {
    for _ in 0..20000 {
        writer.write_all(buf).await.unwrap();
    }
}
