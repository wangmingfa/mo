//! 大目录优化相关的行为测试：过滤、可见区窗口、元数据缓存预热。

use std::path::PathBuf;
use std::time::Duration;

use mo_app::AppState;
use mo_core::MetadataState;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-app-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

#[tokio::test]
async fn filter_narrows_visible_count_and_can_be_cleared() {
    let dir = tmp("filter");
    for name in ["alpha.txt", "beta.txt", "gamma.md"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");
    assert_eq!(app.visible_count().await, 3);

    app.set_filter(Some("txt".to_string())).await;
    assert_eq!(app.visible_count().await, 2, "只应剩下两个 txt");
    assert!(app.is_filtered().await);

    app.set_filter(None).await;
    assert_eq!(app.visible_count().await, 3, "清除过滤后应恢复");
}

#[tokio::test]
async fn visible_window_returns_only_requested_range() {
    let dir = tmp("window");
    for i in 0..10 {
        std::fs::write(dir.join(format!("file-{i}.txt")), b"x").unwrap();
    }

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");

    let (start, entries) = app.visible_window(2..5).await;
    assert_eq!(start, 2);
    assert_eq!(entries.len(), 3);
    // 自然序下第 2..5 个可见条目就是 file-2 ~ file-4。
    assert_eq!(entries[0].name, "file-2.txt");
    assert_eq!(entries[2].name, "file-4.txt");
}

#[tokio::test]
async fn metadata_cache_primes_entries_on_reopen() {
    let dir = tmp("prime");
    for name in ["one.bin", "two.bin", "three.bin"] {
        std::fs::write(dir.join(name), b"payload").unwrap();
    }

    // 第一次打开：后台 stat 并写回缓存。
    let first = AppState::new();
    first.open_directory(&dir).await.expect("打开目录失败");
    tokio::time::sleep(Duration::from_millis(800)).await;

    // 第二次打开（新实例，共享同一个默认缓存库）：应立刻带上缓存里的元数据。
    let second = AppState::new();
    second.open_directory(&dir).await.expect("打开目录失败");
    let entries = second.current_entries().await;
    let primed = entries
        .iter()
        .filter(|e| matches!(e.metadata, MetadataState::Loaded(_)))
        .count();

    assert!(
        primed > 0,
        "重开目录应命中元数据缓存并立即填充，实际命中 {primed} 条"
    );
}
