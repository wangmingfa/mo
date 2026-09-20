//! 文件夹同步的应用层集成测试：配对持久化 → 计划 → 执行 → 多余文件交回回收站。
//!
//! ⚠️ 必须先设 `MO_CONFIG_DIR`：`set_sync_target` 会写配置文件，不隔离就会
//! 污染开发者机器上的真实配置。

use std::fs;
use std::path::{Path, PathBuf};

use mo_app::AppState;
use mo_operations::{SyncConflictPolicy, SyncMode, SyncOptions};

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "mo-sync-it-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&p, body).unwrap();
}

/// 隔离配置目录（整个测试进程共用一份，所以测试之间不并发写同一个键）。
fn isolate_config() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-sync-it-config-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    std::env::set_var("MO_CONFIG_DIR", &dir);
    dir
}

#[tokio::test]
async fn pair_persists_and_plan_executes() {
    isolate_config();
    let src = scratch("src");
    let dst = scratch("dst");
    put(&src, "a.txt", "AAA");
    put(&src, "sub/b.txt", "BBB");
    put(&dst, "only-dst.txt", "D");

    let app = AppState::new();
    // 未配对时读不到目标。
    assert!(app.sync_target(&src).is_none());

    app.set_sync_target(&src, Some(&dst));
    assert_eq!(app.sync_target(&src), Some(dst.clone()), "配对必须持久化");

    // 双向合并：a/b 补到目标端，only-dst 补回源端。
    let opts = SyncOptions {
        mode: SyncMode::TwoWay,
        conflict: SyncConflictPolicy::NewerWins,
        delete_extras: false,
    };
    let plan = app
        .sync_plan(src.clone(), dst.clone(), opts)
        .await
        .expect("生成计划");
    assert_eq!(plan.actions.len(), 3, "{:?}", plan.actions);

    let (report, victims) = app.sync_apply(src.clone(), dst.clone(), plan).await;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.copied, 3, "{report:?}");
    assert!(victims.is_empty(), "双向合并不该产生删除");
    assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "AAA");
    assert_eq!(fs::read_to_string(dst.join("sub/b.txt")).unwrap(), "BBB");
    assert_eq!(fs::read_to_string(src.join("only-dst.txt")).unwrap(), "D");

    // 再计划一次应当无事可做（两侧已一致）。
    let again = app
        .sync_plan(src.clone(), dst.clone(), opts)
        .await
        .expect("第二次生成计划");
    assert!(again.is_empty(), "同步完不该还有动作：{:?}", again.actions);

    // 镜像 + 清理：目标端多余的应作为待删除项交回（由调用方送回收站）。
    put(&dst, "stray.txt", "S");
    let mirror = SyncOptions {
        mode: SyncMode::Mirror,
        conflict: SyncConflictPolicy::Skip,
        delete_extras: true,
    };
    let plan = app
        .sync_plan(src.clone(), dst.clone(), mirror)
        .await
        .expect("镜像计划");
    let (report, victims) = app.sync_apply(src.clone(), dst.clone(), plan).await;
    assert_eq!(report.copied, 0, "镜像方向上没有新文件：{report:?}");
    assert_eq!(victims.len(), 1, "{victims:?}");
    assert!(victims[0].ends_with("stray.txt"));
    assert!(
        dst.join("stray.txt").exists(),
        "应用层没调用回收站之前，文件必须还在"
    );

    // 解除配对。
    app.set_sync_target(&src, None);
    assert!(app.sync_target(&src).is_none());

    fs::remove_dir_all(&src).ok();
    fs::remove_dir_all(&dst).ok();}
