//! headless 布局验证（不需要真实窗口 / GPU）。
//!
//! 这一层专门守住两个 GPUI 坑，它们都曾让界面「一片空白」：
//!
//! 1. **`flex_row()` / `flex_col()` 不设置 `display`**。
//!    GPUI 的 `Style::default().display` 是 `Block`，而 `flex_row()` / `flex_col()`
//!    只设置 `flex-direction`。不额外调用 `.flex()`，容器就是 block——
//!    `flex_1()` / `items_center()` / `gap()` / `justify_*` 全部静默失效。
//!    （gpui-component 的 `h_flex()` / `v_flex()` 会一并设置，但 gpui 0.3.5 没有。）
//! 2. **`uniform_list` 不会自己撑高**。它的行只在 prepaint 阶段渲染，布局阶段
//!    taffy 看到的是一个没有子节点的元素，身高算出来是 0，必须由调用方给出
//!    确定高度（`flex_1()` / `size_full()` / `h(...)`）。
//!
//! `debug_selector` 只在 test / `test-support` 构建里生效，release 下是 no-op。

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    px, size, Bounds, InputEvent, Pixels, Size, TestAppContext, VisualTestContext, WindowHandle,
};
use mo_app::AppState;
use mo_ui::RootView;

/// 以指定窗口尺寸启动一个 headless 窗口，返回可查询布局的上下文与窗口句柄。
fn open_app(
    window_size: Size<Pixels>,
    cx: &mut TestAppContext,
) -> (VisualTestContext, WindowHandle<RootView>) {
    open_app_with_trash(
        window_size,
        std::env::temp_dir().join(format!("mo-layout-trash-{}", std::process::id())),
        cx,
    )
}

/// 同 [`open_app`]，但回收站根由调用方指定——需要**预种** index.json 的回收站
/// 测试各用独立目录（同测试进程内并行跑的用例共享 pid，共用一个根会互相
/// `remove_dir_all` / 重写 index.json，实测把对方落点文件删到预览读不出）。
fn open_app_with_trash(
    window_size: Size<Pixels>,
    trash_root: std::path::PathBuf,
    cx: &mut TestAppContext,
) -> (VisualTestContext, WindowHandle<RootView>) {
    // 钉住配置与缓存目录：前者不隔离会读到人家的真实 config.json（视图模式 / 侧边栏
    // 开关会改变渲染结构），后者不隔离会把这些测试爬过的目录写进真实 `search.sqlite`。
    mo_ui::isolate_user_dirs_for_tests();
    // ⚠️ `AppState::new()` 会经 `spawn_blocking` 异步读真实的 Home 目录，读完由 tokio
    // 的 worker 线程唤醒 GPUI 任务。gpui 的 TestScheduler 默认把「外部线程唤醒本地
    // 任务」判成 `not deterministic`，并在测试收尾（`end_test`）时 panic——高负载 /
    // 并发下一撞一个准（实测 4 并发 × 3 波里 5/12 次命中，见 `assert_correct_thread`）。
    // 这不是被测代码的问题，是「真 IO + 确定性调度器」混搭的固有冲突，所以开一次
    // 官方的豁免开关 `allow_parking`：
    //   * 它把 `parking_allowed_once` 置位（此后不复位），让那道线程检查直接 return；
    //   * 不改变 `run_until_parked`（就是 `while tick() {}`）的执行时序，也不碰任何
    //     布局断言——这些测试要守的仍然照守。
    // 少了这一句，`cargo test` 会随机红在「哪条 layout 测试」上，而与被测布局无关。
    cx.dispatcher.allow_parking();
    // 回收站根也要**隔离**：`AppState::new()` 用真实 `~/.mo-trash`，测试机上有
    // 真实条目时 `open_trash_panel` 一刷新就会把注入的假条目顶掉（曾让回收站
    // 相关断言靠机器状态侥幸通过）。
    let app = AppState::with_trash(trash_root);
    let window = cx.open_window(window_size, move |_, cx| RootView::new(app.clone(), cx));
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    // Home 目录是异步加载的；这里只关心布局，跑一轮让首帧画出来即可。
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    (vcx, window)
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} 没有出现在渲染帧里（debug_selector 未生效？）"))
}

/// 路径作为 JSON 字符串字面量的正文：Windows 的 `display()` 带 `\`，
/// 不转义成 `\\` 的话预种的 index.json 根本解析不出来（回收站面板空白）。
fn jpath(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

/// 键串里的主修饰键：macOS 写 `cmd`，别的平台写 `ctrl`。
/// （键表本身两个写法同义，但 headless 模拟把 `cmd` 解释成 platform 修饰位，
/// 非 macOS 的事件里那一位永远不亮，只有 `ctrl` 能命中。）
const PRIMARY: &str = if cfg!(target_os = "macos") {
    "cmd"
} else {
    "ctrl"
};

/// 「重命名」的平台默认键：Finder 惯例 Enter、资源管理器惯例 F2（见 keys.rs 的 `default_spec`）。
const RENAME_KEY: &str = if cfg!(target_os = "macos") {
    "enter"
} else {
    "f2"
};

/// 文件列表必须吃满中央区的剩余高度——它没有自我撑高的能力。
#[gpui_kit::test]
fn file_list_fills_the_central_area(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(700.)), cx);

    let list = bounds(&mut cx, "mo-file-list");

    assert!(
        list.size.height > px(300.),
        "文件列表只有 {} 高（窗口 700）：uniform_list 需要调用方给出确定高度",
        list.size.height
    );
    assert!(
        list.size.width > px(300.),
        "文件列表宽度异常：{}",
        list.size.width
    );
}

/// 高度应当由「窗口剩余空间」驱动，而不是某个写死的值。
#[gpui_kit::test]
fn file_list_height_tracks_the_window(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(400.)), cx);

    let list = bounds(&mut cx, "mo-file-list");

    assert!(
        list.size.height > px(200.),
        "窗口 400 高时文件列表高度只有 {}",
        list.size.height
    );
    assert!(
        list.size.height < px(400.),
        "文件列表高度 {} 超过了窗口高度：它没有让位于工具栏 / 状态栏",
        list.size.height
    );
}

/// 表头每两列之间都要有一条**可见**的分隔线，位置落在两列间隙的中线上。
///
/// 这条线同时是列宽的拖动把手：它必须真的画出来（曾经只有 1px 不可见的命中区），
/// 而且必须正好卡在列间隙里，不能压在列头文字上。
#[gpui_kit::test]
fn header_dividers_sit_between_columns(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(700.)), cx);

    let header = bounds(&mut cx, "mo-file-list-header");
    let center = |b: &Bounds<Pixels>| f32::from(b.origin.x) + f32::from(b.size.width) / 2.0;

    let name = bounds(&mut cx, "mo-header-cell-name");
    let date = bounds(&mut cx, "mo-header-cell-date");
    let size = bounds(&mut cx, "mo-header-cell-size");
    let kind = bounds(&mut cx, "mo-header-cell-kind");

    assert!(
        cx.debug_bounds("mo-header-divider-name").is_none(),
        "第一列左侧不该有分隔线"
    );

    // 默认列序：名称 | 修改日期 | 大小 | 种类 → 三条线。
    for (selector, line_selector, left, right) in [
        (
            "mo-header-divider-date",
            "mo-header-divider-line-date",
            name,
            date,
        ),
        (
            "mo-header-divider-size",
            "mo-header-divider-line-size",
            date,
            size,
        ),
        (
            "mo-header-divider-kind",
            "mo-header-divider-line-kind",
            size,
            kind,
        ),
    ] {
        let divider = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} 没有渲染：分隔线不可见就等于没有把手"));
        let gap_mid =
            (f32::from(left.origin.x) + f32::from(left.size.width) + f32::from(right.origin.x))
                / 2.0;
        assert!(
            (center(&divider) - gap_mid).abs() <= 0.51,
            "{selector} 不在两列间隙的中线上：divider={divider:?} left={left:?} right={right:?}"
        );
        assert!(
            divider.origin.y == header.origin.y
                && divider.size.height >= header.size.height - px(1.),
            "{selector} 的命中区没有贯穿表头高度（去掉底边框那 1px）：divider={divider:?} header={header:?}"
        );

        // 看得见的那条线体：上下要留出间距（不顶边），且在命中区内垂直居中。
        let line = cx
            .debug_bounds(line_selector)
            .unwrap_or_else(|| panic!("{line_selector} 没有渲染：分隔线不可见"));
        let top_inset = f32::from(line.origin.y - divider.origin.y);
        let bottom_inset =
            f32::from(divider.origin.y + divider.size.height - line.origin.y - line.size.height);
        assert!(
            top_inset >= 1.0 && bottom_inset >= 1.0,
            "{line_selector} 的线体顶边了（上下要留间距）：top={top_inset} bottom={bottom_inset}"
        );
        assert!(
            (top_inset - bottom_inset).abs() <= 0.51,
            "{line_selector} 的线体没有垂直居中：top={top_inset} bottom={bottom_inset}"
        );
    }
}

/// 侧边栏与文件列表必须左右并排（block 布局下它们会变成上下堆叠，列表随即被压成 0 高）。
#[gpui_kit::test]
fn sidebar_sits_left_of_the_file_list(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(700.)), cx);

    let sidebar = bounds(&mut cx, "mo-sidebar");
    let center = bounds(&mut cx, "mo-center");
    let list = bounds(&mut cx, "mo-file-list");
    let header = bounds(&mut cx, "mo-file-list-header");

    assert_eq!(sidebar.origin.x, px(0.), "侧边栏不在最左侧");
    assert_eq!(
        center.origin.x,
        sidebar.origin.x + sidebar.size.width,
        "中央区没有紧贴侧边栏右侧：center.x={} sidebar={:?}",
        center.origin.x,
        sidebar
    );
    assert_eq!(
        center.origin.y, sidebar.origin.y,
        "中央区与侧边栏没有落在同一行：center.y={} sidebar.y={}",
        center.origin.y, sidebar.origin.y
    );
    assert_eq!(
        center.size.height, sidebar.size.height,
        "中央区与侧边栏高度不一致"
    );
    // 列表视图 = 固定表头 + 滚动列表，二者合计吃满中央区
    // （过滤条未显示时）。表头不随内容滚动，所以列表本体比中央区矮一个表头高。
    assert_eq!(
        list.size.height + header.size.height,
        center.size.height,
        "表头+列表没有吃满中央区高度：list={list:?} header={header:?} center={center:?}"
    );
}

/// 顶部行（标签页条所在行）高度必须钉死在 `TOOLBAR_HEIGHT`——沉浸式红绿灯的垂直居中依赖它。
#[gpui_kit::test]
fn toolbar_height_is_pinned_for_traffic_lights(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(700.)), cx);

    let toprow = bounds(&mut cx, "mo-toprow");

    assert_eq!(
        toprow.origin.y,
        px(0.),
        "顶部行没有贴着窗口顶部（内容必须延伸到标题栏区域，红绿灯才能与标签页同行）"
    );
    assert_eq!(
        toprow.size.height,
        px(mo_ui::toolbar::TOOLBAR_HEIGHT),
        "顶部行高度漂移：红绿灯按 TOOLBAR_HEIGHT 居中，二者必须一致"
    );
}

/// 「＋」右侧那截空白必须能拖窗口。
///
/// 顶栏是自绘的 48px，而 macOS 的 AppKit 只认原生标题栏那一条带（≈28pt）；
/// 没有应用层拖拽带时，下半截谁都不管——标签右侧的空白就拖不动窗口。
/// 这里断言那条带子确实铺在「＋」右侧、且铺满整行高度（直到窗口右缘）。
#[gpui_kit::test]
fn titlebar_drag_filler_covers_the_area_right_of_new_tab(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(700.)), cx);

    let toprow = bounds(&mut cx, "mo-toprow");
    let new_tab = bounds(&mut cx, "mo-tab-new");
    let filler = bounds(&mut cx, "mo-titlebar-drag");

    assert!(
        filler.origin.x >= new_tab.origin.x + new_tab.size.width,
        "拖拽带压在了「＋」按钮上：filler={filler:?} new_tab={new_tab:?}"
    );
    assert_eq!(
        filler.origin.y, toprow.origin.y,
        "拖拽带不在顶栏里：filler={filler:?} toprow={toprow:?}"
    );
    assert_eq!(
        filler.size.height,
        toprow.size.height - px(1.),
        "拖拽带没有铺满顶栏内容高度（行高减掉底部那条 1px 分隔线），死区还剩一条：filler={filler:?} toprow={toprow:?}"
    );
    // macOS 顶栏右缘没有窗口控制按钮 → 必须一路铺到窗口右缘（右内边距也要吃掉）。
    if cfg!(target_os = "macos") {
        assert_eq!(
            filler.origin.x + filler.size.width,
            toprow.origin.x + toprow.size.width,
            "拖拽带没铺到窗口右缘，最右那条仍拖不动：filler={filler:?} toprow={toprow:?}"
        );
    }
}

/// 地址栏已合并进工具栏（Win11 风格单栏）——面包屑必须是工具栏内的一个元素。
#[gpui_kit::test]
fn address_bar_lives_inside_the_toolbar(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(700.)), cx);

    let toolbar = bounds(&mut cx, "mo-toolbar");
    let address = cx.debug_bounds("mo-address").expect("地址栏没有渲染");

    assert!(
        address.origin.y >= toolbar.origin.y
            && address.origin.y + address.size.height <= toolbar.origin.y + toolbar.size.height,
        "地址栏不在工具栏垂直范围内：toolbar={toolbar:?} address={address:?}"
    );
}

/// 刷新按钮排在地址栏**左边**，与 ←→↑ 同属导航那一组。
///
/// 钉的是**左右关系**而不是「按钮在不在」：地址栏是弹性的（`flex_1`），刷新挪到它
/// 右边时按钮自己的宽高一点没变，只有「谁在谁左边」能抓住。刷新是「重读当前目录」，
/// 与后退 / 前进 / 上一级同类，摆在一起才读得出是一组；地址栏右侧整块留给视图模式
/// 那一组（互斥、带外框，自成一体）。
#[gpui_kit::test]
fn refresh_sits_left_of_the_address_bar(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(700.)), cx);

    let parent = bounds(&mut cx, "mo-icon-nav-parent");
    let refresh = bounds(&mut cx, "mo-icon-nav-refresh");
    let address = bounds(&mut cx, "mo-address");

    assert!(
        refresh.origin.x >= parent.origin.x + parent.size.width,
        "刷新不在「上一级」右边：parent={parent:?} refresh={refresh:?}"
    );
    assert!(
        refresh.origin.x + refresh.size.width <= address.origin.x,
        "刷新没在地址栏左边：refresh={refresh:?} address={address:?}"
    );
}

/// 工具栏应当只有一行；状态栏应当贴着窗口底部，中央区正好顶到状态栏。
#[gpui_kit::test]
fn toolbar_is_one_row_and_status_bar_is_pinned_to_the_bottom(cx: &mut TestAppContext) {
    let (mut cx, _window) = open_app(size(px(1000.), px(700.)), cx);

    let toolbar = bounds(&mut cx, "mo-toolbar");
    let list = bounds(&mut cx, "mo-file-list");
    let status = bounds(&mut cx, "mo-statusbar");

    assert!(
        toolbar.size.height < px(100.),
        "工具栏高 {}：按钮被竖着堆成了多行（flex_row 没生效？）",
        toolbar.size.height
    );
    assert_eq!(
        status.origin.y + status.size.height,
        px(700.),
        "状态栏没有贴着窗口底部：{:?}",
        status
    );
    assert_eq!(
        list.origin.y + list.size.height,
        status.origin.y,
        "中央区与状态栏之间有多余空白：list={:?} status={:?}",
        list,
        status
    );
}

// ── 传输指示（左下角卡片 + 任务浮层）与回收站入口 ──────────────────────────

use mo_operations::OperationStatus;

/// 假操作：实现 `Operation` trait，只为种进真的 `OperationManager`。
///
/// 任务浮层必须走**真管线**验证：视图里的 `tab.ops` 是快照缓存，任何总线事件
/// 都会触发 `sync_panel` 用 manager 快照覆盖它——注入视图的假条目一拍就被冲掉
/// （实测踩到）。种进 manager 后，泵 / 事件驱动的同步行为与生产完全一致。
#[derive(Clone)]
struct FakeOp {
    id: u64,
    status: OperationStatus,
    progress: (u64, u64),
}

impl mo_operations::Operation for FakeOp {
    fn id(&self) -> u64 {
        self.id
    }
    fn describe(&self) -> String {
        format!("删除（回收站）/Users/demo/file-{}", self.id)
    }
    fn status(&self) -> OperationStatus {
        self.status
    }
    fn progress(&self) -> (u64, u64) {
        self.progress
    }
    fn cancel(&self) {}
    fn pause(&self) {}
    fn resume(&self) {}
    fn run(&self) -> Result<(), mo_core::MoError> {
        Ok(())
    }
}

/// 种一批假操作进当前标签页的 `OperationManager`（广播事件触发 `sync_panel`）。
fn seed_ops(window: &WindowHandle<RootView>, cx: &mut TestAppContext, ops: Vec<FakeOp>) {
    window
        .update(cx, |root, _window, _cx| {
            let app = mo_ui::app_state_for_tests(root);
            app.seed_ops_for_tests(
                ops.into_iter()
                    .map(|o| std::sync::Arc::new(o) as _)
                    .collect(),
            );
        })
        .expect("种入假操作失败");
}

/// 从 `OperationManager` 摘掉指定操作（广播事件触发 `sync_panel`）。
fn remove_ops(window: &WindowHandle<RootView>, cx: &mut TestAppContext, ids: &[u64]) {
    window
        .update(cx, |root, _window, _cx| {
            mo_ui::app_state_for_tests(root).remove_ops_for_tests(ids);
        })
        .expect("摘除假操作失败");
}

/// 种一批假操作并**等到任务卡片真的画出来**。
///
/// seed 的广播事件可能赶在 `tab_loop` 订阅总线之前发出去而丢失（open_home 的
/// 真 IO 还在路上），所以失败就重发——register 同 id 幂等（HashMap 覆盖）。
fn seed_ops_until_visible(
    vcx: &mut VisualTestContext,
    window: &WindowHandle<RootView>,
    cx: &mut TestAppContext,
    ops: &[FakeOp],
) {
    for _ in 0..20 {
        seed_ops(window, cx, ops.to_vec());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        if vcx.debug_bounds("mo-ops-badge").is_some() {
            return;
        }
    }
    panic!("种入操作后任务卡片始终没有出现（事件丢失且重试无效）");
}

/// 传输任务必须收成**左下角统一任务卡片**（常显长条），不能再是整条横幅把状态栏
/// 顶上去。
///
/// 卡片宽与侧栏同档、贴在状态栏上方，且状态栏高度不受任务影响。
#[gpui_kit::test]
fn transfers_render_as_a_corner_badge_above_the_status_bar(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    let baseline = bounds(&mut vcx, "mo-statusbar");

    seed_ops_until_visible(
        &mut vcx,
        &window,
        cx,
        &[
            FakeOp {
                id: 1,
                status: OperationStatus::Running,
                progress: (3, 10),
            },
            FakeOp {
                id: 2,
                status: OperationStatus::Completed,
                progress: (5, 5),
            },
        ],
    );

    let badge = bounds(&mut vcx, "mo-ops-badge");
    assert!(
        f32::from(badge.size.width) <= 310.0,
        "卡片宽 {}：又铺回横幅了",
        badge.size.width
    );
    let status = bounds(&mut vcx, "mo-statusbar");
    assert_eq!(
        status, baseline,
        "有任务之后状态栏位置/大小变了：卡片不该占布局"
    );
    assert!(
        f32::from(badge.origin.y) + f32::from(badge.size.height) <= f32::from(status.origin.y),
        "卡片应悬在状态栏上方：badge={badge:?} status={status:?}"
    );
    // 默认不展开浮层。
    assert!(vcx.debug_bounds("mo-ops-popover").is_none());
}

/// 聚合进度条必须收在卡片的**直边区**里：这个 fork 的 `div` 不把子元素裁进
/// 父级圆角（`overflow_hidden()` 不被消费），通栏贴边会从 8px 圆角底下戳出去
/// （用户截图：进度条跑到圆角外面）。修法是左右收进 10px（≥ 圆角半径）。
#[gpui_kit::test]
fn aggregate_bar_stays_inside_the_badge_corners(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    seed_ops_until_visible(
        &mut vcx,
        &window,
        cx,
        &[FakeOp {
            id: 1,
            status: OperationStatus::Running,
            progress: (3, 10),
        }],
    );

    let badge = bounds(&mut vcx, "mo-ops-badge");
    let bar = bounds(&mut vcx, "mo-ops-bar");
    let inset_l = f32::from(bar.origin.x - badge.origin.x);
    let inset_r = f32::from((badge.origin.x + badge.size.width) - (bar.origin.x + bar.size.width));
    assert!(
        inset_l >= 8.0 && inset_r >= 8.0,
        "进度条距卡片左右缘只有 {inset_l}/{inset_r}px：会从 8px 圆角底下戳出去"
    );
}

/// 点常显卡片在**上方**弹出任务浮层，再点一次收起——浮层贴卡片顶部展开、
/// 左缘对齐（top-start），不是把卡片原地长高。
#[gpui_kit::test]
fn badge_click_toggles_the_transfer_popover(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    seed_ops_until_visible(
        &mut vcx,
        &window,
        cx,
        &[FakeOp {
            id: 1,
            status: OperationStatus::Running,
            progress: (0, 4),
        }],
    );

    vcx.update(|window, cx| window.click("mo-ops-badge", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    let pop = bounds(&mut vcx, "mo-ops-popover");
    assert!(
        f32::from(pop.size.width) <= 400.0,
        "浮层宽 {}：不该铺满窗口",
        pop.size.width
    );
    // 浮层贴在卡片**上方**（top-start）：底缘不越过卡片顶、左缘对齐。
    let badge = bounds(&mut vcx, "mo-ops-badge");
    assert!(
        f32::from(pop.origin.y) + f32::from(pop.size.height) <= f32::from(badge.origin.y),
        "任务浮层应贴在卡片上方：pop={pop:?} badge={badge:?}"
    );
    assert_eq!(
        pop.origin.x, badge.origin.x,
        "任务浮层与常显卡片左缘不对齐：不是 top-start"
    );

    vcx.update(|window, cx| window.click("mo-ops-badge", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-ops-popover").is_none(),
        "再点一次卡片应收起任务浮层"
    );
}

/// 浮层右上角的扫帚：一键清除**已完成**的任务（进行中的原样保留）；
/// 任务全部移除后浮层自动收起，新任务再来时只显示常显卡片（不自动展开）。
#[gpui_kit::test]
fn broom_clears_completed_and_popover_auto_closes(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);

    seed_ops_until_visible(
        &mut vcx,
        &window,
        cx,
        &[
            FakeOp {
                id: 1,
                status: OperationStatus::Running,
                progress: (3, 10),
            },
            FakeOp {
                id: 2,
                status: OperationStatus::Completed,
                progress: (5, 5),
            },
        ],
    );
    vcx.update(|window, cx| window.click("mo-ops-badge", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(vcx.debug_bounds("mo-ops-row-1").is_some());
    assert!(
        vcx.debug_bounds("mo-ops-row-2").is_some(),
        "前提：两条都在浮层上"
    );

    // 扫帚：清除已完成（id=2）——dismiss 走真 manager，视图侧乐观更新先摘行，
    // 泵 / 事件随后的快照同步与之一致。浮层应保持开着（还有进行中任务）。
    vcx.update(|window, cx| window.click("mo-ops-broom", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-ops-row-2").is_none(),
        "扫帚应清掉已完成的任务"
    );
    assert!(
        vcx.debug_bounds("mo-ops-row-1").is_some(),
        "进行中的任务应原样保留"
    );
    assert!(
        vcx.debug_bounds("mo-ops-popover").is_some(),
        "还有进行中任务，浮层应保持开着"
    );

    // 任务全部移除（从真 manager 摘掉最后一条，事件触发 sync_panel）：
    // 浮层自动收起、卡片消失。
    remove_ops(&window, cx, &[1]);
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-ops-popover").is_none(),
        "任务清空后浮层应自动收起"
    );
    assert!(
        vcx.debug_bounds("mo-ops-badge").is_none(),
        "任务清空后常显卡片应消失"
    );

    // 新任务再来：只显示常显卡片，浮层保持收起（不凭空展开旧浮层）。
    seed_ops_until_visible(
        &mut vcx,
        &window,
        cx,
        &[FakeOp {
            id: 3,
            status: OperationStatus::Running,
            progress: (0, 4),
        }],
    );
    assert!(
        vcx.debug_bounds("mo-ops-popover").is_none(),
        "自动收起后新任务不应把浮层重新弹开"
    );
}

/// 侧栏必须有「回收站」入口；点开进回收站面板，且**侧栏不消失**（次级视图
/// 是「浏览型」的，左侧导航要一直在），行样式与文件列表同一套（24px 行高）。
#[gpui_kit::test]
fn sidebar_trash_entry_opens_the_trash_panel(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);

    // 注入两条假回收站记录（真记录要走 Trash 索引链路，headless 拉不动），
    // 行样式 / 侧栏保留的断言靠它们渲染出来。⚠️ 必须**先点开面板再注入**：
    // `open_trash_panel` 会用 `AppState::trash_list()` 刷新条目，先注入会被顶掉。
    let mk = |name: &str, is_dir: bool| mo_operations::TrashEntry {
        id: format!("t-{name}"),
        original: std::path::PathBuf::from("/Users/demo").join(name),
        trashed: std::path::PathBuf::from("/tmp/mo-trash").join(name),
        is_dir,
        at: 1_700_000_000,
    };

    let sidebar = bounds(&mut vcx, "mo-sidebar");
    let trash = bounds(&mut vcx, "mo-sidebar-trash");
    assert!(
        trash.origin.x >= sidebar.origin.x
            && trash.origin.x + trash.size.width <= sidebar.origin.x + sidebar.size.width,
        "回收站入口不在侧栏内：sidebar={sidebar:?} trash={trash:?}"
    );
    // 浏览态下中央区不是次级视图；点回收站后必须是。
    assert!(vcx.debug_bounds("mo-central-view").is_none());

    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    window
        .update(cx, |root, _window, _cx| {
            mo_ui::inject_trash_for_tests(
                root,
                vec![mk("新建文本.txt", false), mk("trae-cn", true)],
            );
        })
        .expect("注入回收站条目失败");
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-central-view").is_some(),
        "点了回收站入口，回收站面板没有出现"
    );
    // 回归（用户报）：进回收站后侧栏不能整个消失。
    let sidebar_after = bounds(&mut vcx, "mo-sidebar");
    assert_eq!(
        sidebar, sidebar_after,
        "进回收站后侧栏位置/大小变了：侧栏不该被次级视图顶掉"
    );
    // 回归（用户报）：回收站行要复用文件列表的行语言，行高就是 24px。
    let row = bounds(&mut vcx, "mo-trash-row-0");
    assert_eq!(
        f32::from(row.size.height),
        24.0,
        "回收站行高 {}：又自成一派了（文件列表行高 24px）",
        f32::from(row.size.height)
    );
}

/// 从回收站点侧栏位置 = **离开回收站去那个目录**（用户报：地址栏变了但人
/// 还留在回收站里，列表也没变）。侧栏导航必须顺手退出次级视图。
#[gpui_kit::test]
fn clicking_a_sidebar_location_leaves_the_trash_panel(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);

    window
        .update(cx, |root, _window, _cx| {
            mo_ui::inject_trash_for_tests(
                root,
                vec![mo_operations::TrashEntry {
                    id: "t-1".into(),
                    original: std::path::PathBuf::from("/Users/demo/a.txt"),
                    trashed: std::path::PathBuf::from("/tmp/mo-trash/a.txt"),
                    is_dir: false,
                    at: 1_700_000_000,
                }],
            );
        })
        .expect("注入回收站条目失败");
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(vcx.debug_bounds("mo-central-view").is_some(), "先进回收站");

    vcx.update(|window, cx| window.click(("sidebar-loc", 0usize), cx));
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-central-view").is_none(),
        "点了侧栏位置，还停在回收站：导航没有退出次级视图"
    );
    assert!(
        vcx.debug_bounds("mo-file-list").is_some(),
        "离开回收站后文件列表没有回来"
    );
}

// ── 空白点击语义与斑马纹铺满一屏（用户报：点空白选中了最后一条；下方空白没有斑马纹）──

use gpui_kit::point;
use std::path::PathBuf;

/// 造一个只装 `n` 个 txt 的临时目录（真导航，别注入假行——后台元数据事件会把
/// 注入的窗口快照作废清掉，测试随机红）。
fn dir_with_files(tag: &str, n: usize) -> PathBuf {
    let base = std::env::temp_dir().join(format!("mo-layout-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    for i in 0..n {
        std::fs::write(base.join(format!("file-{i}.txt")), b"x").unwrap();
    }
    base
}

/// 读当前标签页的本地选中数。
fn selection_count(window: &WindowHandle<RootView>, cx: &mut TestAppContext) -> usize {
    window
        .update(cx, |root, _window, _cx| {
            mo_ui::panel_selection_count_for_tests(root)
        })
        .expect("读选中数失败")
}

/// 真实导航到 `dir`，轮询到第 `want_rows` 行真的画出来（含外部 IO 完成唤醒）。
fn navigate_and_wait(
    vcx: &mut VisualTestContext,
    window: &WindowHandle<RootView>,
    cx: &mut TestAppContext,
    dir: PathBuf,
    want_rows: usize,
) {
    window
        .update(cx, |root, _window, cx| {
            mo_ui::navigate_for_tests(root, dir, cx);
        })
        .expect("导航失败");
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        let ready = window
            .update(cx, |root, _window, _cx| {
                mo_ui::panel_window_ready_for_tests(root, want_rows)
            })
            .unwrap_or(false);
        if ready {
            return;
        }
    }
    panic!("导航后 {} 行没画出来", want_rows);
}

/// 在列表空白处（最后一行之下）单击，应该**清空选择**，而不是选中最后一行。
///
/// 用户报的原状：点一下列表下方空白，最后一行被选中了。根因是橡皮筋几何
/// round + clamp 双叠加——界外的 y 被硬折到最后一行，抬起时回灌成单选。
#[gpui_kit::test]
fn clicking_blank_below_the_list_clears_the_selection(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    let dir = dir_with_files("blank-click", 3);
    navigate_and_wait(&mut vcx, &window, cx, dir.clone(), 3);

    let list = bounds(&mut vcx, "mo-file-list");
    let row0 = bounds(&mut vcx, "mo-file-row-0");
    // 先点第一行（坐标级点击，走真实鼠标事件），确认行点击仍然有效。
    let p_row = point(
        row0.origin.x + row0.size.width / 2.0,
        row0.origin.y + row0.size.height / 2.0,
    );
    vcx.update(|window, cx| window.drag(p_row, p_row, cx));
    vcx.run_until_parked();
    assert_eq!(
        selection_count(&window, cx),
        1,
        "点第一行应选中它（floor 行映射不能把行内点击弄丢）"
    );

    // 再点最后一行之下的空白：选择应清空，而不是选中最后一行。
    // （列表底部 padding 是 10px，取剩余空白的中点，别贴着末行边缘。）
    let p_blank = point(
        list.origin.x + px(200.0),
        list.origin.y + list.size.height - px(5.0),
    );
    vcx.update(|window, cx| window.drag(p_blank, p_blank, cx));
    vcx.run_until_parked();
    assert_eq!(
        selection_count(&window, cx),
        0,
        "点空白应清空选择，不是选中最后一行"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 条目不足一屏时，斑马纹要一直铺到视口底部（下方空白也是斑马行）。
///
/// 靠「把 uniform_list 的行数补到视口装得下的行数」实现，补出来的行没有
/// 数据、走占位斑马纹分支（`mo-file-ph-*`，与窗口未就绪的占位是同一种行）。
#[gpui_kit::test]
fn zebra_stripes_fill_the_viewport_below_the_last_row(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    let dir = dir_with_files("zebra-fill", 3);
    navigate_and_wait(&mut vcx, &window, cx, dir.clone(), 3);
    // 第一帧 prepaint 记下列表高度并 notify；这一帧补足行才画出来。
    vcx.update(|window, cx| window.render_frame(cx));

    // 真实行只有 3 条，行 10 一定是补足行；它必须在视口内、行高 24px。
    let ph = vcx
        .debug_bounds("mo-file-ph-10")
        .expect("条目不足一屏时斑马纹应铺满视口：ph-10 没画出来");
    let list = bounds(&mut vcx, "mo-file-list");
    assert_eq!(f32::from(ph.size.height), 24.0, "补足行高必须与数据行一致");
    assert!(
        f32::from(ph.origin.y) >= f32::from(bounds(&mut vcx, "mo-file-row-2").origin.y),
        "补足行应在真实行之下"
    );
    assert!(
        f32::from(ph.origin.y) + f32::from(ph.size.height)
            <= f32::from(list.origin.y) + f32::from(list.size.height) + 0.5,
        "补足行溢出了列表视口（会出现能滚进空白区的假滚动量）：ph={ph:?} list={list:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 滚动到后面再点行，必须仍能选中（用户报：翻到后面的数据，鼠标点击选不中文件）。
///
/// 走真实事件链路：滚轮把列表滚下去 → 等窗口快照跟上 → 对滚进视口的行发
/// mousedown/mouseup。整条链任何一环断了（虚拟化元素没拿到 click 注册、
/// 事件命中区错位、框选误吞），这条测试都会红。
#[gpui_kit::test]
fn clicking_a_row_after_scrolling_still_selects_it(cx: &mut TestAppContext) {
    use gpui_kit::ScrollDelta;
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    let dir = dir_with_files("scroll-click", 400);
    // 大目录下窗口快照只持有可见区±BUFFER，`panel_window_ready_for_tests` 的
    // `window.len()==rows` 永远不成立——这里只等首屏行真的画出来。
    window
        .update(cx, |root, _window, cx| {
            mo_ui::navigate_for_tests(root, dir.clone(), cx);
        })
        .expect("导航失败");
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        if vcx.debug_bounds("mo-file-row-0").is_some() {
            break;
        }
    }
    assert!(
        vcx.debug_bounds("mo-file-row-0").is_some(),
        "导航后首屏行没画出来"
    );

    // 基线：顶部第一行点击有效（隔离「滚动搞坏了点击」与「点击本来就坏」）。
    // 行元素不进 observation 注册表（uniform_list 子项），点击一律走坐标。
    let row0 = bounds(&mut vcx, "mo-file-row-0");
    let p_row0 = point(
        row0.origin.x + row0.size.width / 2.0,
        row0.origin.y + row0.size.height / 2.0,
    );
    vcx.update(|window, cx| window.drag(p_row0, p_row0, cx));
    vcx.run_until_parked();
    assert_eq!(selection_count(&window, cx), 1, "顶部行点击应选中");

    // 滚轮把列表往下滚（delta.y 为负 = 内容上移），分几步滚并让窗口快照跟上。
    // uniform_list 不进 observation 注册表，滚轮事件直接按坐标派发。
    use gpui_kit::{InputEvent, ScrollWheelEvent};
    let list = bounds(&mut vcx, "mo-file-list");
    let p_wheel = point(
        list.origin.x + list.size.width / 2.0,
        list.origin.y + list.size.height / 2.0,
    );
    for _ in 0..3 {
        vcx.update(|window, cx| {
            window.dispatch_event(
                ScrollWheelEvent {
                    position: p_wheel,
                    delta: ScrollDelta::Pixels(point(px(0.), px(-1500.))),
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        });
        vcx.run_until_parked();
    }
    vcx.update(|window, cx| window.render_frame(cx));

    // 滚动必须真的发生了：首行已滚出渲染帧。
    assert!(
        vcx.debug_bounds("mo-file-row-0").is_none(),
        "滚轮事件没有生效，首行仍在渲染帧里"
    );

    // 找一个已滚进视口的靠后行（不假设精确落点，只要「不在首屏」即可）。
    let later = [50usize, 100, 150, 200, 250, 300].into_iter().find(|i| {
        vcx.debug_bounds(Box::leak(format!("mo-file-row-{i}").into_boxed_str()))
            .is_some()
    });
    let Some(later) = later else {
        panic!("滚动后没有任何靠后行被渲染出来");
    };
    let selector: &'static str = Box::leak(format!("mo-file-row-{later}").into_boxed_str());
    let target = bounds(&mut vcx, selector);
    assert!(
        f32::from(target.origin.y) >= 0.0 && f32::from(target.origin.y) < 700.0,
        "滚动后行 {later} 应落在视口内，实际 {target:?}"
    );

    vcx.update(|window, cx| {
        let p = point(
            target.origin.x + target.size.width / 2.0,
            target.origin.y + target.size.height / 2.0,
        );
        window.drag(p, p, cx)
    });
    vcx.run_until_parked();
    assert_eq!(
        selection_count(&window, cx),
        1,
        "滚动到后面再点行也应选中它"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 暂存区抽屉：默认不占地方，点状态栏入口展开，且**贴在状态栏之上**、不挤掉它。
///
/// 抽屉是常驻 UI（不是模态），所以它必须在布局里真占一行、且只在展开时占——
/// 收起时文件区的高度不该变（与传输浮层绝对定位不占布局是不同的取舍）。
#[gpui_kit::test]
fn staging_tray_is_toggled_from_the_status_bar(cx: &mut TestAppContext) {
    let (mut vcx, _window) = open_app(size(px(1000.), px(700.)), cx);

    assert!(
        vcx.debug_bounds("mo-staging-tray").is_none(),
        "抽屉默认收起，不该出现在渲染帧里"
    );
    let list_before = bounds(&mut vcx, "mo-file-list");

    vcx.update(|window, cx| window.click("statusbar-staging", cx));
    vcx.update(|window, cx| window.render_frame(cx));

    let tray = bounds(&mut vcx, "mo-staging-tray");
    let status = bounds(&mut vcx, "mo-statusbar");
    let list_after = bounds(&mut vcx, "mo-file-list");
    assert!(
        f32::from(tray.origin.y) + f32::from(tray.size.height) <= f32::from(status.origin.y),
        "抽屉应贴在状态栏上方：tray={tray:?} status={status:?}"
    );
    assert_eq!(
        tray.size.width, status.size.width,
        "抽屉通栏：与状态栏同宽，不是浮在角落的一块"
    );
    // 抽屉真占布局（状态栏始终贴底不动，被挤的是文件区）——所以它才有
    // 「常驻」的意义，而不是像传输浮层那样绝对定位悬着。
    assert!(
        f32::from(list_after.size.height) < f32::from(list_before.size.height),
        "展开后文件区应让出高度：before={list_before:?} after={list_after:?}"
    );

    // 再点一次收起：文件区高度还原。
    vcx.update(|window, cx| window.click("statusbar-staging", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-staging-tray").is_none(),
        "再点一次应收起"
    );
    assert_eq!(
        bounds(&mut vcx, "mo-file-list").size.height,
        list_before.size.height,
        "收起后文件区应回到原高度"
    );
}

/// 磁盘地图：块铺满画布，且**面积与大小成正比**——这是 treemap 唯一的硬契约。
///
/// 数据用假树注入（真统计要递归扫盘，不该在测试里做）；布局算法本身由
/// `mo-app` 的单测守着，这里守的是「UI 真把它画出来了、几何没走形」。
#[gpui_kit::test]
fn disk_usage_treemap_tiles_are_proportional(cx: &mut TestAppContext) {
    use mo_app::UsageTree;

    let tree = UsageTree {
        path: std::path::PathBuf::from("/tmp/usage-root"),
        name: "usage-root".to_string(),
        size: 1000,
        is_dir: true,
        children: vec![
            UsageTree {
                path: std::path::PathBuf::from("/tmp/usage-root/big"),
                name: "big".to_string(),
                size: 600,
                is_dir: false,
                children: vec![],
            },
            UsageTree {
                path: std::path::PathBuf::from("/tmp/usage-root/mid"),
                name: "mid".to_string(),
                size: 300,
                is_dir: false,
                children: vec![],
            },
            UsageTree {
                path: std::path::PathBuf::from("/tmp/usage-root/small"),
                name: "small".to_string(),
                size: 100,
                is_dir: false,
                children: vec![],
            },
        ],
    };

    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    window
        .update(cx, |v, _window, cx| {
            mo_ui::inject_usage_tree_for_tests(v, tree);
            cx.notify();
        })
        .expect("注入磁盘地图树失败");
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));

    let canvas = bounds(&mut vcx, "mo-usage-treemap");
    let area = |b: Bounds<Pixels>| f32::from(b.size.width) * f32::from(b.size.height);
    let canvas_area = area(canvas);
    assert!(canvas_area > 0.0, "画布应该有尺寸");

    let tiles: Vec<Bounds<Pixels>> = (0..3)
        .map(|i| {
            bounds(
                &mut vcx,
                Box::leak(format!("mo-usage-tile-{i}").into_boxed_str()),
            )
        })
        .collect();
    let sum: f32 = tiles.iter().map(|t| area(*t)).sum();
    assert!(
        (sum - canvas_area).abs() / canvas_area < 0.02,
        "块应当铺满画布：sum={sum} canvas={canvas_area}"
    );

    // 面积比例 = 大小比例（6 : 3 : 1），每个块都单独核对。
    for (i, want) in [0.6f32, 0.3, 0.1].iter().enumerate() {
        let got = area(tiles[i]) / canvas_area;
        assert!(
            (got - want).abs() < 0.02,
            "第 {i} 块的面积占比 {got} 应当约等于 {want}"
        );
    }
}

/// 分栏对比的图例条：常驻一行、贴在状态栏上方、点「关闭对比」即退场。
///
/// 它跟暂存区抽屉一样是**占布局**的一行（不是浮层）：对比是个持续状态，
/// 用户得一直看得见「现在正在对比」，而不是点开一次就忘。关掉之后文件区
/// 高度必须还原——少一步就是漏了一块永久占位。
#[gpui_kit::test]
fn compare_legend_occupies_a_row_and_closes(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);

    assert!(
        vcx.debug_bounds("mo-compare-legend").is_none(),
        "默认不在对比，不该有图例条"
    );
    let list_before = bounds(&mut vcx, "mo-file-list");

    window
        .update(cx, |v, _window, cx| {
            mo_ui::inject_compare_for_tests(v, (3, 2, 1));
            cx.notify();
        })
        .expect("注入对比状态失败");
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));

    let legend = bounds(&mut vcx, "mo-compare-legend");
    let status = bounds(&mut vcx, "mo-statusbar");
    let list_after = bounds(&mut vcx, "mo-file-list");
    assert!(
        f32::from(legend.origin.y) + f32::from(legend.size.height) <= f32::from(status.origin.y),
        "图例应贴在状态栏上方：legend={legend:?} status={status:?}"
    );
    assert_eq!(
        legend.size.width, status.size.width,
        "图例通栏：与状态栏同宽"
    );
    assert!(
        f32::from(list_after.size.height) < f32::from(list_before.size.height),
        "对比期间文件区应让出一行高度：before={list_before:?} after={list_after:?}"
    );

    vcx.update(|window, cx| window.click("compare-close", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-compare-legend").is_none(),
        "点「关闭对比」后图例应退场"
    );
    assert_eq!(
        bounds(&mut vcx, "mo-file-list").size.height,
        list_before.size.height,
        "关掉后文件区应回到原高度"
    );
}

/// 内容搜索面板：打开后骨架完整——四个开关胶囊 + 搜索按钮 + 结果区占位都在。
///
/// 它跟全局搜索共用「中央区 + 侧栏」框架，区别在结果区是 grep 报告而非文件列表。
/// 这里只守骨架（范围 / 命中细节由 `mo-search` 单测管），确保打开不白屏、控件齐。
#[gpui_kit::test]
fn content_search_panel_renders_its_skeleton(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);

    assert!(
        vcx.debug_bounds("mo-content-body").is_none(),
        "默认不在内容搜索，不该有结果区"
    );

    window
        .update(cx, |v, _window, cx| {
            mo_ui::inject_content_search_for_tests(v);
            cx.notify();
        })
        .expect("打开内容搜索失败");
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));

    // 结果区骨架。
    bounds(&mut vcx, "mo-content-body");
    // 四个开关胶囊 + 搜索按钮齐全。
    for i in 0..4 {
        let sel = Box::leak(format!("mo-content-opt-{i}").into_boxed_str());
        bounds(&mut vcx, sel);
    }
    bounds(&mut vcx, "mo-content-go");

    // 没跑过搜索 → 结果区是「输入即搜」提示，不该出现命中行。
    assert!(
        vcx.debug_bounds("mo-content-row-0").is_none(),
        "还没搜，不应有命中行"
    );
}

/// 回收站条目不足一屏时，斑马纹要一直铺到视口底部（用户报：只有两条记录时
/// 下面一片白，不像同一个应用）。补出来的行没有数据、只有底色
/// （`mo-trash-ph-*`），底色 / 行高与数据行同一套规矩。
#[gpui_kit::test]
fn trash_zebra_stripes_fill_the_viewport(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    let mk = |name: &str, is_dir: bool| mo_operations::TrashEntry {
        id: format!("t-{name}"),
        original: std::path::PathBuf::from("/Users/demo").join(name),
        trashed: std::path::PathBuf::from("/tmp/mo-trash").join(name),
        is_dir,
        at: 1_700_000_000,
    };
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    window
        .update(cx, |root, _window, _cx| {
            mo_ui::inject_trash_for_tests(root, vec![mk("a.txt", false), mk("b", true)]);
        })
        .expect("注入回收站条目失败");
    // 第一帧 prepaint 记下内容区高度并 notify；这一帧补足行才画出来。
    vcx.update(|window, cx| window.render_frame(cx));
    vcx.update(|window, cx| window.render_frame(cx));

    // 第一条占位行必须紧贴最后一条真实行（无缝，底色才接得上）。
    let row1 = bounds(&mut vcx, "mo-trash-row-1");
    let ph0 = bounds(&mut vcx, "mo-trash-ph-2");
    assert_eq!(f32::from(ph0.size.height), 24.0, "占位行高必须与数据行一致");
    assert_eq!(
        f32::from(ph0.origin.y),
        f32::from(row1.origin.y) + f32::from(row1.size.height),
        "占位行与真实行之间出现缝隙：row1={row1:?} ph0={ph0:?}"
    );

    // 连续占位行逐行 24px 往下排。
    let ph3 = bounds(&mut vcx, "mo-trash-ph-3");
    assert_eq!(
        f32::from(ph3.origin.y),
        f32::from(ph0.origin.y) + 24.0,
        "占位行之间没有按 24px 行高连续排布"
    );

    // 铺满一屏：从第一条占位行往下数到断档，最后一行必须盖过视口大半
    // （700px 窗口，内容区高约 500px；若只画真实行这里只有 48px）。
    let mut last_bottom = f32::from(ph0.origin.y) + f32::from(ph0.size.height);
    let mut i = 3;
    loop {
        // `debug_bounds` 要 `&'static str`，循环里构造的选择子直接泄漏一块
        // （测试进程生命周期内就这一小段，无所谓）。
        let sel: &'static str = Box::leak(format!("mo-trash-ph-{i}").into_boxed_str());
        let Some(b) = vcx.debug_bounds(sel) else {
            break;
        };
        last_bottom = f32::from(b.origin.y) + f32::from(b.size.height);
        assert_eq!(f32::from(b.size.height), 24.0, "占位行 {i} 行高不是 24px");
        i += 1;
    }
    assert!(i - 2 >= 10, "补足行只有 {} 条：斑马纹没铺满一屏", i - 2);
    assert!(
        last_bottom > 550.0,
        "斑马纹只铺到 y={last_bottom}，没到视口底部"
    );
}

/// 回归（用户报，Windows 构建）：空回收站里斑马纹补足行**自己喂自己**——
/// `overflow_y_scrollbar()` 把原 div 重构成滚动条叠加层，`on_prepaint` 量到的是
/// **内容层**高度（随行数增长）而不是视口高度。补足行让内容变高 → 下一帧量到
/// 更高 → 补更多行，无限正反馈：0 条目也能滚出几千行空白。
/// 这里连续渲染多帧，断言补足行总数有界（铺满一屏即可，不该随帧数增长）。
#[gpui_kit::test]
fn trash_filler_rows_do_not_grow_every_frame(cx: &mut TestAppContext) {
    let seed = std::env::temp_dir().join(format!("mo-layout-trash-filler-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&seed);
    let (mut vcx, window) = open_app_with_trash(size(px(1000.), px(700.)), seed, cx);
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    window
        .update(cx, |root, _window, _cx| {
            mo_ui::inject_trash_for_tests(root, Vec::new());
        })
        .expect("注入空回收站失败");
    // 跑多帧：正反馈每帧把行数往上推一档，帧数足够多必然暴露。
    for _ in 0..300 {
        vcx.update(|window, cx| window.render_frame(cx));
    }

    let mut count = 0usize;
    loop {
        let sel: &'static str = Box::leak(format!("mo-trash-ph-{count}").into_boxed_str());
        if vcx.debug_bounds(sel).is_some() {
            count += 1;
        } else {
            break;
        }
    }
    // 700px 窗口的回收站视口装得下 ~26 行 24px；给足余量也不该超过 40。
    assert!(
        count > 0 && count <= 40,
        "补足行有 {count} 条：内容高度回写把补足数喂成了无限增长（空白行可无限下滚）"
    );
}

/// 回归（用户报）：**首次**进入回收站先是一片空白，几百毫秒~1 秒后才冒出斑马纹。
///
/// 补足行要靠 prepaint 量到的内容区高度，所以「首帧还没有补足行」是设计使然；
/// 真正的判据是**首帧有没有替自己排下一帧**。gpui 里 paint/prepaint 阶段调
/// `cx.notify()` 只把视图标脏，**不会叫醒平台的帧循环**——没有下一个外部事件
/// （条目到货、图标扫描回来、鼠标动一下）就永远没有第二帧，空白就一直挂着。
/// `Window::request_animation_frame()` 才是那道唤醒：往 next-frame 队列放一个
/// notify 回调，并 `schedule_frame` + `wake_platform`。
///
/// 测试就把这条队列当 observable：**先清空队列**（把文件列表首帧自己那条唤醒
/// 排掉），再进回收站画一帧，队列里必须留下 ≥1 条回调——少了它 = 唤醒丢了。
#[gpui_kit::test]
fn trash_zebra_fill_requests_its_own_second_frame(cx: &mut TestAppContext) {
    let seed = std::env::temp_dir().join(format!(
        "mo-layout-trash-first-frame-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&seed);
    let (mut vcx, _window) = open_app_with_trash(size(px(1000.), px(700.)), seed, cx);
    // 排掉进回收站之前就挂在队列里的唤醒，让下面的计数只反映这一次面板首帧。
    vcx.update(|window, cx| window.simulate_next_frame(cx));
    // **空**回收站也必须有斑马纹（用户要求「不管有没有文件」）。
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));

    let queued = vcx.update(|window, cx| window.simulate_next_frame(cx));
    assert!(
        queued > 0,
        "进入回收站的首帧没有为自己请求下一帧：prepaint 回写高度后的 notify 落不了地，斑马纹要等外部事件才出现"
    );

    // 那一帧真的把补足行画出来了，并且铺到视口底部。
    vcx.update(|window, cx| window.render_frame(cx));
    let ph0 = bounds(&mut vcx, "mo-trash-ph-0");
    assert_eq!(f32::from(ph0.size.height), 24.0);
    let mut last_bottom = f32::from(ph0.origin.y) + f32::from(ph0.size.height);
    let mut i = 1;
    loop {
        let sel: &'static str = Box::leak(format!("mo-trash-ph-{i}").into_boxed_str());
        let Some(b) = vcx.debug_bounds(sel) else {
            break;
        };
        last_bottom = f32::from(b.origin.y) + f32::from(b.size.height);
        i += 1;
    }
    assert!(
        last_bottom > 550.0,
        "自请求的第二帧仍然只铺到 y={last_bottom}，没到视口底部"
    );
}

/// 清空回收站必须**先弹确认卡**（面板标题栏「清空回收站」按钮）：Esc / 取消 /
/// 点遮罩回面板、条目原样；确认（Enter 或红色按钮）才真正执行并回到面板。
///
/// 条目**预种进 store**（`<root>/index.json` + 真实落点文件）而不是注入视图：
/// `sync_panel` 会拿 store 的列表覆盖 `trash_entries`，keystroke 泵 effect 时
/// 注入的视图本地条目会被空 store 顶掉（本测试首次运行时踩到）。
#[gpui_kit::test]
fn trash_empty_asks_for_confirmation(cx: &mut TestAppContext) {
    // 独立的回收站根（见 `open_app_with_trash`）：先清干净再预种两条记录
    // （含真实落点文件）。
    let seed = std::env::temp_dir().join(format!("mo-layout-trash-confirm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&seed);
    std::fs::create_dir_all(&seed).unwrap();
    // index.json 手写（layout 测试不引 serde_json）：字段名与 TrashEntry 的
    // serde 默认命名逐字一致。
    let mut records = String::from("[");
    for (i, name) in ["a.txt", "b.txt"].iter().enumerate() {
        let id = format!("seed-{i}");
        let dir = seed.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), b"x").unwrap();
        if i > 0 {
            records.push(',');
        }
        records.push_str(&format!(
            r#"{{"id":"{id}","original":"/Users/demo/{name}","trashed":"{}","is_dir":false,"at":1700000000}}"#,
            jpath(&dir.join(name))
        ));
    }
    records.push(']');
    std::fs::write(seed.join("index.json"), records).unwrap();

    let (mut vcx, window) = open_app_with_trash(size(px(1000.), px(700.)), seed, cx);
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    let state = |vcx: &mut VisualTestContext, window: &WindowHandle<mo_ui::RootView>| {
        window
            .update(&mut vcx.cx, |root, _w, _cx| {
                mo_ui::trash_panel_state_for_tests(root)
            })
            .expect("读回收站状态失败")
    };
    assert_eq!(state(&mut vcx, &window), (2, false), "前提：两条都在面板上");
    assert!(vcx.debug_bounds("mo-trash-row-1").is_some());

    // 点「清空回收站」按钮：只弹确认卡，不执行（条目数不变）。
    vcx.update(|window, cx| window.click("trash-empty-btn", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-dialog-card").is_some(),
        "清空回收站应当先弹确认卡"
    );
    assert_eq!(
        state(&mut vcx, &window),
        (2, true),
        "确认卡出现时条目数不能变"
    );
    assert!(
        vcx.debug_bounds("mo-trash-row-1").is_some(),
        "回收站面板应保留在遮罩后面"
    );

    // Esc：取消——回面板、条目原样。
    cx.simulate_keystrokes(window.into(), "escape");
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-dialog-card").is_none(),
        "取消后卡应消失"
    );
    assert_eq!(state(&mut vcx, &window), (2, false), "取消后条目应原样");

    // 再点按钮 → 点「取消」按钮：同样回面板、条目原样。
    vcx.update(|window, cx| window.click("trash-empty-btn", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    vcx.update(|window, cx| window.click("trash-confirm-cancel", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(vcx.debug_bounds("mo-dialog-card").is_none());
    assert_eq!(state(&mut vcx, &window), (2, false));

    // 再点按钮 → 点红色「清空」：面板清空（store 里两条记录都被抹掉）。
    vcx.update(|window, cx| window.click("trash-empty-btn", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    vcx.update(|window, cx| window.click("trash-confirm-ok", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-dialog-card").is_none(),
        "确认后卡应关闭"
    );
    assert_eq!(state(&mut vcx, &window), (0, false), "清空后面板应为空");
}

/// 回收站条目支持**预览与打开**（Finder 废纸篓同款）：
/// 空格 = Quick Look 实际落点文件（独立预览窗）；双击 = 打开（文件交系统默认
/// 应用——headless 不能真开，测目录：退出面板并在当前窗口浏览该目录）。
///
/// 条目同样预种 store（理由见 `trash_purge_and_empty_ask_for_confirmation`）。
/// ⚠️ `Trash::list()` 返回 index.json 的**反序**（最新在前），所以种子要按
/// [目录, 文件] 写，面板上才是 row0=文件（预览对象）、row1=目录（双击对象）。
#[gpui_kit::test]
fn trash_space_previews_and_double_click_opens(cx: &mut TestAppContext) {
    // 独立的回收站根（见 `open_app_with_trash`）：seed-0 目录里放文件 a.txt
    // （空格预览的对象），seed-1 是目录（双击打开的对象，里面放一个文件供
    // 列表断言）。
    let seed = std::env::temp_dir().join(format!("mo-layout-trash-preview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&seed);
    std::fs::create_dir_all(seed.join("seed-0")).unwrap();
    std::fs::write(seed.join("seed-0").join("a.txt"), b"hello mo").unwrap();
    std::fs::create_dir_all(seed.join("seed-1")).unwrap();
    std::fs::write(seed.join("seed-1").join("inner.txt"), b"x").unwrap();
    // JSON 顺序 = [目录, 文件]：list() 反序后面板 row0=a.txt、row1=目录。
    std::fs::write(
        seed.join("index.json"),
        format!(
            r#"[{{"id":"seed-1","original":"/Users/demo/folder","trashed":"{}","is_dir":true,"at":1700000000}},{{"id":"seed-0","original":"/Users/demo/a.txt","trashed":"{}","is_dir":false,"at":1700000000}}]"#,
            jpath(&seed.join("seed-1")),
            jpath(&seed.join("seed-0").join("a.txt"))
        ),
    )
    .unwrap();

    let (mut vcx, window) = open_app_with_trash(size(px(1000.), px(700.)), seed, cx);
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-trash-row-0").is_some(),
        "前提：两条都在面板上"
    );
    assert!(vcx.debug_bounds("mo-trash-row-1").is_some());

    // 双击目录行：退出回收站面板，在当前窗口浏览该目录（列表出现 inner.txt）。
    vcx.update(|window, cx| window.double_click("trash-row-1", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-trash-row-0").is_none(),
        "双击目录后应退出回收站面板"
    );
    assert!(
        vcx.debug_bounds("mo-file-row-0").is_some(),
        "双击目录后当前窗口应浏览该目录"
    );

    // 重新打开面板，选中文件行（a.txt 在 row0）后按空格：预览的是 ~/.Trash
    // 里的实际落点文件。
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    vcx.update(|window, cx| window.click("trash-row-0", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    cx.simulate_keystrokes(window.into(), "space");
    vcx.run_until_parked();
    let preview_open = window
        .update(&mut vcx.cx, |root, _w, _cx| {
            mo_ui::trash_preview_open_for_tests(root)
        })
        .expect("读预览窗口状态失败");
    assert!(preview_open, "空格应当打开快速预览窗");
}

/// 任务多时浮层列表必须是**定高滚动区**：`Scrollable` 需要定高上下文，
/// auto 高度链 + `max_h` 撑不出滚动区——此前任务多时既滚不动也看不到滚动条
/// （用户实测）。8 行 × 56 = 448 > 300 上限，浮层总高必须被钳住。
#[gpui_kit::test]
fn task_popover_list_is_bounded_when_many_tasks(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    let many: Vec<FakeOp> = (1..=8)
        .map(|i| FakeOp {
            id: i,
            status: OperationStatus::Running,
            progress: (1, 2),
        })
        .collect();
    seed_ops_until_visible(&mut vcx, &window, cx, &many);
    vcx.update(|window, cx| window.click("mo-ops-badge", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));

    let pop = bounds(&mut vcx, "mo-ops-popover");
    // 列表 300 + 头部 ~36 + 分隔线/边框 ~3：浮层总高 ~339，绝不跟随 448 的内容长。
    assert!(
        f32::from(pop.size.height) <= 342.0,
        "浮层高 {}：任务列表没有钳在 300px 定高滚动区里",
        pop.size.height
    );
}

/// 少任务时列表按内容自适应（高度 = 行数 × 行高），不该被撑到滚动上限。
#[gpui_kit::test]
fn task_popover_list_fits_content_when_few_tasks(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    let few = vec![
        FakeOp {
            id: 1,
            status: OperationStatus::Running,
            progress: (1, 2),
        },
        FakeOp {
            id: 2,
            status: OperationStatus::Completed,
            progress: (2, 2),
        },
    ];
    seed_ops_until_visible(&mut vcx, &window, cx, &few);
    vcx.update(|window, cx| window.click("mo-ops-badge", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    let pop = bounds(&mut vcx, "mo-ops-popover");
    // 2 行 × 56 + 头部 ~36 ≈ 152，远小于 342 的上限。
    assert!(
        f32::from(pop.size.height) < 200.0,
        "2 个任务的浮层高 {}：不该被撑到滚动上限",
        pop.size.height
    );
}

/// 在 headless 里对指定 debug_selector 的元素派发一次**带修饰键**的左键点击
/// （gpui-kit 的 `click` 助手恒为无修饰键，回收站的 ⌘/⇧ 多选语义测不到）。
fn click_with_modifiers(
    vcx: &mut VisualTestContext,
    selector: &'static str,
    mods: gpui_kit::Modifiers,
) {
    let b = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} 没有渲染"));
    let center = gpui_kit::point(
        b.origin.x + b.size.width / 2.0,
        b.origin.y + b.size.height / 2.0,
    );
    vcx.update(|window, cx| {
        window.dispatch_event(
            gpui_kit::MouseDownEvent {
                button: gpui_kit::MouseButton::Left,
                position: center,
                modifiers: mods,
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window.dispatch_event(
            gpui_kit::MouseUpEvent {
                button: gpui_kit::MouseButton::Left,
                position: center,
                modifiers: mods,
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    });
}

/// 断言某个 debug_selector 的行**有没有真的画出**选中底色——比对
/// `painted_quads()` 的真实绘制输出（⌘A 只改状态集合、渲染不画高亮
/// 曾是真 bug：headless 探针读状态全绿、真机没反应）。
fn row_has_selected_bg(
    vcx: &mut VisualTestContext,
    selector: &'static str,
    bg: gpui_kit::Rgba,
) -> bool {
    let Some(b) = vcx.debug_bounds(selector) else {
        return false;
    };
    let scale = vcx.update(|window, _cx| window.scale_factor());
    let (x, y, w, h) = (
        f32::from(b.origin.x) * scale,
        f32::from(b.origin.y) * scale,
        f32::from(b.size.width) * scale,
        f32::from(b.size.height) * scale,
    );
    let near = |a: f32, c: f32| (a - c).abs() < 0.5;
    let quads = vcx.update(|window, _cx| window.painted_quads());
    quads.iter().any(|q| {
        near(q.bounds.origin.x.as_f32(), x)
            && near(q.bounds.origin.y.as_f32(), y)
            && near(q.bounds.size.width.as_f32(), w)
            && near(q.bounds.size.height.as_f32(), h)
            && q.background == gpui_kit::Background::from(bg)
    })
}

/// 回收站面板支持**多选**（Finder / 资源管理器同款语义）：
/// 普通左键 = 单选替换；⌘/Ctrl + 左键 = 切换；⇧ + 左键 = 从锚点连选；
/// ⇧ + ↑↓ = 键盘延伸；普通 ↑↓ = 单选替换；⌘/Ctrl + A = 全选。
#[gpui_kit::test]
fn trash_panel_supports_multi_select(cx: &mut TestAppContext) {
    // 预种三条记录（理由见 trash_purge_and_empty_ask_for_confirmation）。
    let seed = std::env::temp_dir().join(format!("mo-layout-trash-multi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&seed);
    std::fs::create_dir_all(&seed).unwrap();
    let mut records = String::from("[");
    for i in 0..3 {
        let id = format!("seed-{i}");
        let name = format!("f{i}.txt");
        let dir = seed.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&name), b"x").unwrap();
        if i > 0 {
            records.push(',');
        }
        records.push_str(&format!(
            r#"{{"id":"{id}","original":"/Users/demo/{name}","trashed":"{}","is_dir":false,"at":1700000000}}"#,
            jpath(&dir.join(&name))
        ));
    }
    records.push(']');
    std::fs::write(seed.join("index.json"), records).unwrap();

    let (mut vcx, window) = open_app_with_trash(size(px(1000.), px(700.)), seed, cx);
    // 还没进回收站：地址栏是普通浏览态，没有「回收站」胶囊。
    assert!(
        vcx.debug_bounds("mo-address-trash").is_none(),
        "普通浏览态不该出现回收站地址胶囊"
    );
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(vcx.debug_bounds("mo-trash-row-2").is_some(), "三条都应渲染");
    // 进了回收站：地址栏换成「回收站」胶囊（用户报：进回收站地址栏没变）。
    assert!(
        vcx.debug_bounds("mo-address-trash").is_some(),
        "回收站模式地址栏应显示「回收站」胶囊"
    );

    let sel = |vcx: &mut VisualTestContext, window: &WindowHandle<RootView>| -> Vec<usize> {
        window
            .update(&mut vcx.cx, |root, _w, _cx| {
                mo_ui::trash_selection_for_tests(root)
            })
            .expect("读回收站多选失败")
    };
    let redraw = |vcx: &mut VisualTestContext| {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
    };

    // 普通左键 = 单选替换。
    vcx.update(|window, cx| window.click("trash-row-1", cx));
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![1]);
    // 视觉回归：选中底色必须**真的画出来**（曾出现状态全选、渲染只亮游标）。
    let bg = mo_ui::trash_selected_bg_for_tests();
    assert!(
        row_has_selected_bg(&mut vcx, "mo-trash-row-1", bg),
        "row1 单选后应画选中底色"
    );
    assert!(
        !row_has_selected_bg(&mut vcx, "mo-trash-row-0", bg),
        "row0 未选中不应亮"
    );

    // ⇧ + ↓：从锚点（1）延伸到 2。
    cx.simulate_keystrokes(window.into(), "shift-down");
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![1, 2]);

    // ⇧ + ↑：收回一格。
    cx.simulate_keystrokes(window.into(), "shift-up");
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![1]);
    // 再 ⇧ + ↑：越过锚点向上连选。
    cx.simulate_keystrokes(window.into(), "shift-up");
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![0, 1]);

    // 普通 ↓：回到单选替换。
    cx.simulate_keystrokes(window.into(), "down");
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![1]);

    // ⌘A / Ctrl+A：全选。
    cx.simulate_keystrokes(window.into(), &format!("{PRIMARY}-a"));
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![0, 1, 2]);
    // 视觉回归：三行都得画上选中底色。
    for s in ["mo-trash-row-0", "mo-trash-row-1", "mo-trash-row-2"] {
        assert!(
            row_has_selected_bg(&mut vcx, s, bg),
            "{s} ⌘A 后应画选中底色"
        );
    }

    // ⌘ + 左键：把 row2 从全选里切掉。
    click_with_modifiers(
        &mut vcx,
        "mo-trash-row-2",
        gpui_kit::Modifiers {
            platform: true,
            ..Default::default()
        },
    );
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![0, 1]);

    // 普通左键：单选替换。
    vcx.update(|window, cx| window.click("trash-row-0", cx));
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![0]);

    // ⇧ + 左键：从锚点（0）连选到 2。
    click_with_modifiers(
        &mut vcx,
        "mo-trash-row-2",
        gpui_kit::Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![0, 1, 2]);
}

/// 多选时**动作作用于整个选中集**：「还原」按钮把所有选中项一起还原（按钮
/// 文案带条数）。动作后多选清空。
#[gpui_kit::test]
fn trash_multi_select_restore_acts_on_the_selection(cx: &mut TestAppContext) {
    let seed =
        std::env::temp_dir().join(format!("mo-layout-trash-multi-act-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&seed);
    std::fs::create_dir_all(&seed).unwrap();
    let mut records = String::from("[");
    for i in 0..3 {
        let id = format!("seed-{i}");
        let name = format!("f{i}.txt");
        let dir = seed.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&name), b"x").unwrap();
        if i > 0 {
            records.push(',');
        }
        records.push_str(&format!(
            r#"{{"id":"{id}","original":"{}/demo-{name}","trashed":"{}","is_dir":false,"at":1700000000}}"#,
            jpath(&seed),
            jpath(&dir.join(&name))
        ));
    }
    records.push(']');
    std::fs::write(seed.join("index.json"), records).unwrap();

    let (mut vcx, window) = open_app_with_trash(size(px(1000.), px(700.)), seed, cx);
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    let state = |vcx: &mut VisualTestContext, window: &WindowHandle<RootView>| {
        window
            .update(&mut vcx.cx, |root, _w, _cx| {
                mo_ui::trash_panel_state_for_tests(root)
            })
            .expect("读回收站状态失败")
    };
    let sel = |vcx: &mut VisualTestContext, window: &WindowHandle<RootView>| -> Vec<usize> {
        window
            .update(&mut vcx.cx, |root, _w, _cx| {
                mo_ui::trash_selection_for_tests(root)
            })
            .expect("读回收站多选失败")
    };
    let redraw = |vcx: &mut VisualTestContext| {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
    };

    // 选中 0、1 两行，Enter 还原——两条一起走（还原落点是 seed 下的 demo-f*.txt，
    // 与预种目录互不相干）。
    vcx.update(|window, cx| window.click("trash-row-0", cx));
    redraw(&mut vcx);
    cx.simulate_keystrokes(window.into(), "shift-down");
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![0, 1]);

    // Enter 不再还原（macOS = 重命名）：还原走面板上方的「还原」按钮。
    assert!(
        vcx.debug_bounds("mo-trash-restore").is_some(),
        "有选中时还原按钮应出现"
    );
    vcx.update(|window, cx| window.click("trash-restore-btn", cx));
    for _ in 0..20 {
        redraw(&mut vcx);
        if state(&mut vcx, &window).0 == 1 {
            break;
        }
    }
    assert_eq!(state(&mut vcx, &window).0, 1, "两条选中项应一起被还原");
    assert!(
        sel(&mut vcx, &window).is_empty(),
        "动作后多选应清空（列表变了）"
    );
}

/// 回收站 Enter 平台语义 + 还原按钮显隐：
/// * 进面板**不默认选中**任何行；「还原」按钮只在有选中时出现；
/// * macOS 上 Enter = 重命名：弹重命名卡（预填当前名），提交后实际落点文件
///   与面板列表同步改名（账本一致，之后还原仍可用）。
#[gpui_kit::test]
fn trash_enter_renames_and_restore_button_follows_selection(cx: &mut TestAppContext) {
    // 预种三条（f0..f2；列表最新在前 → 行序 f2, f1, f0）。
    let seed = std::env::temp_dir().join(format!("mo-layout-trash-rename-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&seed);
    std::fs::create_dir_all(&seed).unwrap();
    let mut records = String::from("[");
    for i in 0..3 {
        let id = format!("seed-{i}");
        let name = format!("f{i}.txt");
        let dir = seed.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&name), b"x").unwrap();
        if i > 0 {
            records.push(',');
        }
        records.push_str(&format!(
            r#"{{"id":"{id}","original":"{}/demo-{name}","trashed":"{}","is_dir":false,"at":1700000000}}"#,
            jpath(&seed),
            jpath(&dir.join(&name))
        ));
    }
    records.push(']');
    std::fs::write(seed.join("index.json"), records).unwrap();

    let (mut vcx, window) = open_app_with_trash(size(px(1000.), px(700.)), seed.clone(), cx);
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    let names = |vcx: &mut VisualTestContext, window: &WindowHandle<RootView>| -> Vec<String> {
        window
            .update(&mut vcx.cx, |root, _w, _cx| {
                mo_ui::trash_entry_names_for_tests(root)
            })
            .expect("读条目名失败")
    };
    let sel = |vcx: &mut VisualTestContext, window: &WindowHandle<RootView>| -> Vec<usize> {
        window
            .update(&mut vcx.cx, |root, _w, _cx| {
                mo_ui::trash_selection_for_tests(root)
            })
            .expect("读回收站多选失败")
    };
    let redraw = |vcx: &mut VisualTestContext| {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
    };

    // ① 不默认选中第一条：无选中、无还原按钮。
    assert!(
        sel(&mut vcx, &window).is_empty(),
        "进面板不应默认选中任何行"
    );
    assert!(
        vcx.debug_bounds("mo-trash-restore").is_none(),
        "无选中时不应出现还原按钮"
    );

    // ② 选中一行 → 还原按钮出现。
    vcx.update(|window, cx| window.click("trash-row-0", cx));
    redraw(&mut vcx);
    assert_eq!(sel(&mut vcx, &window), vec![0]);
    assert!(
        vcx.debug_bounds("mo-trash-restore").is_some(),
        "有选中时应出现还原按钮"
    );

    // ③ 重命名默认键（Finder=Enter / 资源管理器=F2）：弹卡并预填游标行的当前名。
    assert_eq!(
        names(&mut vcx, &window),
        vec!["demo-f2.txt", "demo-f1.txt", "demo-f0.txt"]
    );
    cx.simulate_keystrokes(window.into(), RENAME_KEY);
    redraw(&mut vcx);
    assert!(
        vcx.debug_bounds("mo-dialog-card").is_some(),
        "{RENAME_KEY} 应弹重命名卡"
    );
    // Esc 回面板，多选保持。
    cx.simulate_keystrokes(window.into(), "escape");
    redraw(&mut vcx);
    assert!(vcx.debug_bounds("mo-dialog-card").is_none());
    assert_eq!(sel(&mut vcx, &window), vec![0]);

    // ④ 再进重命名卡：清空预填 → 输入新名 → Enter 提交。
    cx.simulate_keystrokes(window.into(), RENAME_KEY);
    redraw(&mut vcx);
    cx.simulate_keystrokes(
        window.into(),
        "backspace backspace backspace backspace backspace backspace backspace backspace backspace backspace backspace",
    );
    cx.simulate_keystrokes(window.into(), "r e n a m e d . t x t");
    cx.simulate_keystrokes(window.into(), "enter");
    for _ in 0..20 {
        redraw(&mut vcx);
        if names(&mut vcx, &window)[0] == "renamed.txt" {
            break;
        }
    }
    assert_eq!(
        names(&mut vcx, &window),
        vec!["renamed.txt", "demo-f1.txt", "demo-f0.txt"],
        "提交后面板列表应显示新名"
    );
    // 实际落点文件也改了名（旧路径没了、新路径在）。
    assert!(!seed.join("seed-2").join("f2.txt").exists());
    assert!(seed.join("seed-2").join("renamed.txt").exists());
}

/// 回收站列表视图要有**表头**（名称 / 列表同源的四列），且**点空白清选**
/// （用户报：回收站点空白不会取消选中文件）。
#[gpui_kit::test]
fn trash_header_and_blank_click_clears_selection(cx: &mut TestAppContext) {
    // 预种两条（真实落点文件，大小列才有得算）。
    let seed = std::env::temp_dir().join(format!("mo-layout-trash-header-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&seed);
    std::fs::create_dir_all(&seed).unwrap();
    let mut records = String::from("[");
    for (i, name) in ["a.txt", "b.txt"].iter().enumerate() {
        let id = format!("seed-{i}");
        let dir = seed.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), b"hello").unwrap();
        if i > 0 {
            records.push(',');
        }
        records.push_str(&format!(
            r#"{{"id":"{id}","original":"/Users/demo/{name}","trashed":"{}","is_dir":false,"at":1700000000}}"#,
            jpath(&dir.join(name))
        ));
    }
    records.push(']');
    std::fs::write(seed.join("index.json"), records).unwrap();

    let (mut vcx, window) = open_app_with_trash(size(px(1000.), px(700.)), seed, cx);
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    let sel = |vcx: &mut VisualTestContext, window: &WindowHandle<RootView>| -> Vec<usize> {
        window
            .update(&mut vcx.cx, |root, _w, _cx| {
                mo_ui::trash_selection_for_tests(root)
            })
            .expect("读回收站多选失败")
    };

    // 表头存在，且在数据行上方。
    let header = bounds(&mut vcx, "mo-trash-header");
    let row0 = bounds(&mut vcx, "mo-trash-row-0");
    assert!(
        header.origin.y + header.size.height <= row0.origin.y,
        "表头应位于数据行上方：header={header:?} row0={row0:?}"
    );

    // 点第一行 → 选中；再点最后一行之下的空白 → 清选（坐标级真实鼠标事件）。
    vcx.update(|window, cx| window.click("trash-row-0", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert_eq!(sel(&mut vcx, &window), vec![0], "点行应选中");

    let row1 = bounds(&mut vcx, "mo-trash-row-1");
    let p_blank = point(
        row1.origin.x + px(200.0),
        row1.origin.y + row1.size.height + px(30.0),
    );
    vcx.update(|window, cx| window.drag(p_blank, p_blank, cx));
    vcx.run_until_parked();
    assert!(
        sel(&mut vcx, &window).is_empty(),
        "点空白应清空选择，不是保持 / 改动选中"
    );
}

/// 回收站的视图切换要**真的生效**（用户报：工具栏切了没反应——按钮写的
/// 是浏览面板的状态，回收站渲染不读）：List 有表头有行；Grid / Gallery 变
/// 格子；列视图置灰不接；切回 List 一切复原。
#[gpui_kit::test]
fn trash_view_switch_really_switches(cx: &mut TestAppContext) {
    let seed = std::env::temp_dir().join(format!("mo-layout-trash-view-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&seed);
    std::fs::create_dir_all(&seed).unwrap();
    let mut records = String::from("[");
    for i in 0..2 {
        let id = format!("seed-{i}");
        let name = format!("f{i}.txt");
        let dir = seed.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&name), b"x").unwrap();
        if i > 0 {
            records.push(',');
        }
        records.push_str(&format!(
            r#"{{"id":"{id}","original":"{}/demo-{name}","trashed":"{}","is_dir":false,"at":1700000000}}"#,
            jpath(&seed),
            jpath(&dir.join(&name))
        ));
    }
    records.push(']');
    std::fs::write(seed.join("index.json"), records).unwrap();

    let (mut vcx, window) = open_app_with_trash(size(px(1000.), px(700.)), seed, cx);
    vcx.update(|window, cx| window.click("sidebar-trash", cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    let redraw = |vcx: &mut VisualTestContext| {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
    };
    let mode = |vcx: &mut VisualTestContext, window: &WindowHandle<RootView>| -> &'static str {
        window
            .update(&mut vcx.cx, |root, _w, _cx| {
                mo_ui::trash_view_mode_for_tests(root)
            })
            .expect("读回收站视图模式失败")
    };

    // 初始 List：行 + 表头都在，格子不存在。
    assert_eq!(mode(&mut vcx, &window), "list");
    assert!(vcx.debug_bounds("mo-trash-row-0").is_some());
    assert!(vcx.debug_bounds("mo-trash-header").is_some());
    assert!(vcx.debug_bounds("mo-trash-cell-0").is_none());

    // 切网格：格子出现，行 / 表头退场。
    vcx.update(|window, cx| window.click("view-mode-grid", cx));
    redraw(&mut vcx);
    assert_eq!(mode(&mut vcx, &window), "grid", "点网格按钮应切到 grid");
    assert!(
        vcx.debug_bounds("mo-trash-cell-0").is_some(),
        "网格下应画格子"
    );
    assert!(
        vcx.debug_bounds("mo-trash-row-0").is_none(),
        "网格下不该还有表格行"
    );
    assert!(
        vcx.debug_bounds("mo-trash-header").is_none(),
        "网格没有列，不该有表头"
    );

    // 切画廊：仍是格子。
    vcx.update(|window, cx| window.click("view-mode-gallery", cx));
    redraw(&mut vcx);
    assert_eq!(mode(&mut vcx, &window), "gallery");
    assert!(vcx.debug_bounds("mo-trash-cell-0").is_some());

    // 列视图置灰：点了不切。
    vcx.update(|window, cx| window.click("view-mode-columns", cx));
    redraw(&mut vcx);
    assert_eq!(
        mode(&mut vcx, &window),
        "gallery",
        "列视图对回收站无意义，点击不应生效"
    );

    // 切回列表：行 + 表头复原。
    vcx.update(|window, cx| window.click("view-mode-list", cx));
    redraw(&mut vcx);
    assert_eq!(mode(&mut vcx, &window), "list");
    assert!(vcx.debug_bounds("mo-trash-row-0").is_some());
    assert!(vcx.debug_bounds("mo-trash-header").is_some());
}
