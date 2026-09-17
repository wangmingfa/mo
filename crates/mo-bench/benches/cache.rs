//! 元数据缓存的性能基准。
//!
//! 重点是**批量写入 vs 逐条写入**的差距：这是打开大目录时最容易踩的坑——
//! 一万条目逐条插入要几秒，合并成事务后降到几十毫秒。

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use criterion::{criterion_group, criterion_main, Criterion};
use mo_cache::MetadataCache;
use mo_core::{FileId, FileMetadata, Permissions};

fn temp_db(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("mo-bench-cache");
    std::fs::create_dir_all(&dir).expect("创建基准目录失败");
    dir.join(format!("{tag}.sqlite"))
}

fn make_items(n: usize) -> Vec<(FileId, FileMetadata)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (0..n)
        .map(|i| {
            (
                FileId::new(1, i as u128),
                FileMetadata {
                    size: (i * 1024) as u64,
                    modified: Some(UNIX_EPOCH + std::time::Duration::from_secs(now + i as u64)),
                    created: Some(UNIX_EPOCH + std::time::Duration::from_secs(now)),
                    permissions: Permissions {
                        readonly: false,
                        hidden: false,
                    },
                },
            )
        })
        .collect()
}

fn bench_put(c: &mut Criterion) {
    let cache = MetadataCache::open(&temp_db("put")).expect("打开缓存失败");
    let items = make_items(10_000);

    let mut group = c.benchmark_group("cache_write_10k");
    group.bench_function("put_many_batched", |b| {
        b.iter(|| cache.put_many(&items).expect("写入失败"));
    });
    // 逐条提交每条都是一次事务，慢到必须单独降采样，否则基准跑不完。
    group.sample_size(10);
    group.bench_function("put_one_by_one", |b| {
        b.iter(|| {
            for (id, m) in items.iter().take(1_000) {
                cache.put(id, m).expect("写入失败");
            }
        });
    });
    group.finish();
}

fn bench_get(c: &mut Criterion) {
    let cache = MetadataCache::open(&temp_db("get")).expect("打开缓存失败");
    let items = make_items(10_000);
    cache.put_many(&items).expect("预填充失败");
    let ids: Vec<FileId> = items.iter().map(|(id, _)| *id).collect();

    c.bench_function("cache_get_many_10k", |b| {
        b.iter(|| cache.get_many(&ids).expect("读取失败"));
    });
}

criterion_group!(benches, bench_put, bench_get);
criterion_main!(benches);
