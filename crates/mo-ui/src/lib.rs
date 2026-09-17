//! mo-ui：Mo 的图形界面层（基于 GPUI Kit）。
//!
//! 这一层才使用 GPUI；核心逻辑全部在 `mo-core` / `mo-app` 中，UI 只通过
//! [`mo_app::AppState`] 发命令、通过事件总线订阅变化，不直接操作文件系统。

mod app;
mod breadcrumb;
mod file_item;
mod file_list;
mod icon;
mod progress_panel;
mod sidebar;
mod status_bar;
mod theme;
mod toolbar;

pub use app::RootView;

use gpui_kit::*;
use mo_app::AppState;

/// 启动 Mo 图形界面。
pub fn run() {
    let app = AppState::new();
    // 启动文件监听泵：外部程序增删改文件时做增量更新并广播事件。
    app.spawn_watcher_pump();
    // 启动刷新泵：把密集的元数据 / 缩略图回填合并成节拍性的 UI 刷新。
    app.spawn_refresh_pump();
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        // NSApplication 此时已创建：把嵌入的 PNG 设为 Dock / ⌘Tab 图标。
        icon::set_dock_icon();
        let app = app.clone();
        cx.spawn(async move |cx| {
            let _window = cx
                .open_window(gpui_kit::WindowOptions::default(), move |_, cx| {
                    cx.new(|cx| RootView::new(app.clone(), cx))
                })
                .expect("failed to open window");
        })
        .detach();
    });
}
