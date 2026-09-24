//! 暂存区（收集夹）的核心契约。
//!
//! 与剪贴板（⌘C / ⌘V）的分工是这里唯一要钉死的东西：**累积**而非替换——
//! 可以连着在好几个目录里各挑几个文件，最后一起投递。由此派生出三条：
//!
//! 1. 换目录再收集，清单是**追加**的，不清空上一处；
//! 2. 同一路径重复收集只留一条（否则粘贴时会变成「复制两份、第二份改名」）；
//! 3. 复制后**保留**（还要往别的目录放一份），移动后**清空**（源已经不在了）。
//!
//! ⚠️ 每个用例都用 [`app`] 造一份**独占**的暂存区：进程级那一份（`mo_app::staging`）
//! 会被并行跑的用例互相串味（同 `AppState::with_sessions` 的道理）。

use mo_app::{session_registry, AppState, Staging};
use parking_lot::Mutex as PlMutex;
use std::path::PathBuf;
use std::sync::Arc;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-app-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

/// 一份独占的暂存区（回收站也钉到临时目录，免得写进开发者机器上的真回收站）。
fn app(tag: &str) -> AppState {
    AppState::with_staging(
        tmp(&format!("{tag}-trash")),
        session_registry(),
        Arc::new(PlMutex::new(Staging::new())),
    )
}

/// 等一个路径出现（传输是后台提交的操作，测试不能「等」，只能轮询）。
async fn wait_exists(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

/// 跨目录收集：清单是**追加**的，来源目录逐条记着。
#[tokio::test]
async fn collecting_accumulates_across_directories() {
    let a = tmp("staging-a");
    let b = tmp("staging-b");
    std::fs::write(a.join("a.txt"), b"a").unwrap();
    std::fs::write(a.join("b.txt"), b"b").unwrap();
    std::fs::write(b.join("c.txt"), b"c").unwrap();

    let app = app("staging-accumulate");
    app.open_directory(&a).await.expect("打开 A 失败");
    app.select_all_visible().await;
    assert_eq!(app.stage_selection().await, 2);

    // 换到另一个目录再收集：上一处的两条还在（剪贴板在这里就已经被覆盖了）。
    app.open_directory(&b).await.expect("打开 B 失败");
    app.select_all_visible().await;
    assert_eq!(app.stage_selection().await, 1);
    assert_eq!(app.staged_count(), 3, "换目录收集应当是追加，不是替换");

    let froms: Vec<PathBuf> = app.staged().iter().map(|e| e.from.clone()).collect();
    assert_eq!(froms, vec![a.clone(), a.clone(), b.clone()], "来源逐条记住");
}

/// 同一路径重复收集只留一条。
#[tokio::test]
async fn recollecting_the_same_path_does_not_duplicate() {
    let dir = tmp("staging-dedup");
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    std::fs::write(dir.join("b.txt"), b"b").unwrap();

    let app = app("staging-dedup");
    app.open_directory(&dir).await.expect("打开目录失败");
    app.select_all_visible().await;
    assert_eq!(app.stage_selection().await, 2);

    // 只选 a.txt 再收集一次：它已经在里面了，条数不能变。
    app.focus_by_prefix("a").await.expect("应定位到 a.txt");
    assert_eq!(app.stage_selection().await, 0, "重复收集应被去重");
    assert_eq!(app.staged_count(), 2);
}

/// 复制投递：文件到了目标目录，清单**保留**（还要往别的目录放一份）。
#[tokio::test]
async fn copying_keeps_the_list_for_another_destination() {
    let src = tmp("staging-copy-src");
    let dst = tmp("staging-copy-dst");
    std::fs::write(src.join("a.txt"), b"a").unwrap();
    std::fs::write(src.join("b.txt"), b"b").unwrap();

    let app = app("staging-copy");
    app.open_directory(&src).await.expect("打开源目录失败");
    app.select_all_visible().await;
    app.stage_selection().await;

    app.open_directory(&dst).await.expect("打开目标目录失败");
    let ids = app.paste_staged(None, false).await;
    assert_eq!(ids.len(), 2, "两条各提交一个操作");
    assert!(
        wait_exists(&dst.join("a.txt")).await && wait_exists(&dst.join("b.txt")).await,
        "暂存区的两个文件都应落到目标目录"
    );
    assert_eq!(app.staged_count(), 2, "复制后保留清单：可以再往别处放一份");
    assert!(src.join("a.txt").exists(), "复制不动源文件");
}

/// 移动投递：文件离开源目录，清单**清空**（源已经不在了，留着只会让下次粘贴失败）。
#[tokio::test]
async fn moving_empties_the_list() {
    let src = tmp("staging-move-src");
    let dst = tmp("staging-move-dst");
    std::fs::write(src.join("a.txt"), b"a").unwrap();

    let app = app("staging-move");
    app.open_directory(&src).await.expect("打开源目录失败");
    app.select_all_visible().await;
    app.stage_selection().await;

    app.open_directory(&dst).await.expect("打开目标目录失败");
    app.paste_staged(None, true).await;
    assert!(
        wait_exists(&dst.join("a.txt")).await,
        "文件应被移动到目标目录"
    );
    assert_eq!(app.staged_count(), 0, "移动后清单清空");
}

/// 逐条移除与清空。
#[tokio::test]
async fn unstage_and_clear() {
    let dir = tmp("staging-remove");
    for n in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.join(n), b"x").unwrap();
    }

    let app = app("staging-remove");
    app.open_directory(&dir).await.expect("打开目录失败");
    app.select_all_visible().await;
    app.stage_selection().await;
    assert_eq!(app.staged_count(), 3);

    assert!(app.unstage(&dir.join("b.txt")));
    assert!(
        !app.unstage(&dir.join("b.txt")),
        "移除一条不存在于清单里的路径应返回 false"
    );
    assert_eq!(app.staged_count(), 2);

    app.clear_staged();
    assert!(app.staged().is_empty());
}

/// 清单是**进程级共享**的：两个（窗格 / 标签页的）`AppState` 拿到同一份。
///
/// 这是「跨目录挑文件」的前提——各自一份就成了另一个剪贴板。
#[tokio::test]
async fn the_list_is_shared_between_two_app_states() {
    let dir = tmp("staging-shared");
    std::fs::write(dir.join("a.txt"), b"a").unwrap();

    let shared = Arc::new(PlMutex::new(Staging::new()));
    let one = AppState::with_staging(
        tmp("staging-shared-trash1"),
        session_registry(),
        shared.clone(),
    );
    let two = AppState::with_staging(
        tmp("staging-shared-trash2"),
        session_registry(),
        shared.clone(),
    );

    // 两个 AppState 指向不同目录，模拟「分栏两边」。
    one.open_directory(&dir).await.expect("打开目录失败");
    one.select_all_visible().await;
    assert_eq!(one.stage_selection().await, 1);
    assert_eq!(two.staged_count(), 1, "另一侧应当看得见同一份清单");
}
