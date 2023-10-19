use bytes::BytesMut;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use selium_std::{
    codecs::{BincodeCodec, StringCodec},
    traits::codec::{MessageDecoder, MessageEncoder},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct StockEvent {
    ticker: String,
    change: f64,
}

impl StockEvent {
    pub fn new(ticker: &str, change: f64) -> Self {
        Self {
            ticker: ticker.to_owned(),
            change,
        }
    }
}

pub fn bincode_codec_benchmarks(c: &mut Criterion) {
    c.bench_function("bincode codec", |b| {
        b.iter(|| {
            let message = StockEvent::new("APPL", 25.5);
            let codec = BincodeCodec::default();
            let encoded = codec.encode(black_box(message)).unwrap();
            let mut encoded = BytesMut::from(&encoded[..]);
            let _ = codec.decode(black_box(&mut encoded)).unwrap();
        })
    });
}

pub fn string_codec_benchmarks(c: &mut Criterion) {
    c.bench_function("string codec", |b| {
        b.iter(|| {
            let message = "Hello, world!".to_owned();
            let codec = StringCodec::default();
            let encoded = codec.encode(black_box(message)).unwrap();
            let mut encoded = BytesMut::from(&encoded[..]);
            let _ = codec.decode(black_box(&mut encoded)).unwrap();
        })
    });
}

criterion_group!(benches, bincode_codec_benchmarks, string_codec_benchmarks,);
criterion_main!(benches);
