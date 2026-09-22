//! mo-ui：Mo 的图形界面层（基于 GPUI Kit）。
//!
//! 这一层才使用 GPUI；核心逻辑全部在 `mo-core` / `mo-app` 中，UI 只通过
//! [`mo_app::AppState`] 发命令、通过事件总线订阅变化，不直接操作文件系统。

mod app;
mod columns;
mod context_menu;
mod dialogs;
mod file_item;
mod file_list;
mod grid;
mod icon;
mod icons;
mod keys;
mod list_columns;
mod listing;
mod panel;
mod preview;
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

/// 测试专用：把配置目录钉到进程唯一的临时目录。
///
/// ⚠️ 凡是会建 `AppState` / `RootView` 的测试都要在开头调一次：视图模式、
/// 侧边栏开关这些布局偏好会改变渲染结构，不隔离就会读到**开发者机器上的真实
/// 配置**，于是同一份代码在别人机器上跑测试结论不同（表现是断言莫名失败）。
#[doc(hidden)]
pub fn isolate_config_for_tests() -> std::path::PathBuf {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let p = std::env::temp_dir().join(format!("mo-test-config-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p
    });
    std::env::set_var("MO_CONFIG_DIR", dir);
    dir.clone()
}

/// 启动 Mo 图形界面。
pub fn run() {
    init_tracing();
    let app = AppState::new();
    // 启动文件监听泵：外部程序增删改文件时做增量更新并广播事件。
    app.spawn_watcher_pump();
    // 启动刷新泵：把密集的元数据 / 缩略图回填合并成节拍性的 UI 刷新。
    app.spawn_refresh_pump();
    // 启动图标泵：列表行要的系统图标只在渲染路径上「记账」，真去问系统（AppKit +
    // 重绘 + 编码 + 写盘，一张 1.5–12ms）在这里的后台批里做。
    app.spawn_icon_pump();
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        // 主 run loop 已经跑起来了：允许后台任务把 AppKit 调用 `dispatch_sync`
        // 回主队列（图标泵就靠它）。测试进程永远走到不这里，所以那边一律保守跳过。
        mo_platform::mark_main_loop_ready();
        // 框架组件（地址栏的 Input）的配色对齐到当前主题（`RootView::new` 里
        // 先按配置套用主题，这里再同步一次给全局 Theme）。
        theme::apply_component(cx);
        // NSApplication 此时已创建：把嵌入的 PNG 设为 Dock / ⌘Tab 图标。
        icon::set_dock_icon();
        let app = app.clone();
        // 沉浸式交通灯：隐藏系统标题栏（appears_transparent），内容延伸到窗口顶部，
        // 工具栏左移留出红绿灯位置。红绿灯垂直位置由 toolbar::TOOLBAR_HEIGHT
        // 推导，保证在工具栏内居中。
        // 拖拽：macOS 开 app_owns_titlebar_drag 把 AppKit 请出标题栏（消掉它的
        // 双击判定延迟），改由整条顶栏自营 —— 见 toolbar::attach_titlebar_drag。
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
            // macOS 让 AppKit 彻底退出标题栏：原生实现在按下时会先等一拍判断
            // 是不是双击（决定要不要缩放），那一拍就是点标签时的迟滞感。
            // 代价是拖拽与双击缩放改由应用层负责 —— 见
            // `toolbar::attach_titlebar_drag`（挂在整条顶栏上）。
            app_owns_titlebar_drag: true,
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
