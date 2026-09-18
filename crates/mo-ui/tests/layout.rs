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
    let app = AppState::new();
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
