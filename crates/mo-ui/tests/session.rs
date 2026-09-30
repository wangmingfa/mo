//! 会话恢复：重开 Mo 回到上次那些标签页。
//!
//! 引擎侧（存什么、读什么）由 `mo-app` 的 `tests/session.rs` 守着；这里守
//! **UI 这一半**：
//!
//! * 恢复出来的**结构**：几个窗格、各几个标签页、分栏在不在。这一段是同步
//!   完成的（`restore_session` 当场建好 panes），导航才是异步的，所以它是
//!   确定的、不靠等待；
//! * 快照的形状（本机标签页记路径、不记端点）。远程那一半要真连一次才有
//!   `remote_url`，headless 里拉不起来，由 `mo-app` 的往返测试守着。
//!
//! ⚠️ 本文件**只开一个窗口**：会话写在一个进程级共享的隔离目录里
//! （`isolate_user_dirs_for_tests` 按 pid 定目录），同进程里两个窗口会互相
//! 覆盖种子（第一个窗口的去抖写盘会落在第二个窗口的种子上）。

use gpui_kit::test::TestWindowExt;
use gpui_kit::{px, size, TestAppContext, VisualTestContext, WindowHandle};
use mo_app::{AppState, SavedTab, Session};
use mo_ui::RootView;

fn local(path: &str) -> SavedTab {
    SavedTab {
        path: Some(path.to_string()),
        endpoint: None,
        remote_path: None,
    }
}

/// 在**预种了会话**的隔离目录里开一个窗口（会话写在 `MO_CONFIG_DIR` 下，
/// `RootView::new` 开局就会去读它）。
fn open_with_session(
    session: Session,
    cx: &mut TestAppContext,
) -> (VisualTestContext, WindowHandle<RootView>) {
    // 真 IO（`open_local` 走 spawn_blocking）与确定性调度器的固有冲突，
    // layout 那套测试的开局豁免，这里同理。
    cx.dispatcher.allow_parking();
    mo_ui::isolate_user_dirs_for_tests();
    let app = AppState::new();
    app.save_session(&session);
    let window = cx.open_window(size(px(1000.), px(700.)), move |_, cx| {
        RootView::new(app.clone(), cx)
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    (vcx, window)
}

/// 存了几个标签页就恢复出几个（不是只拿第一个），并且如实拍得回去。
#[gpui_kit::test]
fn restore_brings_back_every_saved_tab(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mo-session-restore-{}", std::process::id()));
    let a = dir.join("a");
    let b = dir.join("b");
    std::fs::create_dir_all(&a).expect("建 a");
    std::fs::create_dir_all(&b).expect("建 b");

    let (_, window) = open_with_session(
        Session {
            panes: vec![vec![
                local(&a.display().to_string()),
                local(&b.display().to_string()),
            ]],
            // 第二个是当前页。
            active_tabs: vec![1],
            active_pane: 0,
            split: false,
            ..Session::default()
        },
        cx,
    );

    let (counts, split) = window
        .update(cx, |root, _window, _cx| {
            mo_ui::pane_tab_counts_for_tests(root)
        })
        .expect("读窗格结构失败");
    assert_eq!(counts, vec![2], "一个窗格里的两个标签页都要回来");
    assert!(!split, "没存分栏就不该分栏");

    let snap = window
        .update(cx, |root, _window, _cx| {
            mo_ui::session_snapshot_for_tests(root)
        })
        .expect("拍快照失败");
    assert_eq!(snap.panes.len(), 1, "一个窗格");
    assert_eq!(snap.panes[0].len(), 2, "两个标签页");
    assert_eq!(snap.active_tabs, vec![1], "当前页还是存进去的那一个");
    for tab in &snap.panes[0] {
        assert_eq!(tab.endpoint, None, "本机标签页不该记端点");
        // 导航是异步的，路径可能还没落地；落地了就必须是存进去的那一个
        // （路径取自 `Panel::path`，不是 AppState 的另一份副本）。
        if let Some(p) = &tab.path {
            let p = std::path::PathBuf::from(p);
            assert!(
                p == a || p == b,
                "记的是存进去的目录之一：{p:?} 不在 {a:?} / {b:?}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
