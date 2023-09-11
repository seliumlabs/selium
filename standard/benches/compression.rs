use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use selium_std::compression::{deflate, zstd};
use selium_traits::compression::{Compress, Decompress};

pub fn deflate_benchmarks(c: &mut Criterion) {
    c.bench_function("DEFLATE | gzip | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));
            let compressed = deflate::comp::gzip().fastest().compress(payload).unwrap();

            deflate::decomp::gzip()
                .decompress(black_box(compressed))
                .unwrap();
        })
    });

    c.bench_function("DEFLATE | zlib | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));
            let compressed = deflate::comp::zlib().fastest().compress(payload).unwrap();

            deflate::decomp::zlib()
                .decompress(black_box(compressed))
                .unwrap();
        })
    });
}

pub fn zstd_benchmarks(c: &mut Criterion) {
    c.bench_function("Zstd | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));
            let compressed = zstd::comp::fastest().compress(payload).unwrap();

            zstd::decomp::new()
                .decompress(black_box(compressed))
                .unwrap();
        })
    });
}

criterion_group!(benches, deflate_benchmarks, zstd_benchmarks);
criterion_main!(benches);
