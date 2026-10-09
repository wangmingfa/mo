//! 全局搜索的性能基准：`FileIndex` 的**写入**与**查找**。
//!
//! 为什么单列一个文件：这条线刚从「`LIKE '%词%'` 全表扫」换成 FTS5 trigram 索引
//! （devlog `windows-port.md` §49），而「快了多少」此前只有一个手测的 ~150ms。
//! 这里的数字是那次优化的效果基线，也是长期回归哨兵——以后谁改坏索引（比如把
//! 查找退回全表扫、或者给写入路径加了逐条事务），`cargo bench --bench search`
//! 立刻见得到。
//!
//! 三档各有各的钉子：
//! * `search_substring`：**中缀**查找（trigram 的主场，也是换索引的全部意义）；
//! * `search_prefix`：前缀查找（`LIKE` 时代唯一快的那一种，换索引后不该变差）；
//! * `search_one_char`：单字符查询——trigram 认不出 3 字符以下的查询，这条走
//!   LIKE 回退，测的就是**回退的代价**（它是全表扫，慢是意料之中，但要知道多慢）。
//! * `index_upsert_batch`：爬取落盘那一步（批量写事务）。

use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use mo_search::{CrawledEntry, FileIndex};

/// 造一个装了 `n` 条的内存索引（名字错开，便于测中缀命中）。
fn make_index(n: usize) -> FileIndex {
    let index = FileIndex::open_in_memory().expect("打开内存索引失败");
    let mut index = index;
    let entries: Vec<CrawledEntry> = (0..n)
        .map(|i| {
            let name = format!("photo-{i:06}-report.txt");
            let path = PathBuf::from(format!("/bench/dir-{:03}/{}", i % 100, name));
            CrawledEntry {
                path,
                name,
                is_dir: false,
            }
        })
        .collect();
    index.upsert_batch(&entries).expect("批量写入失败");
    index
}

fn bench_search(c: &mut Criterion) {
    // 20 万条：接近真实主目录索引的量级（优化前那次手测就是 58 万行库）。
    for n in [20_000usize, 200_000] {
        let index = make_index(n);
        let mut group = c.benchmark_group("search");
        // 采样放宽：单条查询是毫秒级，criterion 默认 100 个样本会把基准跑很久。
        group.sample_size(20);

        group.bench_with_input(BenchmarkId::new("substring", n), &index, |b, index| {
            // 中缀：只出现在名字中间，前缀索引帮不上忙。
            b.iter(|| index.search("o-0001", 50).expect("查找失败"));
        });
        group.bench_with_input(BenchmarkId::new("prefix", n), &index, |b, index| {
            b.iter(|| index.search("photo-000", 50).expect("查找失败"));
        });
        group.bench_with_input(BenchmarkId::new("one_char", n), &index, |b, index| {
            // 单字符：trigram 认不出，走 LIKE 回退（全表扫）——测它的代价。
            b.iter(|| index.search("7", 50).expect("查找失败"));
        });
        group.finish();
    }
}

fn bench_upsert(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_upsert_batch");
    group.sample_size(20);
    for n in [1_000usize, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let mut index = FileIndex::open_in_memory().expect("打开内存索引失败");
                let entries: Vec<CrawledEntry> = (0..n)
                    .map(|i| CrawledEntry {
                        path: PathBuf::from(format!("/bench/{i:06}.txt")),
                        name: format!("{i:06}.txt"),
                        is_dir: false,
                    })
                    .collect();
                index.upsert_batch(&entries).expect("批量写入失败");
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_search, bench_upsert);
criterion_main!(benches);
