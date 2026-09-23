//! 「显示隐藏文件」开关的集成测试。
//!
//! 开关在 `mo-config` 里一直是有的（`show_hidden`），但列目录的过滤没接上——
//! dotfile 恒显示，`.git` / `node_modules` 铺满整个列表。这里钉的是**过滤真的生效**
//! 且**切换后立刻重读**：只改开关不重读的话，界面上什么都没变，用户只会以为按键坏了。

use mo_app::AppState;
use std::path::PathBuf;

fn tree(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("mo-hidden-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("建临时目录");
    std::fs::write(base.join("visible.txt"), b"v").expect("写普通文件");
    std::fs::write(base.join(".secret"), b"s").expect("写 dotfile");
    std::fs::create_dir_all(base.join(".dotdir")).expect("写 dot 目录");
    base
}

/// 当前目录里可见条目的名字（按视图序）。
async fn names(app: &AppState) -> Vec<String> {
    let (_, _, entries) = app.visible_window(0..200).await;
    entries.iter().map(|e| e.name.clone()).collect()
}

#[test]
fn hidden_entries_are_filtered_until_the_switch_is_on() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app = AppState::new();
    // 开关会落盘到真实配置，测完原样恢复（不留下副作用）。
    let original = app.show_hidden();
    let base = tree("filter");

    let (off, on) = rt.block_on(async {
        app.set_show_hidden(false);
        app.open_directory(&base).await.expect("打开目录");
        let off = names(&app).await;

        app.set_show_hidden(true);
        // 切完必须重读：过滤发生在建视图之前，不重读列表不会变。
        app.refresh().await.expect("刷新");
        let on = names(&app).await;
        (off, on)
    });

    assert!(off.contains(&"visible.txt".to_string()), "普通文件始终可见");
    assert!(
        !off.iter().any(|n| n.starts_with('.')),
        "关掉开关时不该出现 dotfile：{off:?}"
    );
    assert_eq!(off.len(), 1, "只该剩 visible.txt：{off:?}");

    assert!(
        on.contains(&".secret".to_string()) && on.contains(&".dotdir".to_string()),
        "打开开关后 dotfile 必须出现：{on:?}"
    );
    assert!(on.contains(&"visible.txt".to_string()));
    assert_eq!(on.len(), 3, "三条都在：{on:?}");

    app.set_show_hidden(original);
    let _ = std::fs::remove_dir_all(&base);
}

/// 列视图与主列表同一条判据：换了个画法，藏起来的东西不该冒出来。
#[test]
fn columns_view_hides_them_too() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app = AppState::new();
    let original = app.show_hidden();
    let base = tree("columns");

    let (off, on) = rt.block_on(async {
        app.set_show_hidden(false);
        let off = app.list_dir(&base).await.expect("列目录");
        app.set_show_hidden(true);
        let on = app.list_dir(&base).await.expect("列目录");
        (off, on)
    });

    assert_eq!(off.len(), 1, "列视图不该列出 dotfile：{off:?}");
    assert_eq!(on.len(), 3, "开关打开后列视图也要列出：{on:?}");

    app.set_show_hidden(original);
    let _ = std::fs::remove_dir_all(&base);
}
