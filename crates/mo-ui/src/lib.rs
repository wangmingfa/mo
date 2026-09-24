//! mo-ui：Mo 的图形界面层（基于 GPUI Kit）。
//!
//! 这一层才使用 GPUI；核心逻辑全部在 `mo-core` / `mo-app` 中，UI 只通过
//! [`mo_app::AppState`] 发命令、通过事件总线订阅变化，不直接操作文件系统。

mod app;
/// 内存位图 → gpui 渲染图的转换缓存（`ImageSource::Render` 同步上屏的根）。
mod bitmap;
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
mod path_label;
mod preview;
mod progress_panel;
mod sidebar;
/// 暂存区抽屉（跨目录收集待处理文件）。
mod staging;
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

/// 测试专用：向**当前标签页**注入假的操作快照（传输小块 / 浮层的数据源）。
///
/// 真操作要走 `OperationManager` 的注册 / 执行链路，headless 测试里拉不动整条
/// 后台管线；`Panel.ops` 本来就是 UI 侧的快照缓存，直接塞进去即可渲染验证。
#[doc(hidden)]
pub fn inject_ops_for_tests(view: &mut RootView, ops: Vec<mo_operations::OperationHandle>) {
    if let Some(p) = view.panel_at_mut(0, 0) {
        p.ops = ops;
    }
}

/// 测试专用：直接置传输浮层的开合状态（点击小块在 headless 里不好模拟）。
#[doc(hidden)]
pub fn set_ops_open_for_tests(view: &mut RootView, open: bool) {
    view.ops_open = open;
}

/// 测试专用：注入假的回收站条目（回收站面板的数据源）。
///
/// 真条目要走 `Trash` 的索引文件与删除链路，headless 拉不动；字段直接置上
/// 即可渲染验证（行高 / 侧栏保留 / 选中样式都靠它）。
#[doc(hidden)]
pub fn inject_trash_for_tests(view: &mut RootView, entries: Vec<mo_operations::TrashEntry>) {
    view.trash_entries = entries;
}

/// 测试专用：读回收站面板状态——`(条目数, 是否停在确认卡上)`。
#[doc(hidden)]
pub fn trash_panel_state_for_tests(view: &RootView) -> (usize, bool) {
    (
        view.trash_entries.len(),
        matches!(view.modal, crate::app::Modal::ConfirmTrash(_)),
    )
}

/// 测试专用：快速预览窗口是否开着（空格预览回收站条目后置位）。
#[doc(hidden)]
pub fn trash_preview_open_for_tests(view: &RootView) -> bool {
    view.preview_window.is_some()
}

/// 测试专用：驱动当前标签页真实导航到 `dir`。
///
/// headless 里自己铺窗口快照撑不住——Home 目录的元数据回填等后台事件随时会
/// 触发 `sync_panel`，把注入的假行当「旧目录窗口」作废清掉。干脆走真链路：
/// `DirectoryController::open` 发事件 → 标签页的订阅循环自动 sync + 补窗，
/// 测试侧轮询行出现即可（`allow_parking` 下外部 IO 完成会唤醒测试执行器）。
#[doc(hidden)]
pub fn navigate_for_tests(
    view: &mut RootView,
    dir: std::path::PathBuf,
    cx: &mut gpui_kit::Context<RootView>,
) {
    let Some(p) = view.panel_at_mut(0, 0) else {
        return;
    };
    let app = p.app.clone();
    cx.spawn(async move |_this, _cx| {
        let _ = mo_app::DirectoryController::new(app).open(&dir).await;
    })
    .detach();
}

/// 测试专用：读当前标签页的本地选中数（空白点击语义断言用）。
#[doc(hidden)]
pub fn panel_selection_count_for_tests(view: &RootView) -> usize {
    view.panel_at(0, 0)
        .map(|p| p.selection.count())
        .unwrap_or(0)
}

/// 测试专用：注入一棵假的磁盘地图树并切到地图视图。
///
/// 真的树要递归扫几万个文件（headless 里拉不动，也不该在测试里扫真盘），
/// 而 layout 的正确性由 `mo-app` 的单测守着——这里只验证「画出来了、面积对」。
#[doc(hidden)]
pub fn inject_usage_tree_for_tests(view: &mut RootView, tree: mo_app::UsageTree) {
    view.usage_root = Some(tree.path.clone());
    view.usage_tree = Some(tree);
    view.usage_map = true;
    view.modal = app::Modal::DiskUsage;
}

/// 测试专用：打开分栏对比的图例条（只给统计，不真扫盘）。
///
/// 真的 `compare_trees` 要递归扫两棵树；「两侧该怎么染」的语义由
/// `app::tests::compare_maps_*` 守着，这里只验证图例条真进了布局。
#[doc(hidden)]
pub fn inject_compare_for_tests(view: &mut RootView, stats: (usize, usize, usize)) {
    view.compare = true;
    view.compare_stats = Some(stats);
}

/// 测试专用：直接打开内容搜索面板（不依赖当前目录，范围写死 `/`）。
///
/// 真的 `open_content_search` 从当前面板取路径；布局测试只验证「面板长什么样」，
/// 不关心范围，故这里直接给定。结果区由 `mo-search` 单测守着，本测试只看骨架。
#[doc(hidden)]
pub fn inject_content_search_for_tests(view: &mut RootView) {
    view.content_root = Some(std::path::PathBuf::from("/"));
    view.content_report = None;
    view.content_index = 0;
    view.content_dirty = true;
    view.modal = app::Modal::ContentSearch;
}

/// 测试专用：当前标签页的窗口快照是否已同步到 `rows` 行（导航等待用）。
#[doc(hidden)]
pub fn panel_window_ready_for_tests(view: &RootView, rows: usize) -> bool {
    view.panel_at(0, 0)
        .is_some_and(|p| p.list_count == rows && p.window.len() == rows)
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
    // 全局搜索自举：索引现在落在磁盘上（`~/Library/Caches/mo/search.sqlite`），
    // 但**首次启动**仍要爬一遍主目录才有东西可搜。这里后台起，不挡窗口出现；
    // 之后每次启动只重爬「超过 6 小时没刷」的根（见 `ensure_index_started`）。
    app.ensure_index_started();
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
