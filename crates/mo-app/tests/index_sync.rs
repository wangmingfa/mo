//! 条目被**移走**之后，全局索引里不该留下它（devlog/windows-port.md §45）。
//!
//! 触发这次收账的是用户的「拖动多个文件进行移动时，界面会卡住几秒钟」：§43 把
//! 同盘直拖改成默认移动之后，一次拖四个文件就是四次「rename 出当前目录」，
//! 每一次都要跑一遍 `FileIndex::remove_under`。旧写法是
//! `DELETE ... WHERE path LIKE '前缀/%'`——58 万行的真索引上实测**一次 147ms 的
//! 全表扫**（LIKE 的前缀通配用不上索引，`EXPLAIN QUERY PLAN` 给的是 `SCAN files`），
//! 期间索引锁被攥着；而同一段时间里主线程每拍都要问一次「已索引 N」（那也是一条
//! 64ms 的 `COUNT(*)`），排队等锁 + 全表扫来回叠加，就是那几秒钟的停顿。
//! 换成 path 的字节范围扫描后走 `idx_path`，未命中 33µs。
//!
//! 数字与查询计划由 `cargo run -p mo-search --example probe_freeze -- <索引库副本>`
//! 量出来（见该文件头）。
//!
//! 这里钉的是**行为**那一面，两条各对应旧写法的一种错法：
//! 1. 移走的文件本身要从索引里消失（这条旧写法也对，钉住不回归）；
//! 2. 移走的**目录**要连子树一起消失——Windows 落库的是反斜杠路径，旧写法拼的
//!    `{前缀}/%` 一个孩子都匹配不上，本机删目录只删了目录自己那条，剩下的孤儿
//!    「搜得到、点不开」。
//!
//! `_` 当通配符那一条（`LIKE 'a_b.txt/%'` 会连邻居 `axb.txt` 的子树一起抹）在
//! `mo-search` 的单测里钉（`remove_under_does_not_treat_underscores_as_wildcards`）：
//! 索引的搜索本身也用 LIKE，在应用层断言「邻居还在」会因为查询侧的通配而绕不开
//! 竞态，交给纯函数层测更准。
//!
//! 事件一律直投 `apply_watcher_event`：headless 里首标签页没有 watcher 泵，而这条
//! 链要验的正是「watcher 事件 → 索引同步」那一段。

use std::fs;
use std::path::{Path, PathBuf};

use mo_app::AppState;
use mo_fs::WatcherEvent;

mod common;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

/// 一棵临时目录树：`<base>/src/...`（被监听、被爬取）与 `<base>/dst/`（移动的目标端）。
/// `src_files` 里写 `box/inside.txt` 这样的相对路径，父目录自动建出来。
fn tree(tag: &str, src_files: &[&str]) -> PathBuf {
    let base = std::env::temp_dir().join(format!("mo-indexsync-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("dst")).unwrap();
    for name in src_files {
        let p = base.join("src").join(name);
        fs::create_dir_all(p.parent().expect("相对路径总有父目录")).unwrap();
        fs::write(&p, b"payload").unwrap();
    }
    base
}

/// 轮询直到 `f()` 为真（约 6 秒）。索引的写都在 blocking 池里跑，得等。
async fn wait_until<F: Fn() -> bool>(f: F) -> bool {
    for _ in 0..300 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

/// 把 `src` 整棵爬进索引，并等这些名字真的可搜（爬取是后台派发的）。
async fn index_src(app: &AppState, src: &Path, needles: &[&str]) {
    app.index_root(src.to_path_buf(), 0);
    for needle in needles {
        let found = wait_until(|| !app.global_search(needle, 50).is_empty()).await;
        assert!(found, "爬完之后 {needle} 应当搜得到");
    }
}

/// ③ 状态栏那个「已索引 N」是**缓存 + 后台重算**，不是每问一次扫一遍全表。
///
/// 这里钉的是「重算真的会落地」：先把缓存种下（此刻的数），再让索引长大，数字
/// 必须在几拍之内跟上。把 `index_count` 里的派发摘掉（只读缓存、永不重算）——
/// 现有测试一处都不会红：`global_index.rs` 的断言打在 `global_search` 上，
/// 重启那半走的是「进程内第一次就地数」的分支。所以这条必须存在。
#[test]
fn the_indexed_count_settles_after_the_cache_is_seeded() {
    let base = tree("count", &["one.txt"]);
    let src = base.join("src");
    let app = common::isolated("count", AppState::new);

    runtime().block_on(async {
        let seeded = app.index_count();
        fs::write(src.join("two.txt"), b"payload").unwrap();
        fs::write(src.join("three.txt"), b"payload").unwrap();
        app.index_root(src.clone(), 0);

        assert!(
            wait_until(|| app.index_count() > seeded).await,
            "爬取新增了三条，缓存的数字该在 TTL 之后被后台重算追上来（起点 {seeded}）"
        );
    });

    let _ = fs::remove_dir_all(&base);
}

fn hits(app: &AppState, needle: &str) -> Vec<PathBuf> {
    app.global_search(needle, 50)
        .into_iter()
        .map(|h| h.path)
        .collect()
}

/// ① 单个文件被移到别的目录：源路径要从索引里消失。
///
/// 这正是 §43 那次拖动里每个文件走的路：跨目录 rename 在 `apply_watcher_event`
/// 里按「离开本目录」处理（`from.parent() != to.parent()`）→ `sync_index_removed(from)`。
#[test]
fn a_moved_away_file_leaves_the_index() {
    let base = tree("file", &["aardvark.txt"]);
    let src = base.join("src");
    let app = common::isolated("file", AppState::new);

    runtime().block_on(async {
        app.open_local(&src).await.expect("打开 src");
        index_src(&app, &src, &["aardvark"]).await;

        app.apply_watcher_event(WatcherEvent::Renamed {
            from: src.join("aardvark.txt"),
            to: base.join("dst").join("aardvark.txt"),
        })
        .await;

        assert!(
            wait_until(|| hits(&app, "aardvark").is_empty()).await,
            "文件已经不在 src 了，搜索不该还把它端出来：{:?}",
            hits(&app, "aardvark")
        );
    });

    let _ = fs::remove_dir_all(&base);
}

/// ② 整个目录被移走：它下面的记录要**连子树**一起消失。
///
/// 旧写法在 Windows 上正是错在这一条——落库的是反斜杠路径，`LIKE '前缀/%'`
/// 匹配不到任何孩子，`inside.txt` 就成了孤儿。
#[test]
fn a_moved_away_directory_takes_its_subtree() {
    let base = tree("subtree", &["box/inside.txt"]);
    let src = base.join("src");
    let app = common::isolated("subtree", AppState::new);

    runtime().block_on(async {
        app.open_local(&src).await.expect("打开 src");
        // 先确认子记录真的进了索引，否则「消失」可以是因为从来没进来。
        index_src(&app, &src, &["inside"]).await;

        app.apply_watcher_event(WatcherEvent::Renamed {
            from: src.join("box"),
            to: base.join("dst").join("box"),
        })
        .await;

        assert!(
            wait_until(|| hits(&app, "inside").is_empty()).await,
            "目录都搬走了，它那条子记录不该留在索引里：{:?}",
            hits(&app, "inside")
        );
    });

    let _ = fs::remove_dir_all(&base);
}
