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

/// 选中项的**名字**（拿 app 侧选择快照回查条目）。
async fn selected_name(app: &AppState) -> String {
    let ids = app.selection_ids().await;
    assert_eq!(ids.len(), 1, "定位应单选一个文件");
    app.current_entries()
        .await
        .iter()
        .find(|e| e.id == ids[0])
        .expect("选中项应在可见条目里")
        .name
        .clone()
}

/// 连按同一个字母跳到**下一个**匹配项（Finder / 资源管理器习惯），转一圈回到起点。
#[tokio::test]
async fn repeating_one_letter_jumps_to_the_next_match() {
    let dir = tmp("typeahead-repeat");
    for name in ["apple.txt", "apricot.md", "avocado.rs", "banana.txt"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");

    // 首个字母从第一个匹配项开始（不跳过）。
    let h1 = app.locate_by_prefix("a", false).await.expect("应命中");
    assert_eq!(selected_name(&app).await, "apple.txt");

    // 之后再按同一个字母：从当前焦点之后找，依次落到第二、第三个匹配项。
    let h2 = app.locate_by_prefix("a", true).await.expect("应命中第二个");
    assert_eq!(selected_name(&app).await, "apricot.md");
    assert_ne!(h1.pos, h2.pos, "连按同字母应换一个目标");
    app.locate_by_prefix("a", true).await.expect("应命中第三个");
    assert_eq!(selected_name(&app).await, "avocado.rs");

    // 环绕：三个 a 开头的都轮过之后回到第一个。
    let h4 = app
        .locate_by_prefix("a", true)
        .await
        .expect("应环绕回第一个");
    assert_eq!(selected_name(&app).await, "apple.txt");
    assert_eq!(h4.pos, h1.pos, "转一圈应回到起点");
}

/// 严格前缀全军覆没时退到子序列匹配（`mt` → `Mars.txt`）。
#[tokio::test]
async fn subsequence_fallback_locates_when_no_prefix_matches() {
    let dir = tmp("typeahead-subseq");
    for name in ["Mars.txt", "zebra.log"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");

    // "mt" 不是任何文件的前缀，但按序出现在 Mars.txt 里。
    app.locate_by_prefix("mt", false)
        .await
        .expect("子序列兜底应命中 Mars.txt");
    assert_eq!(selected_name(&app).await, "Mars.txt");

    // 连子序列都不匹配时仍然是 None。
    assert!(app.locate_by_prefix("zz", false).await.is_none());
}

/// 前缀命中时**不**走子序列兜底：有 `dm.txt` 就不该跳到 `Documents`。
#[tokio::test]
async fn prefix_match_wins_over_subsequence() {
    let dir = tmp("typeahead-prefix-wins");
    std::fs::create_dir_all(dir.join("Documents")).expect("建子目录失败");
    std::fs::write(dir.join("dm.txt"), b"x").unwrap();

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");

    app.locate_by_prefix("dm", false)
        .await
        .expect("前缀匹配应命中");
    assert_eq!(selected_name(&app).await, "dm.txt");
}

/// 方向键定位返回两种空间的下标（无分组时条目位 = 列表行）。
#[tokio::test]
async fn cursor_move_reports_both_spaces() {
    let dir = tmp("typeahead-cursor");
    for name in ["one.txt", "two.txt", "three.txt"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");

    let hit = app.locate_cursor(1, false).await.expect("应有焦点");
    assert_eq!(hit.pos, 0, "无焦点时 ↓ 应聚焦第一项");
    assert_eq!(hit.row, 0, "无分组时列表行与条目位相等");
    let hit = app.locate_cursor(1, false).await.expect("应有焦点");
    assert_eq!(hit.pos, 1);
    assert_eq!(hit.row, 1);
    // 薄壳 `move_cursor` 仍是列表行（旧调用点 / 测试的契约不变）。
    let row = app.move_cursor(1, false).await.expect("应有焦点");
    assert_eq!(row, 2, "move_cursor 返回列表行");
}
