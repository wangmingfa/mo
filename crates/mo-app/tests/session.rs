//! 会话（窗口布局）的持久化：`AppState::save_session` / `load_session`。
//!
//! 重开 Mo 要回到上次那些标签页，靠的就是这份快照。这里钉两件事：
//!
//! * **往返一致**（尤其远程标签页的端点——它跟路径是两回事，记错就永远恢复不回去）；
//! * **不进 config.json**：改设置与写会话是两条独立的写路径，共用一份文件就会
//!   互相抹掉（先读整份 → 改一个字段 → 写回）。
//!
//! ⚠️ 整段都塞进一次 [`common::isolated`]：`AppState::config_path()` 是**每次
//! 调用现读** `MO_CONFIG_DIR` 的，而隔离只保护「改变量 + 当场构造」那一步——
//! 放到外面就会读到并行用例刚改走的目录（单跑绿、连跑红，见 common 的模块文档）。

use mo_app::{AppState, SavedTab, Session};

mod common;

fn local(path: &str) -> SavedTab {
    SavedTab {
        path: Some(path.to_string()),
        endpoint: None,
        remote_path: None,
    }
}

/// 存进去再读出来是同一份；空会话写进去读回来仍是空的（别把「没有标签页」
/// 变成「一个空窗格」）。
#[test]
fn session_round_trips_through_the_isolated_dir() {
    common::isolated("session-rt", || {
        let app = AppState::new();
        // 开局：这个隔离目录里没有会话。
        assert!(app.load_session().is_empty(), "首次启动没有会话");

        let s = Session {
            panes: vec![
                vec![local("/tmp/mo-a")],
                vec![
                    local("/tmp/mo-b"),
                    SavedTab {
                        path: None,
                        endpoint: Some("ftp://example.org".to_string()),
                        remote_path: Some("/pub".to_string()),
                    },
                ],
            ],
            active_tabs: vec![0, 1],
            active_pane: 1,
            split: true,
        };
        app.save_session(&s);
        assert_eq!(app.load_session(), s, "往返要一致（含远程端点与远端路径）");

        app.save_session(&Session::default());
        assert!(app.load_session().is_empty(), "空会话存进去读回来仍是空");
    });
}

/// 会话**不写进** config.json：写会话不该把设置抹回去。
#[test]
fn session_does_not_disturb_the_settings_file() {
    common::isolated("session-split", || {
        let app = AppState::new();
        // 改一项界面偏好（走 set_ui_prefs 落盘），再写会话，看它还在不在。
        let mut ui = app.ui_prefs();
        ui.zebra = !ui.zebra;
        app.set_ui_prefs(ui.clone());

        app.save_session(&Session {
            panes: vec![vec![local("/tmp/mo-c")]],
            active_tabs: vec![0],
            active_pane: 0,
            split: false,
        });

        assert_eq!(app.ui_prefs(), ui, "写会话不该把界面偏好抹回去");
    });
}
