//! 操作历史（`AppState::record_history` / `history_snapshot`）。
//!
//! 引擎侧一直在记（环形 200 条：复制 / 移动 / 删除 / 重命名 + 落点目录），
//! 2026-09-29 之前 mo-ui 里**一个消费者都没有**——数据躺在内存里没人看。
//! 历史面板靠它吃饭，所以这里把两件事钉住：
//!
//! * **落点怎么算**（[`mo_app::HistoryEntry::landing_dir`]）：面板上「跳过去」
//!   用的就是这个目录，算错就等于把用户送到别的地方；
//! * **`dest` 恒为目录**（含重命名）：重命名记的是新名字所在目录，不是新名字
//!   那条路径——否则跳过去会拿一个文件路径当目录打开。
//!
//! 记账点本身（哪种操作记什么）只验重命名与删除两条：前者是「目录还是路径」
//! 这个坑的现场，后者是 `dest` 缺失时退回源目录的那一支。

use mo_app::{AppState, HistoryEntry};
use std::path::PathBuf;

mod common;

fn entry(kind: &str, sources: Vec<PathBuf>, dest: Option<PathBuf>) -> HistoryEntry {
    HistoryEntry {
        kind: kind.to_string(),
        sources,
        dest,
        remote: false,
        at: 1_700_000_000,
    }
}

/// 落点：有 `dest` 用 `dest`（它已经是目录），没有就退回第一个源所在目录。
#[test]
fn landing_dir_prefers_destination_then_source_parent() {
    let with_dest = entry(
        "复制",
        vec![PathBuf::from("/a/b.txt")],
        Some(PathBuf::from("/z")),
    );
    assert_eq!(with_dest.landing_dir(), Some(PathBuf::from("/z")));

    // 删除类没有 dest：落点是「事发现场」= 源所在目录。
    let deleted = entry("删除", vec![PathBuf::from("/a/b.txt")], None);
    assert_eq!(deleted.landing_dir(), Some(PathBuf::from("/a")));

    // 多个源（批量复制）：取第一个的父目录即可——同批一般同父，跳过去能看见结果。
    let many = entry(
        "复制",
        vec![PathBuf::from("/a/1.txt"), PathBuf::from("/b/2.txt")],
        None,
    );
    assert_eq!(many.landing_dir(), Some(PathBuf::from("/a")));

    // 既没有 dest 也没有源（不该发生，但别 panic）。
    let empty = entry("复制", Vec::new(), None);
    assert_eq!(empty.landing_dir(), None);
}

/// 快照**最新在前**，且环形上限是 200（老的自溢出去，不留 201 条）。
#[test]
fn snapshot_is_newest_first_and_capped_at_200() {
    let app = common::isolated("history-ring", AppState::new);
    for i in 0..205 {
        app.record_history(
            "复制",
            vec![PathBuf::from(format!("/src/{i}.txt"))],
            None,
            false,
        );
    }
    let snap = app.history_snapshot();
    assert_eq!(snap.len(), 200, "环形上限 200，多的被挤掉");
    // 最新的是第 204 条，最老的是第 5 条（0..=4 已被挤掉）。
    assert_eq!(
        snap.first().expect("非空").sources[0],
        PathBuf::from("/src/204.txt")
    );
    assert_eq!(
        snap.last().expect("非空").sources[0],
        PathBuf::from("/src/5.txt")
    );

    app.clear_history();
    assert!(app.history_snapshot().is_empty(), "清空后不该再有条目");
}

/// 重命名记的是**新名字所在目录**（不是新名字那条路径）。
#[tokio::test]
async fn rename_records_the_directory_not_the_new_path() {
    let app = common::isolated("history-rename", AppState::new);
    let root = std::env::temp_dir().join(format!("mo-history-rename-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("建临时目录");
    let from = root.join("a.txt");
    let to = root.join("b.txt");
    std::fs::write(&from, b"x").expect("写源文件");

    app.rename_many(vec![(from.clone(), to.clone())])
        .await
        .expect("重命名提交");

    let snap = app.history_snapshot();
    let e = snap.first().expect("记了一条");
    assert_eq!(e.kind, "重命名");
    assert_eq!(
        e.dest,
        Some(root.clone()),
        "dest 是目录，不是 b.txt 那条路径"
    );
    assert_eq!(e.landing_dir(), Some(root.clone()));
    assert!(!e.remote, "本地重命名不该标远程");
    let _ = std::fs::remove_dir_all(&root);
}

/// 删除没有 dest：落点退回源所在目录，且**不标远程**（本机回收站那条路）。
#[tokio::test]
async fn trash_records_local_delete_without_destination() {
    let app = common::isolated("history-trash", AppState::new);
    let root = std::env::temp_dir().join(format!("mo-history-trash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("建临时目录");
    let file = root.join("gone.txt");
    std::fs::write(&file, b"x").expect("写源文件");

    app.trash_paths(vec![file.clone()]).await;

    let snap = app.history_snapshot();
    let e = snap.first().expect("记了一条");
    assert_eq!(e.kind, "删除");
    assert_eq!(e.dest, None);
    assert_eq!(e.landing_dir(), Some(root.clone()), "删除的落点是事发现场");
    assert!(!e.remote);
    let _ = std::fs::remove_dir_all(&root);
}
