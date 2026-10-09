//! 文件剪贴板（⌘C / ⌘X / ⌘V）的契约，含它与**系统**剪贴板之间那一手交接。
//!
//! 钉的是三件事：
//!
//! 1. 粘的永远是**剪贴板里那批**，不是「此刻选中的那批」——复制后换个目录再粘
//!    是日常操作，早先复制分支走的是当前选区，换完目录就粘出错的东西（曾经直接
//!    粘出 0 项）；
//! 2. 源在**哪一端**跟着剪贴板走：从资源管理器复制的一批本机路径，粘进正在浏览的
//!    远程会话时读的是本机（上传），拿当前端点当源就会去服务器上找一个不存在的文件；
//! 3. 系统剪贴板只在「换了一批」时才覆盖内部那批——Mo 自己复制时也会把同一批路径
//!    写进系统剪贴板，那种情况下内部记的 `cut` 才是权威。

use mo_app::AppState;
use std::path::{Path, PathBuf};

mod common;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-clip-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

/// 一份把缓存与配置都钉进隔离目录的 `AppState`（见 common::isolated）。
fn app(tag: &str) -> AppState {
    common::isolated(tag, || AppState::with_trash(tmp(&format!("{tag}-trash"))))
}

/// 等一个路径出现（传输是后台提交的操作，测试不能「等」，只能轮询）。
async fn wait_exists(path: &Path) -> bool {
    for _ in 0..150 {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

/// 复制 → 换目录 → 粘贴：粘的是**刚才复制的那两条**，与换目录后的选区无关。
#[tokio::test]
async fn copy_survives_the_selection_changing_underneath_it() {
    let src = tmp("xfer-src");
    let dst = tmp("xfer-dst");
    std::fs::write(src.join("a.txt"), b"a").unwrap();
    std::fs::write(src.join("b.txt"), b"b").unwrap();

    let app = app("copy-across-dirs");

    app.open_directory(&src).await.expect("打开源目录失败");
    app.select_all_visible().await;
    app.copy_selection_to_clipboard().await;

    // 换到另一个目录并全选：这里的选区是**空**的（目标目录还没有文件）。
    app.open_directory(&dst).await.expect("打开目标目录失败");
    app.select_all_visible().await;

    let ids = app.paste_clipboard(None).await;
    assert!(
        !ids.started_ids().is_empty(),
        "剪贴板里有两条，粘贴不该空手而归"
    );
    assert!(
        wait_exists(&dst.join("a.txt")).await && wait_exists(&dst.join("b.txt")).await,
        "复制的必须是剪贴板里那批，而不是当前选区"
    );
    assert!(
        src.join("a.txt").exists() && src.join("b.txt").exists(),
        "复制（非剪切）不该动源文件"
    );
}

/// 剪切 → 换目录 → 粘贴：搬走，而且**只能粘一次**（二次粘是空的）。
#[tokio::test]
async fn a_cut_is_consumed_by_the_first_paste() {
    let src = tmp("cut-src");
    let dst = tmp("cut-dst");
    std::fs::write(src.join("only.txt"), b"x").unwrap();

    let app = app("cut-consumed");

    app.open_directory(&src).await.expect("打开源目录失败");
    app.select_all_visible().await;
    app.cut_selection_to_clipboard().await;
    app.open_directory(&dst).await.expect("打开目标目录失败");

    app.paste_clipboard(None).await;
    assert!(
        wait_exists(&dst.join("only.txt")).await,
        "剪切后换目录也要粘得出来"
    );
    assert!(!src.join("only.txt").exists(), "剪切是移动，原处该空掉");
    assert!(
        app.paste_clipboard(None).await.started_ids().is_empty(),
        "剪切是一次性消耗品，第二次粘应当无事发生"
    );
}

/// 系统剪贴板里那批文件（资源管理器复制的）能直接粘进当前目录。
#[tokio::test]
async fn files_from_the_system_clipboard_land_in_the_current_dir() {
    let foreign = tmp("explorer-src");
    let here = tmp("explorer-here");
    let victim = foreign.join("from-explorer.txt");
    std::fs::write(&victim, b"copied in explorer").unwrap();

    let app = app("external-in");

    app.open_directory(&here).await.expect("打开当前目录失败");
    // UI 层从 gpui 读到 `ExternalPaths` 后递进来的就是这一手（路径 + 剪切位）。
    assert!(
        app.adopt_system_clipboard(vec![victim.clone()], false)
            .await,
        "内部剪贴板是空的，系统那批该被采纳"
    );
    app.paste_clipboard(None).await;
    assert!(
        wait_exists(&here.join("from-explorer.txt")).await,
        "外部复制的文件要落到当前目录"
    );
    assert!(victim.exists(), "复制语义：外部那份原文件不动");
}

/// 系统剪贴板里同一批路径**不该**覆盖内部那批：`cut` 以内部为准。
///
/// Mo 自己剪切时也会把同一批路径写进系统剪贴板（Windows 上就是 `CF_HDROP`），
/// 于是粘贴时两边路径一模一样。此时 macOS 那边读不出剪切位，内部记的 `true`
/// 是唯一可信的答案——被覆盖成「复制」就会把源文件留成双份。
#[tokio::test]
async fn the_same_batch_on_the_system_clipboard_does_not_overrule_the_cut_flag() {
    let src = tmp("own-src");
    let dst = tmp("own-dst");
    let one = src.join("mine.txt");
    std::fs::write(&one, b"x").unwrap();

    let app = app("own-cut-wins");

    app.open_directory(&src).await.expect("打开源目录失败");
    app.select_all_visible().await;
    app.cut_selection_to_clipboard().await;

    // 系统剪贴板回报同一批路径，但剪切位读不出来（按复制答）。
    assert!(
        !app.adopt_system_clipboard(vec![one.clone()], false).await,
        "同一批路径不该覆盖内部剪贴板"
    );

    app.open_directory(&dst).await.expect("打开目标目录失败");
    app.paste_clipboard(None).await;
    assert!(wait_exists(&dst.join("mine.txt")).await, "仍然要粘出来");
    assert!(
        !one.exists(),
        "内部记的是剪切：粘完原处必须消失，不能被外部的「复制」翻案"
    );
}

/// 别的应用换了一批路径 → 内部那批旧的作废。
#[tokio::test]
async fn a_different_batch_on_the_system_clipboard_replaces_the_internal_one() {
    let stale_src = tmp("stale-src");
    let new_src = tmp("new-src");
    let here = tmp("stale-here");
    std::fs::write(stale_src.join("stale.txt"), b"old").unwrap();
    std::fs::write(new_src.join("fresh.txt"), b"new").unwrap();

    let app = app("external-wins");

    app.open_directory(&stale_src).await.expect("打开目录失败");
    app.select_all_visible().await;
    app.copy_selection_to_clipboard().await;

    assert!(
        app.adopt_system_clipboard(vec![new_src.join("fresh.txt")], false)
            .await,
        "换了一批路径，系统剪贴板说了算"
    );
    app.open_directory(&here).await.expect("打开目录失败");
    app.paste_clipboard(None).await;

    assert!(
        wait_exists(&here.join("fresh.txt")).await,
        "粘出来的该是系统那批"
    );
    assert!(
        !here.join("stale.txt").exists(),
        "内部那批旧的该作废，不该跟着粘出来"
    );
}

// ---- 剪贴板历史（被动记录最近 N 批，与暂存区的分工见 `mo_app::ClipHistoryEntry`）----

/// 每次复制留一条，**最近在前**；连着复制同一批只留一条。
#[tokio::test]
async fn history_records_each_copy_newest_first() {
    let dir = tmp("hist-record");
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    std::fs::write(dir.join("b.txt"), b"b").unwrap();

    let app = app("hist-record");
    app.open_directory(&dir).await.expect("打开目录失败");

    app.select_all_visible().await;
    app.copy_selection_to_clipboard().await;
    // 只选一条再复制（选区模型按路径选，这里直接换一批路径）。
    app.clear_selection().await;
    app.select_path(&dir.join("b.txt")).await;
    app.copy_selection_to_clipboard().await;

    let h = app.clipboard_history().await;
    assert_eq!(h.len(), 2, "两次复制该留两条：{:?}", h.len());
    assert_eq!(h[0].paths, vec![dir.join("b.txt")], "最新的在前");
    assert!(!h[0].cut);
    assert!(h.iter().all(|e| e.at > 0), "每条都该带时间");
}

/// 连着两次复制同一批：历史里只留一条（否则「刚才那批」被挤到第二行）。
#[tokio::test]
async fn repeating_the_same_copy_does_not_add_a_second_entry() {
    let dir = tmp("hist-dup");
    std::fs::write(dir.join("a.txt"), b"a").unwrap();

    let app = app("hist-dup");
    app.open_directory(&dir).await.expect("打开目录失败");
    for _ in 0..3 {
        app.select_all_visible().await;
        app.copy_selection_to_clipboard().await;
    }
    assert_eq!(
        app.clipboard_history().await.len(),
        1,
        "同一批连按三次只该留一条"
    );
}

/// 采纳**系统**剪贴板不进历史：那会随每次前台切换反复触发，几秒就占满 20 条。
#[tokio::test]
async fn adopting_the_system_clipboard_stays_out_of_history() {
    let dir = tmp("hist-sys");
    std::fs::write(dir.join("a.txt"), b"a").unwrap();

    let app = app("hist-sys");
    app.open_directory(&dir).await.expect("打开目录失败");
    app.adopt_system_clipboard(vec![dir.join("a.txt")], false)
        .await;
    assert!(
        app.clipboard_history().await.is_empty(),
        "外部那批不该进历史"
    );
}

/// 从历史里粘**第二条**（更早那批）：粘的是那一批自己，不是当前剪贴板那条。
#[tokio::test]
async fn pasting_an_older_history_entry_pastes_that_batch() {
    let src = tmp("hist-paste-src");
    let dst = tmp("hist-paste-dst");
    std::fs::write(src.join("older.txt"), b"o").unwrap();
    std::fs::write(src.join("newer.txt"), b"n").unwrap();

    let app = app("hist-paste");
    app.open_directory(&src).await.expect("打开源目录失败");
    app.select_all_visible().await;
    app.copy_selection_to_clipboard().await; // 第 0 条（等下会被顶到第二）
    app.clear_selection().await;
    app.select_path(&src.join("newer.txt")).await;
    app.copy_selection_to_clipboard().await; // 现在最新是 newer

    let h = app.clipboard_history().await;
    assert_eq!(h.len(), 2);

    app.open_directory(&dst).await.expect("打开目标目录失败");
    // 第二条 = 更早就复制的那批（含 older.txt）。
    let ids = app.paste_history_entry(1).await;
    assert!(!ids.started_ids().is_empty(), "历史里有条目，粘贴不该空手");
    assert!(
        wait_exists(&dst.join("older.txt")).await,
        "粘的该是历史里那一批"
    );
}
