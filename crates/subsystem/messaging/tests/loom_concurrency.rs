#![cfg(feature = "loom")]

use std::sync::Arc;

use loom::{future::block_on, thread};
use selium_messaging::{Backpressure, Channel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn concurrent_write_read() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(|| {
        let channel = Arc::new(Channel::new(2));
        let mut writer1 = channel.clone().new_writer();
        let mut writer2 = channel.clone().new_writer();
        let reader = channel.new_strong_reader();

        let t1 = thread::spawn(move || {
            block_on(async move { writer1.write_all(&[1]).await.unwrap() });
        });
        let t2 = thread::spawn(move || {
            block_on(async move { writer2.write_all(&[2]).await.unwrap() });
        });

        t1.join().unwrap();
        t2.join().unwrap();

        block_on(async move {
            let mut buf = [0u8; 2];
            let mut r = reader;
            r.read_exact(&mut buf).await.unwrap();
            assert!(matches!(&buf, [1, 2] | [2, 1]));
        });
    });
}

#[test]
fn drop_backpressure_with_concurrent_writers() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(|| {
        let channel = Arc::new(Channel::with_parameters(1, Backpressure::Drop));
        let mut reader = channel.new_strong_reader();
        let mut w1 = channel.new_writer();
        let mut w2 = channel.new_writer();

        let t1 = thread::spawn(move || {
            block_on(async move {
                let _ = w1.write_all(&[1]).await;
            });
        });
        let t2 = thread::spawn(move || {
            block_on(async move {
                let _ = w2.write_all(&[2]).await;
            });
        });

        t1.join().unwrap();
        t2.join().unwrap();

        block_on(async move {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf).await.unwrap();
            assert!(buf[0] == 1 || buf[0] == 2);
        });
    });
}
