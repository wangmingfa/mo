//! mo-ui：Mo 的图形界面层（基于 GPUI Kit）。
//!
//! 这一层才使用 GPUI；核心逻辑全部在 `mo-core` / `mo-app` 中，UI 只通过
//! [`mo_app::AppState`] 发命令、通过事件总线订阅变化，不直接操作文件系统。

mod app;
mod columns;
mod dialogs;
mod file_item;
mod file_list;
mod grid;
mod icon;
mod icons;
mod listing;
mod panel;
mod progress_panel;
mod sidebar;
mod status_bar;
mod theme;
/// 公开仅为测试读取 `TOOLBAR_HEIGHT`（红绿灯居中的依据）。
pub mod toolbar;

pub use app::RootView;

use gpui_kit::*;
use mo_app::AppState;

/// 初始化日志：默认 info 级别输出到 stderr，可用 `RUST_LOG` 覆盖
/// （如 `RUST_LOG=mo_ui=debug` 诊断窗口懒加载）。需重定向时 `2> /tmp/mo.log`。
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .init();
}

/// 启动 Mo 图形界面。
pub fn run() {
    init_tracing();
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
        // 沉浸式交通灯：隐藏系统标题栏（appears_transparent），内容延伸到窗口顶部，
        // 工具栏左移留出红绿灯位置；AppKit 仍负责顶部条拖拽与双击缩放。
        // 红绿灯垂直位置由 toolbar::TOOLBAR_HEIGHT 推导，保证在工具栏内居中。
        let (tl_x, tl_y) = toolbar::traffic_light_position();
        // 默认尺寸 1280×800，且在屏幕上居中（gpui 缺省 1000×600、左上角显示）。
        let options = gpui_kit::WindowOptions {
            window_bounds: Some(gpui_kit::WindowBounds::centered(
                size(px(1280.0), px(800.0)),
                cx,
            )),
            titlebar: Some(gpui_kit::TitlebarOptions {
                title: Some("Mo".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(tl_x), px(tl_y))),
            }),
            ..Default::default()
        };
        cx.spawn(async move |cx| {
            let _window = cx
                .open_window(options, move |_, cx| {
                    cx.new(|cx| RootView::new(app.clone(), cx))
                })
                .expect("failed to open window");
        })
        .detach();
    });
}
