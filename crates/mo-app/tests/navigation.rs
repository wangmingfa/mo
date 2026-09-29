//! 导航栈回归测试。
//!
//! 曾经的实现里，`go_back` 在弹出导航目标后又调用了一次 `open_directory`。
//! 而 `open_directory` 会 `visit()` —— 这会把刚回来的位置重新压进后退栈，
//! 并清空前进栈，导致「前进」按钮永远不可用。现在抽出了 `load_path`
//! （只加载、不改写历史），前进/后退共用它。

use mo_app::AppState;
use std::path::PathBuf;

mod common;

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
    let app = common::isolated("back-forward", AppState::new);
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
    let app = common::isolated("refresh", AppState::new);
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

/// 推出卷宗后，正在看卷里的页面要被带走（回到卷的父目录）。
///
/// 用户报的现象：推出「/Volumes/TraeWork CN」后地址栏与文件列表还停在原处——
/// 卷已经没了，往后随便一按（排序、刷新、双击）全是错。
#[test]
fn ejecting_a_volume_navigates_to_its_parent() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app = common::isolated("eject-leave", AppState::new);
    let base = std::env::temp_dir().join(format!("mo-eject-{}", std::process::id()));
    let volume = base.join("TraeWork CN");
    let inside = volume.join("子目录");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&inside).unwrap();

    let outcome = rt.block_on(async {
        app.open_local(&inside).await.expect("打开卷里的目录");
        assert_eq!(app.current_path().await, Some(inside.clone()));
        let navigated = app.leave_ejected_volume(&volume).await;
        (navigated, app.current_path().await)
    });

    drop(app);
    let _ = std::fs::remove_dir_all(&base);

    assert!(outcome.0, "在卷里：应当导航");
    assert_eq!(
        outcome.1,
        Some(volume.parent().unwrap().to_path_buf()),
        "落点是卷的父目录"
    );
}

/// 看的不是卷里（别的目录 / 名字相近的**兄弟**卷）就是空操作。
///
/// 名字相近那条钉住分量级比较：`/Volumes/X2` 不在 `/Volumes/X` 里——
/// `Path::starts_with` 按分量比，字符串前缀就会误判。
#[test]
fn eject_leaves_other_locations_alone() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app = common::isolated("eject-stay", AppState::new);
    let base = std::env::temp_dir().join(format!("mo-eject-stay-{}", std::process::id()));
    let volume = base.join("vol");
    let sibling = base.join("vol2");
    let elsewhere = base.join("elsewhere");
    let _ = std::fs::remove_dir_all(&base);
    for d in [&volume, &sibling, &elsewhere] {
        std::fs::create_dir_all(d).unwrap();
    }

    let outcome = rt.block_on(async {
        // 名字相近的兄弟卷。
        app.open_local(&sibling).await.expect("打开 vol2");
        let sibling_case = (
            app.leave_ejected_volume(&volume).await,
            app.current_path().await,
        );
        // 完全不相干的目录。
        app.open_local(&elsewhere).await.expect("打开 elsewhere");
        let unrelated_case = (
            app.leave_ejected_volume(&volume).await,
            app.current_path().await,
        );
        (sibling_case, unrelated_case)
    });

    drop(app);
    let _ = std::fs::remove_dir_all(&base);

    assert!(!outcome.0 .0, "兄弟卷不算在里面");
    assert_eq!(outcome.0 .1, Some(sibling), "不该被带走");
    assert!(!outcome.1 .0, "别的目录不算在里面");
    assert_eq!(outcome.1 .1, Some(elsewhere), "不该被带走");
}
