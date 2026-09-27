//! 全局搜索索引的持久化与自举。
//!
//! 之前索引是**内存库**（`FileIndex::open_in_memory`），而且只有命令面板里那条
//! 「索引当前目录」能触发爬取——结果是：⌘F 打开搜索框，除非先手动跑一次索引，
//! 否则什么都搜不到；重开应用又归零。这里钉的是「索引落在盘上」+「不用手动触发」。

use mo_app::AppState;
use std::path::PathBuf;

mod common;

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
    let base = tree("persist");
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    // 两次构造开的是**同一个**隔离库（同一个 `tag`），且整段都在 `isolated` 里跑完：
    // 目录只建一次，不会被第二次构造时「先清一遍」把自己上一轮的索引抹掉。
    let (found_now, after_restart) = common::isolated("persist", || {
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
        (found_now, after_restart)
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
    let base = tree("visited");
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app = common::isolated("visited", AppState::new);
    rt.block_on(async {
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
