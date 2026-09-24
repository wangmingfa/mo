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
use gpui_kit::{px, size, Bounds, Pixels, Size, TestAppContext, VisualTestContext, WindowHandle};
use mo_app::AppState;
use mo_ui::RootView;

/// 以指定窗口尺寸启动一个 headless 窗口，返回可查询布局的上下文与窗口句柄。
fn open_app(
    window_size: Size<Pixels>,
    cx: &mut TestAppContext,
) -> (VisualTestContext, WindowHandle<RootView>) {
    // 钉住配置目录到临时路径：否则视图模式 / 侧边栏开关这些布局偏好会读到人家的
    // 真实 config.json，同一份代码在不同机器上渲染结构不同。
    mo_ui::isolate_config_for_tests();
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
    let trash_root = std::env::temp_dir().join(format!("mo-layout-trash-{}", std::process::id()));
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

// ── 传输指示（左下角小块 + 浮层）与回收站入口 ────────────────────────────

use mo_operations::{OperationHandle, OperationStatus};

/// 造一条假操作快照（真操作要走 OperationManager 的后台执行链路，headless 拉不动）。
/// `pausable = false`：删除这类快操作；传输类（复制 / 移动）快照才带暂停能力。
fn fake_op(id: u64, status: OperationStatus, done: u64, total: u64) -> OperationHandle {
    OperationHandle {
        id,
        describe: format!("删除（回收站）/Users/demo/file-{id}"),
        status,
        progress: (done, total),
        pausable: false,
    }
}

fn inject_ops(window: &WindowHandle<RootView>, cx: &mut TestAppContext, ops: Vec<OperationHandle>) {
    window
        .update(cx, |root, _window, _cx| {
            mo_ui::inject_ops_for_tests(root, ops);
        })
        .expect("注入操作快照失败");
}

/// 传输任务必须收成**左下角统一任务面板**（折叠态），不能再是整条横幅把状态栏
/// 顶上去，也不能是悬空浮层加底下一份重复小块。
///
/// 折叠态：面板宽不超过 310px、贴在状态栏上方，且状态栏高度不受任务影响。
#[gpui_kit::test]
fn transfers_render_as_a_corner_badge_above_the_status_bar(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);
    let baseline = bounds(&mut vcx, "mo-statusbar");

    inject_ops(
        &window,
        cx,
        vec![
            fake_op(1, OperationStatus::Running, 3, 10),
            fake_op(2, OperationStatus::Completed, 5, 5),
        ],
    );
    vcx.update(|window, cx| window.render_frame(cx));

    let badge = bounds(&mut vcx, "mo-ops-badge");
    assert!(
        f32::from(badge.size.width) <= 310.0,
        "折叠面板宽 {}：又铺回横幅了",
        badge.size.width
    );
    let status = bounds(&mut vcx, "mo-statusbar");
    assert_eq!(
        status, baseline,
        "有任务之后状态栏位置/大小变了：面板不该占布局"
    );
    assert!(
        f32::from(badge.origin.y) + f32::from(badge.size.height) <= f32::from(status.origin.y),
        "面板应悬在状态栏上方：badge={badge:?} status={status:?}"
    );
    // 默认折叠。
    assert!(vcx.debug_bounds("mo-ops-popover").is_none());
}

/// 点折叠态的小块原地展开成任务列表，再点一次收起——同一张卡片，不是
/// 悬空浮层 + 底下还留一份重复小块。
#[gpui_kit::test]
fn badge_click_toggles_the_transfer_popover(cx: &mut TestAppContext) {
    let (mut vcx, window) = open_app(size(px(1000.), px(700.)), cx);

    inject_ops(
        &window,
        cx,
        vec![fake_op(1, OperationStatus::Running, 0, 4)],
    );
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-ops-badge").is_some(),
        "有任务时应画出小块"
    );

    vcx.update(|window, cx| window.click("mo-ops-badge", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    let pop = bounds(&mut vcx, "mo-ops-popover");
    assert!(
        f32::from(pop.size.width) <= 400.0,
        "展开面板宽 {}：不该铺满窗口",
        pop.size.width
    );
    // 列表在标题行（mo-ops-badge）**正下方**、同一张卡片内：左缘对齐，中间无缝。
    let badge = bounds(&mut vcx, "mo-ops-badge");
    assert!(
        f32::from(pop.origin.y) >= f32::from(badge.origin.y) + f32::from(badge.size.height),
        "任务列表应紧跟标题行下方：pop={pop:?} badge={badge:?}"
    );
    assert_eq!(
        pop.origin.x, badge.origin.x,
        "展开面板与折叠小块左缘不对齐：不像同一张卡片"
    );

    vcx.update(|window, cx| window.click("mo-ops-badge", cx));
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-ops-popover").is_none(),
        "再点一次标题行应收起任务列表"
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
    let p_blank = point(
        list.origin.x + px(200.0),
        list.origin.y + list.size.height - px(10.0),
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
