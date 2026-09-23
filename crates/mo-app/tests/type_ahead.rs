//! 输入即定位（type-ahead）的核心契约：敲字符跳到第一个前缀匹配项并选中，
//! 但**不收窄列表**（区别于「输入即过滤」）。大小写不敏感。

use std::path::PathBuf;

use mo_app::AppState;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-app-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

/// 小写前缀也能定位到大写开头的文件；定位是「选」，不是「过滤」。
#[tokio::test]
async fn locate_is_case_insensitive_and_does_not_filter() {
    let dir = tmp("typeahead");
    for name in ["Alpha.txt", "beta.txt", "Gamma.md"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");
    assert_eq!(app.visible_count().await, 3);

    // 小写 "b" 应定位到大写开头的 Beta.txt（排在第三位），而非隐藏其它文件。
    let row = app.focus_by_prefix("b").await.expect("应能定位到匹配项");
    let selected = app.selection_ids().await;
    assert_eq!(selected.len(), 1, "定位应单选一个文件");
    assert_eq!(app.visible_count().await, 3, "定位不应收窄列表");

    // 选中的应当就是 Beta.txt。
    let entries = app.current_entries().await;
    let sel_name = entries
        .iter()
        .find(|e| e.id == selected[0])
        .expect("选中项应在可见条目里")
        .name
        .clone();
    assert_eq!(sel_name, "beta.txt");

    // 继续输入形成 "beta"，仍是同一个目标，行号不变。
    let row2 = app.focus_by_prefix("beta").await.expect("应能定位");
    assert_eq!(row, row2, "同一目标，行号应一致");

    // 无匹配时返回 None，且不改变已有的选择。
    assert!(app.focus_by_prefix("zzz").await.is_none());
    assert_eq!(app.selection_ids().await.len(), 1);
}
