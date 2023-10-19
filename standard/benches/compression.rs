use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use selium_std::compression::{brotli, deflate, lz4, zstd};
use selium_std::traits::compression::{Compress, CompressionLevel, Decompress};

pub fn deflate_benchmarks(c: &mut Criterion) {
    c.bench_function("DEFLATE | gzip | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));

            let compressed = deflate::DeflateComp::gzip()
                .fastest()
                .compress(payload)
                .unwrap();

            deflate::DeflateDecomp::gzip()
                .decompress(black_box(compressed))
                .unwrap();
        })
    });

    c.bench_function("DEFLATE | zlib | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));

            let compressed = deflate::DeflateComp::zlib()
                .fastest()
                .compress(payload)
                .unwrap();

            deflate::DeflateDecomp::zlib()
                .decompress(black_box(compressed))
                .unwrap();
        })
    });
}

pub fn zstd_benchmarks(c: &mut Criterion) {
    c.bench_function("zstd | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));
            let compressed = zstd::ZstdComp::new().fastest().compress(payload).unwrap();

            zstd::ZstdDecomp.decompress(black_box(compressed)).unwrap();
        })
    });
}

pub fn brotli_benchmarks(c: &mut Criterion) {
    c.bench_function("brotli | text | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));

            let compressed = brotli::BrotliComp::text()
                .fastest()
                .compress(payload)
                .unwrap();

            brotli::BrotliDecomp
                .decompress(black_box(compressed))
                .unwrap();
        })
    });

    c.bench_function("brotli | generic | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));

            let compressed = brotli::BrotliComp::generic()
                .fastest()
                .compress(payload)
                .unwrap();

            brotli::BrotliDecomp
                .decompress(black_box(compressed))
                .unwrap();
        })
    });

    c.bench_function("brotli | font | fastest", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));

            let compressed = brotli::BrotliComp::font()
                .fastest()
                .compress(payload)
                .unwrap();

            brotli::BrotliDecomp
                .decompress(black_box(compressed))
                .unwrap();
        })
    });
}

pub fn lz4_benchmarks(c: &mut Criterion) {
    c.bench_function("lz4", |b| {
        b.iter(|| {
            let payload = black_box(Bytes::from(black_box("hello, world!")));
            let compressed = lz4::Lz4Comp.compress(payload).unwrap();

            lz4::Lz4Decomp.decompress(black_box(compressed)).unwrap();
        })
    });
}

criterion_group!(
    benches,
    deflate_benchmarks,
    zstd_benchmarks,
    brotli_benchmarks,
    lz4_benchmarks
);

criterion_main!(benches);
