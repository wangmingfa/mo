//! 目录相关的性能基准：读取、排序、过滤、FileId 生成。
//!
//! 这些数字是「大目录优化」的效果基线——改动 `mo-fs` / `mo-core` 的视图逻辑后，
//! 用 `cargo bench` 对比这里的变化，就能判断是变快还是变慢。

use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use mo_core::{Directory, Entry, EntryKind, FileId, SortKey};
use mo_fs::{FileSystem, LocalFileSystem};

/// 造一个包含 `n` 个文件的临时目录（只创建空文件，避免受磁盘写入速度干扰）。
fn make_dir(n: usize, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-bench-{tag}-{n}"));
    if dir.exists() {
        return dir;
    }
    std::fs::create_dir_all(&dir).expect("创建基准目录失败");
    for i in 0..n {
        let _ = std::fs::File::create(dir.join(format!("file-{i:06}.txt")));
    }
    dir
}

/// 造 `n` 个只存在于内存中的条目（测排序 / 过滤的纯 CPU 成本）。
fn make_entries(n: usize) -> Vec<Entry> {
    (0..n)
        .map(|i| {
            let is_dir = i % 7 == 0;
            Entry::new(
                FileId::new(1, i as u128),
                format!("file-{i:06}.txt"),
                if is_dir {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                PathBuf::from(format!("/tmp/file-{i:06}.txt")),
            )
        })
        .collect()
}

fn bench_read_dir(c: &mut Criterion) {
    let fs = LocalFileSystem;
    let mut group = c.benchmark_group("read_dir");
    for n in [1_000usize, 10_000] {
        let dir = make_dir(n, "readdir");
        group.bench_with_input(BenchmarkId::from_parameter(n), &dir, |b, dir| {
            b.iter(|| fs.read_dir_blocking(dir).expect("读取失败"));
        });
    }
    group.finish();
}

fn bench_sort(c: &mut Criterion) {
    let entries = make_entries(10_000);
    let mut group = c.benchmark_group("view_sort");
    for key in [SortKey::Name, SortKey::Size, SortKey::Modified] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{key:?}")),
            &key,
            |b, key| {
                b.iter(|| {
                    let mut dir = Directory::new(FileId::new(1, 1), PathBuf::from("/tmp"));
                    dir.set_entries(entries.clone());
                    dir.set_sort(*key);
                    dir.visible_count()
                });
            },
        );
    }
    group.finish();
}

fn bench_filter(c: &mut Criterion) {
    let entries = make_entries(10_000);
    c.bench_function("view_filter_10k", |b| {
        b.iter(|| {
            let mut dir = Directory::new(FileId::new(1, 1), PathBuf::from("/tmp"));
            dir.set_entries(entries.clone());
            dir.set_filter(Some("file-0001".to_string()));
            dir.visible_count()
        });
    });
}

fn bench_file_id(c: &mut Criterion) {
    let path = PathBuf::from("/tmp/mo-bench-file-id.txt");
    c.bench_function("file_id_synthetic", |b| {
        b.iter(|| FileId::synthetic(&path));
    });
}

criterion_group!(
    benches,
    bench_read_dir,
    bench_sort,
    bench_filter,
    bench_file_id
);
criterion_main!(benches);
