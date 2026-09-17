//! 导航栈回归测试。
//!
//! 曾经的实现里，`go_back` 在弹出导航目标后又调用了一次 `open_directory`。
//! 而 `open_directory` 会 `visit()` —— 这会把刚回来的位置重新压进后退栈，
//! 并清空前进栈，导致「前进」按钮永远不可用。现在抽出了 `load_path`
//! （只加载、不改写历史），前进/后退共用它。

use mo_app::AppState;
use std::path::PathBuf;

fn tmp_dirs(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("mo-nav-{}-{}", tag, std::process::id()));
    let a = base.join("a");
    let b = base.join("b");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&a).expect("create dir a");
    std::fs::create_dir_all(&b).expect("create dir b");
    (base, a, b)
}

#[test]
fn go_back_keeps_forward_stack_usable() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app = AppState::new();
    let (base, a, b) = tmp_dirs("back-forward");

    let outcome = rt.block_on(async {
        app.open_directory(&a).await.expect("open a");
        app.open_directory(&b).await.expect("open b");

        app.go_back().await.expect("go back");
        let after_back = (app.current_path().await, app.can_go_forward().await);

        app.go_forward().await.expect("go forward");
        let after_forward = (app.current_path().await, app.can_go_back().await);

        (after_back, after_forward)
    });

    // 在同步上下文里 drop，避免嵌套 runtime 的阻塞限制。
    drop(app);
    let _ = std::fs::remove_dir_all(&base);

    assert_eq!(outcome.0 .0, Some(a), "后退后应回到 a");
    assert!(
        outcome.0 .1,
        "后退后前进栈必须仍可用（回归：旧实现会清空它）"
    );
    assert_eq!(outcome.1 .0, Some(b), "前进后应回到 b");
    assert!(outcome.1 .1, "前进后退栈应仍可用");
}

#[test]
fn refresh_does_not_pollute_history() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app = AppState::new();
    let (base, a, _b) = tmp_dirs("refresh");

    let outcome = rt.block_on(async {
        app.open_directory(&a).await.expect("open a");
        let back_before = app.can_go_back().await;
        app.refresh().await.expect("refresh");
        let back_after = app.can_go_back().await;
        (back_before, back_after, app.current_path().await)
    });

    drop(app);
    let _ = std::fs::remove_dir_all(&base);

    assert!(!outcome.0, "首次打开目录不应产生后退历史");
    assert!(
        !outcome.1,
        "刷新不应往导航栈里塞历史（回归：旧实现会调 visit）"
    );
    assert_eq!(outcome.2, Some(a), "刷新后仍停留在同一目录");
}
