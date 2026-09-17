//! 缩略图生成的性能基准：冷生成 vs 命中磁盘缓存。
//!
//! 冷生成是 CPU 密集（解码 + 缩放），命中缓存只是一次 `stat`——
//! 两者的差距决定了「为什么缩略图必须先落盘缓存、且只为可见区生成」。

use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion};
use mo_core::FileId;
use mo_thumbnails::ThumbnailCache;

/// 造一张 1024×1024 的测试图。
fn make_source() -> PathBuf {
    let dir = std::env::temp_dir().join("mo-bench-thumb");
    std::fs::create_dir_all(&dir).expect("创建基准目录失败");
    let path = dir.join("source.png");
    if path.exists() {
        return path;
    }
    let img = image::RgbImage::from_fn(1024, 1024, |x, y| {
        image::Rgb([(x % 255) as u8, (y % 255) as u8, 128])
    });
    img.save(&path).expect("写入测试图失败");
    path
}

fn bench_thumbnail(c: &mut Criterion) {
    let src = make_source();
    let id = FileId::new(1, 42);

    let cold_root = std::env::temp_dir().join("mo-bench-thumb/cache-cold");
    let cold = ThumbnailCache::with_root(cold_root);
    c.bench_function("thumbnail_cold_1024px", |b| {
        b.iter(|| {
            // 每次都先清空，强制走完整解码路径。
            cold.clear().expect("清空缓存失败");
            cold.get_or_create(&id, &src, 128).expect("生成失败")
        });
    });

    let warm_root = std::env::temp_dir().join("mo-bench-thumb/cache-warm");
    let warm = ThumbnailCache::with_root(warm_root);
    warm.get_or_create(&id, &src, 128).expect("预生成失败");
    c.bench_function("thumbnail_cached", |b| {
        b.iter(|| warm.get_or_create(&id, &src, 128).expect("读取失败"));
    });
}

criterion_group!(benches, bench_thumbnail);
criterion_main!(benches);
