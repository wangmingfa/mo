//! 全局搜索索引的持久化与自举。
//!
//! 之前索引是**内存库**（`FileIndex::open_in_memory`），而且只有命令面板里那条
//! 「索引当前目录」能触发爬取——结果是：⌘F 打开搜索框，除非先手动跑一次索引，
//! 否则什么都搜不到；重开应用又归零。这里钉的是「索引落在盘上」+「不用手动触发」。

use mo_app::AppState;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

/// 把索引库钉到临时目录（`MO_CACHE_DIR`），别往开发者机器上的真实索引里写。
///
/// ⚠️ 按**测试名**分目录 + 全程互斥（见 [`ENV_LOCK`]）：`MO_CACHE_DIR` 是
/// **进程级**环境变量，而同二进制的测试默认多线程并行——A 刚把变量指到自己的
/// 目录，B 又把它改到自己的，之后谁 `AppState::new()` 打开的就是谁的库。
/// 于是「重开应用后索引归零」这种 assert 会随调度时机随机翻车，
/// 单跑还永远复现不了（单线程 = 没有竞态）。
fn use_temp_index(tag: &str) {
    let dir = std::env::temp_dir().join(format!("mo-index-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("建索引目录");
    std::env::set_var("MO_CACHE_DIR", &dir);
}

/// 串行化本文件里所有动 `MO_CACHE_DIR` 的测试（进程级变量只能进程级保护）。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    // 中毒的锁说明某个持锁测试 panic 过——直接拿回锁继续跑，别让前一个的
    // 失败把这一个也拖成「获取锁失败」。
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn tree(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("mo-gidx-{}-{}-{}", tag, std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sub")).expect("建目录树");
    std::fs::write(base.join("ledger-report.md"), b"a").expect("写文件");
    std::fs::write(base.join("sub").join("notes.txt"), b"b").expect("写文件");
    base
}

/// 轮询直到 `f()` 为真（最多约 6 秒）。
async fn wait_for<F: Fn() -> bool>(f: F) -> bool {
    for _ in 0..300 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

/// 索引必须落在盘上：换一个 `AppState`（等于重开应用）后，之前爬的内容还在。
#[test]
fn index_survives_a_restart() {
    let _env = env_lock();
    use_temp_index("persist");
    let base = tree("persist");
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let found_now = rt.block_on(async {
        let app = AppState::new();
        app.index_root(base.clone(), 0);
        wait_for(|| app.index_count() >= 2).await;
        let n = app.global_search("ledger", 50).len();
        // 顺序 drop：先退出异步上下文，再释放 runtime。
        n
    });

    let after_restart = rt.block_on(async {
        // 全新的 AppState = 重开应用：没有再爬一遍，但索引应当还在。
        let app = AppState::new();
        let count = app.index_count();
        let hits = app.global_search("ledger", 50);
        (count, hits.len())
    });

    assert!(found_now > 0, "爬完之后应当能搜到");
    assert!(after_restart.0 > 0, "重开应用后索引不该归零");
    assert!(
        after_restart.1 > 0,
        "重开应用后不必重新索引也该搜得到（索引已落盘）"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// 进过的目录自动进索引：不用先手动跑「索引当前目录」。
#[test]
fn visiting_a_directory_indexes_it() {
    let _env = env_lock();
    use_temp_index("visited");
    let base = tree("visited");
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    rt.block_on(async {
        let app = AppState::new();
        // 只打开目录，不调 index_root —— 这正是用户实际做的事。
        app.open_directory(&base).await.expect("打开目录");
        let ok = wait_for(|| !app.global_search("ledger", 50).is_empty()).await;
        assert!(ok, "进过这个目录之后，全局搜索应当能搜到里面的文件");

        // 子目录（深度 1）也算进来：`VISITED_INDEX_DEPTH` 的意义就在这里。
        let deep = wait_for(|| !app.global_search("notes", 50).is_empty()).await;
        assert!(deep, "子目录里的文件也该被索引到");
    });

    let _ = std::fs::remove_dir_all(&base);
}
