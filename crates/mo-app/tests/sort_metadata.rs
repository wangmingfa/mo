//! 按大小 / 修改日期排序时，**元数据回填必须触发重排**。
//!
//! 用户报的现象：「下载」目录按修改日期排序，显示的 mtime 是新的、位置是旧的
//! （一条 2025-11-27 的文件夹排在 2025-07-23 后面）。根因：`read_dir` 不带
//! mtime（`ReadDirEntry` 只有名字 / 类型 / 路径），条目初载时全部 `Loading`
//! （排序键 = 0）；点「修改日期」表头那一刻回填还没完成，之后陆续到位的真实
//! mtime 只更新了显示，`update_metadata(_batch)` 不重排——排序键停在旧值。
//!
//! `load_path` 里缓存预填后已经按同一判据重排过一次（Size | Modified 才重排，
//! 按名称 / 类型排与元数据无关）；这里钉住的是**后台回填路径**的同款判据。

use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use filetime::{set_file_mtime, FileTime};
use mo_app::AppState;
use mo_core::{FileMetadata, MetadataState, SortDir, SortKey};

mod common;

/// 建目录 + 三个文件（mtime 各不相同、与名字的自然序**相反**，防用例碰巧通过）。
fn scene(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("mo-sort-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let stamp = |secs: u64| FileTime::from_unix_time(secs as i64, 0);
    // 名字自然序 a < b < c；mtime 故意倒着来：a 最新、c 最旧。
    std::fs::write(base.join("a-new.txt"), "x").unwrap();
    set_file_mtime(base.join("a-new.txt"), stamp(1_700_000_000)).unwrap();
    std::fs::write(base.join("b-mid.txt"), "x").unwrap();
    set_file_mtime(base.join("b-mid.txt"), stamp(1_600_000_000)).unwrap();
    std::fs::write(base.join("c-old.txt"), "x").unwrap();
    set_file_mtime(base.join("c-old.txt"), stamp(1_500_000_000)).unwrap();
    std::fs::create_dir(base.join("资料夹")).unwrap();
    set_file_mtime(base.join("资料夹"), stamp(1_550_000_000)).unwrap();
    base
}

fn meta(secs: u64) -> FileMetadata {
    FileMetadata {
        size: 0,
        modified: Some(UNIX_EPOCH + Duration::from_secs(secs)),
        created: None,
        permissions: mo_core::Permissions::default(),
    }
}

/// 等后台元数据回填跑完（每条目只回填一次；之后的批量更新就是最终值）。
async fn wait_backfilled(app: &AppState, n: usize) {
    for _ in 0..400 {
        let es = app.current_entries().await;
        if es.len() >= n
            && es
                .iter()
                .all(|e| matches!(e.metadata, MetadataState::Loaded(_)))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("元数据回填超时");
}

/// 回填改 mtime 后，按修改日期排的视图必须跟着重排（目录仍恒在最前）。
#[tokio::test]
async fn metadata_backfill_resorts_by_modified() {
    let base = scene("resort");
    let app = common::isolated("sort-resort", AppState::new);
    app.open_local(&base).await.expect("打开目录");
    wait_backfilled(&app, 4).await;

    // 此时真实 mtime 已就位：a(1.7G秒) > 资料(1.55) > b(1.6G)…… 等等，b=1.6G
    // 比 a 旧、比资料新。降序：目录恒在最前，文件 a > b > c。
    app.set_sort(SortKey::Modified, SortDir::Desc).await;
    let names_before: Vec<String> = app
        .visible_window(0..4)
        .await
        .2
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(
        names_before,
        vec!["资料夹", "a-new.txt", "b-mid.txt", "c-old.txt"],
        "前提：真实元数据下的正确顺序"
    );

    // 模拟「回填到了新的时间」：c 反超成最新、a 落到最旧。
    let entries = app.current_entries().await;
    let id_of = |name: &str| entries.iter().find(|e| e.name == name).unwrap().id;
    app.update_metadata_batch(vec![
        (id_of("c-old.txt"), meta(1_800_000_000)),
        (id_of("a-new.txt"), meta(1_400_000_000)),
        (id_of("b-mid.txt"), meta(1_600_000_000)),
    ])
    .await;

    let names_after: Vec<String> = app
        .visible_window(0..4)
        .await
        .2
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(
        names_after,
        vec!["资料夹", "c-old.txt", "b-mid.txt", "a-new.txt"],
        "回填了新 mtime 就要重排——显示新时间、站旧位置就是本刀要修的 bug"
    );

    // 单条路径（watcher 的 Modified 事件走 `update_metadata`）同判据：a 反超回最新。
    app.update_metadata(id_of("a-new.txt"), meta(1_900_000_000))
        .await;
    let names_single: Vec<String> = app
        .visible_window(0..4)
        .await
        .2
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(
        names_single,
        vec!["资料夹", "a-new.txt", "c-old.txt", "b-mid.txt"],
        "单条元数据更新同样要重排（a=1.9G > c=1.8G > b=1.6G）"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// 按名称排时回填**不该**重排：名称序与元数据无关，重排是白做的（还会把
/// 「批内相对稳定」的语义搅掉）。钉住这条判据，防止有人把条件改成无条件重排。
#[tokio::test]
async fn metadata_backfill_keeps_name_order_untouched() {
    let base = scene("resort-name");
    let app = common::isolated("sort-name", AppState::new);
    app.open_local(&base).await.expect("打开目录");
    wait_backfilled(&app, 4).await;

    app.set_sort(SortKey::Name, SortDir::Asc).await;
    let before: Vec<String> = app
        .visible_window(0..4)
        .await
        .2
        .iter()
        .map(|e| e.name.clone())
        .collect();

    let entries = app.current_entries().await;
    let id_of = |name: &str| entries.iter().find(|e| e.name == name).unwrap().id;
    app.update_metadata_batch(vec![
        (id_of("c-old.txt"), meta(1_800_000_000)),
        (id_of("a-new.txt"), meta(1_400_000_000)),
    ])
    .await;

    let after: Vec<String> = app
        .visible_window(0..4)
        .await
        .2
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(before, after, "按名称排时回填不应搅动顺序");
    assert_eq!(
        after,
        vec!["资料夹", "a-new.txt", "b-mid.txt", "c-old.txt"],
        "前提：目录恒在最前 + 名称自然序（与排序方向无关）"
    );

    let _ = std::fs::remove_dir_all(&base);
}
