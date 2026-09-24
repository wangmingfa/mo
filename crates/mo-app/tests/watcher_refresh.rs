//! watcher 事件的增量应用语义（[`AppState::apply_watcher_event`]）。
//!
//! 背景（用户报的「文件删除了，但是列表还显示」）：把文件搬去回收站是一次
//! 「rename 出当前目录」，而 macOS FSEvents 对它的报告形态是**单路径**
//! `Modify(Name)`——不是 `Remove`，也不是双路径 `Renamed`（探针
//! `mo-fs/examples/watch_probe.rs` 的实测输出）。`Modified` 分支若只更新
//! 元数据，这一行就永远留在列表里。
//!
//! 为什么守卫落在 `mo-app` 而不是 UI 层：见 `remote_local.rs` 顶部的说明
//! （headless 的 GPUI 测试调度器不允许「等外部 IO」）。

use std::fs;
use std::path::PathBuf;

use mo_app::AppState;
use mo_fs::WatcherEvent;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn tree(tag: &str, files: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-watcher-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for name in files {
        fs::write(dir.join(name), b"x").unwrap();
    }
    dir
}

fn trash_root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mo-watcher-trash-{tag}-{}", std::process::id()))
}

/// 真实形态：文件被搬出目录（回收站 / `mv` 走），watcher 报单路径
/// `Modify(Name)` —— 条目必须从列表里消失。
#[test]
fn a_rename_out_reported_as_modified_removes_the_entry() {
    let dir = tree("gone", &["gone.txt"]);
    let trash = trash_root("gone");
    let app = AppState::with_trash(trash.clone());

    runtime().block_on(async {
        app.open_local(&dir).await.expect("打开本地目录");
        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "gone.txt"),
            "前置条件：条目应当先在列表里"
        );

        // 真把文件搬出目录（= 删除走回收站的真实路径），再投喂 FSEvents 形态的事件。
        let gone = dir.join("gone.txt");
        let outside = dir.with_extension("out");
        fs::create_dir_all(&outside).unwrap();
        fs::rename(&gone, outside.join("gone.txt")).unwrap();
        app.apply_watcher_event(WatcherEvent::Modified(gone.clone()))
            .await;

        assert!(
            !app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "gone.txt"),
            "文件已搬出目录（watcher 报单路径 Modify），条目不该留在列表里"
        );
    });

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&trash);
}

/// 反向守卫：`Modified` 只在**盘上真的没了**时才摘条目——内容变化
/// （保存文件）仍然只是刷新元数据，绝不能把好好的行删掉。
#[test]
fn a_modify_with_the_file_still_on_disk_keeps_the_entry() {
    let dir = tree("kept", &["kept.txt"]);
    let trash = trash_root("kept");
    let app = AppState::with_trash(trash.clone());

    runtime().block_on(async {
        app.open_local(&dir).await.expect("打开本地目录");

        // 文件还在盘上（只改了内容）：事件照常投喂。
        fs::write(dir.join("kept.txt"), b"changed").unwrap();
        app.apply_watcher_event(WatcherEvent::Modified(dir.join("kept.txt")))
            .await;

        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "kept.txt"),
            "文件还在盘上，Modify 事件不能把条目删掉"
        );
    });

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&trash);
}

/// inotify 形态的跨目录 rename（`Renamed{from, to}`，to 在目录外）：源条目
/// 已经离开本目录，按删除处理——**不能**把条目的 path 改写到目录外
/// （改写后这行点不开、也再不会被任何事件刷新掉）。
#[test]
fn a_cross_directory_rename_event_removes_the_source_entry() {
    let dir = tree("moved", &["moved.txt"]);
    let trash = trash_root("moved");
    let app = AppState::with_trash(trash.clone());
    let outside = dir.with_extension("out2");

    runtime().block_on(async {
        app.open_local(&dir).await.expect("打开本地目录");
        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "moved.txt"),
            "前置条件：条目应当先在列表里"
        );

        app.apply_watcher_event(WatcherEvent::Renamed {
            from: dir.join("moved.txt"),
            to: outside.join("moved.txt"),
        })
        .await;

        let entries = app.current_entries().await;
        assert!(
            !entries.iter().any(|e| e.name == "moved.txt"),
            "跨目录 rename 的源条目必须摘掉"
        );
        assert!(
            !entries.iter().any(|e| e.path == outside.join("moved.txt")),
            "条目不能被改写到目录外——那是一行既点不开也刷不掉的僵尸"
        );
    });

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&trash);
}

/// 同目录 rename 语义不回归：`Renamed{from, to}` 都在本目录里时照旧改写
/// 条目（改名是可撤销操作链路的基础）。
#[test]
fn a_same_directory_rename_still_rewrites_the_entry() {
    let dir = tree("rename", &["old.txt"]);
    let trash = trash_root("rename");
    let app = AppState::with_trash(trash.clone());

    runtime().block_on(async {
        app.open_local(&dir).await.expect("打开本地目录");

        app.apply_watcher_event(WatcherEvent::Renamed {
            from: dir.join("old.txt"),
            to: dir.join("new.txt"),
        })
        .await;

        let entries = app.current_entries().await;
        assert!(!entries.iter().any(|e| e.name == "old.txt"), "旧名应当消失");
        let renamed = entries.iter().find(|e| e.name == "new.txt");
        assert!(renamed.is_some(), "同目录 rename 应当改写条目为新名");
        assert_eq!(
            renamed.map(|e| e.path.clone()),
            Some(dir.join("new.txt")),
            "条目 path 应当指向新名字"
        );
    });

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&trash);
}
