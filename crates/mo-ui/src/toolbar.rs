use std::path::{Path, PathBuf};

use gpui_kit::*;
use mo_app::AppState;

use crate::icons::{self, icon};
use crate::panel::ViewMode;
use crate::theme;
use crate::RootView;

/// 工具栏固定高度。红绿灯的垂直位置由它推导（见 lib.rs），二者必须一致。
pub const TOOLBAR_HEIGHT: f32 = 48.0;

/// macOS 红绿灯标准按钮的 **frame 高度**（可见圆点 12pt 居中于 16pt 模板内）。
/// gpui 定位的是 frame 原点：按钮中心 = pos.y + frame/2。
pub const TRAFFIC_LIGHT_FRAME: f32 = 16.0;

/// 沉浸式红绿灯位置：与固定高度工具栏垂直居中，横向留 14px。
///
/// lib.rs 的 `traffic_light_position` 必须使用本值——红绿灯是 AppKit 画的，
/// 不参与 GPUI 布局，位置错了只能改这里。
pub fn traffic_light_position() -> (f32, f32) {
    (14.0, (TOOLBAR_HEIGHT - TRAFFIC_LIGHT_FRAME) / 2.0)
}

/// 工具栏：导航图标 + Win11 风格地址栏（面包屑 / 可编辑）+ 刷新。
///
/// 按钮只负责**发命令**（调用 [`AppState`] 的导航方法），不负责刷新列表：
/// 状态变化由事件总线广播，UI 快照在 `RootView` 里统一同步。
///
/// 窗口控制按钮（最小化 / 最大化 / 关闭）与 macOS 红绿灯已上移到 `RootView` 的
/// 顶部标签页行（`render_top_row`），本函数只负责导航 + 地址栏这一行。
#[allow(clippy::too_many_arguments)]
pub fn render(
    app: &AppState,
    entity: &Entity<RootView>,
    can_back: bool,
    can_forward: bool,
    path: &Option<PathBuf>,
    address_editing: bool,
    address_input: &str,
    view_mode: ViewMode,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .h(px(TOOLBAR_HEIGHT))
        .flex_shrink_0()
        .pl(px(8.0))
        .pr(px(8.0))
        .bg(theme::container())
        .border_b_1()
        .border_color(theme::separator())
        // 测试用（release no-op）：tests/layout.rs 断言工具栏只有一行高
        .debug_selector(|| "mo-toolbar".to_string())
        .child(icon_button("nav-back", icons::ARROW_LEFT, can_back, {
            let app = app.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), Nav::Back)
        }))
        .child(icon_button(
            "nav-forward",
            icons::ARROW_RIGHT,
            can_forward,
            {
                let app = app.clone();
                move |cx: &mut App| spawn_nav(cx, app.clone(), Nav::Forward)
            },
        ))
        .child(icon_button("nav-parent", icons::ARROW_UP, true, {
            let app = app.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), Nav::Parent)
        }))
        .child(address_bar(
            app,
            entity,
            path,
            address_editing,
            address_input,
        ))
        .child(icon_button("nav-refresh", icons::ROTATE_CW, true, {
            let app = app.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), Nav::Refresh)
        }))
        // 视图模式：点击在列表 / 网格 / 画廊 / 列视图之间循环。
        .child(view_mode_button(view_mode, entity))
}

/// 顶栏里一段可拖拽的空白条带（仅 Windows / Linux 使用）。
///
/// 给它打 [`WindowControlArea::Drag`]：Windows 命中测试返回 `HTCAPTION`，
/// 按住即可拖动窗口、双击最大化 / 还原，均交系统处理。⚠️ 该矩形必须与任何可点击
/// 控件互不重叠——否则重叠处被判成标题栏、子控件收不到点击（命中测试按祖先优先）。
pub fn drag_strip() -> impl IntoElement {
    div()
        .id("titlebar-drag")
        .h_full()
        .min_w(px(24.0))
        .flex_grow(1.0)
        .flex_basis(px(0.0))
        .window_control_area(WindowControlArea::Drag)
}

/// 右缘的窗口控制按钮：最小化 / 最大化（或还原）/ 关闭。
pub fn window_controls(is_maximized: bool) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h_full()
        .child(control_button(
            "win-minimize",
            icons::WIN_MINIMIZE,
            WindowControlArea::Min,
            false,
            |w: &mut Window| w.minimize_window(),
        ))
        .child(control_button(
            if is_maximized { "win-restore" } else { "win-maximize" },
            if is_maximized {
                icons::WIN_RESTORE
            } else {
                icons::WIN_MAXIMIZE
            },
            WindowControlArea::Max,
            false,
            |w: &mut Window| w.zoom_window(),
        ))
        .child(control_button(
            "win-close",
            icons::WIN_CLOSE,
            WindowControlArea::Close,
            true,
            |w: &mut Window| w.remove_window(),
        ))
}

/// 一个窗口控制按钮。
///
/// - Windows：打 [`WindowControlArea`]，点击由系统按 `HTMINBUTTON/HTMAXBUTTON/HTCLOSE`
///   原生处理（含 Win11 贴边分屏吸附），故**不挂 `on_click`**（非客户区点击不会走到这里）。
/// - Linux：无对应命中区，退回到 `on_click` 手动调用窗口动作。
/// - 悬停高亮两种平台都由 GPUI 的 `.hover()` 自绘；关闭键悬停变红。
fn control_button(
    id: &'static str,
    data: &'static [u8],
    area: WindowControlArea,
    close: bool,
    action: impl Fn(&mut Window) + 'static,
) -> Stateful<Div> {
    let mut button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(46.0))
        .h_full()
        .flex_shrink_0()
        .text_color(theme::text());

    // hover / 按压底色单独给一档更明显的中性灰（全局 theme::hover_bg 与工具栏底色
    // 太接近，几乎看不出）；关闭键沿用 Win11 红，并按压态再压深一档。
    button = if close {
        button
            .hover(|s| s.bg(rgb(0xc42b1c)).text_color(rgb(0xffffff)))
            .active(|s| s.bg(rgb(0xb0251a)).text_color(rgb(0xffffff)))
    } else {
        button
            .hover(|s| s.bg(rgb(0xe2e2e5)))
            .active(|s| s.bg(rgb(0xd0d0d4)))
    };

    if cfg!(target_os = "windows") {
        button = button.window_control_area(area);
    } else if cfg!(target_os = "linux") {
        button
            .interactivity()
            .on_click(move |_, window, _cx| action(window));
    }

    button.child(Glyph(data))
}

/// 继承按钮 `text_color` 绘制的图标。
///
/// gpui 的 `svg()` 不自动继承文字色（`style.text.color` 为空即不绘制），但父 div
/// 计算后的文字样式（含 `.hover()` 覆盖）会在绘制子元素前压入窗口的 text_style 栈。
/// 这里在 render 时读取 `window.text_style().color` 显式赋给 SVG——与 `gpui_component::Icon`
/// 同法。于是关闭键 hover 时父级把前景设为白，✕ 便随之变白；常态则继承深色前景。
#[derive(IntoElement)]
struct Glyph(&'static [u8]);

impl RenderOnce for Glyph {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        svg()
            .data(self.0)
            .w(px(16.0))
            .h(px(16.0))
            .text_color(window.text_style().color)
    }
}

/// 地址栏：面包屑模式（每段可点击跳转）⇄ 编辑模式（整行输入路径）。
fn address_bar(
    app: &AppState,
    entity: &Entity<RootView>,
    path: &Option<PathBuf>,
    editing: bool,
    input: &str,
) -> Div {
    let mut pill = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_1()
        .h(px(32.0))
        .px(px(4.0))
        .rounded(px(8.0))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::separator())
        .overflow_hidden()
        // 测试探针：断言地址栏在工具栏内。
        .debug_selector(|| "mo-address".to_string());

    if editing {
        // 编辑态：显示输入中的路径（▏为光标）。
        let display = if input.is_empty() {
            "输入路径，回车跳转".to_string()
        } else {
            format!("{input}▏")
        };
        return pill.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .px(px(6.0))
                .text_color(if input.is_empty() {
                    theme::muted()
                } else {
                    theme::text()
                })
                .child(div().flex_1().truncate().child(text!(display))),
        );
    }

    // 面包屑模式：一段一个可点击胶囊，中间夹「›」分隔符。
    if let Some(p) = path {
        let segs = segments(p);
        let n = segs.len();
        for (i, (label, prefix, is_home)) in segs.into_iter().enumerate() {
            let is_last = i + 1 == n;
            if i > 0 {
                pill = pill.child(icon(icons::CHEVRON_RIGHT, 12.0, theme::muted()));
            }
            let seg_app = app.clone();
            let seg_path = prefix.clone();
            let mut seg = div()
                .id(("crumb", i))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(5.0))
                // 高度钉死：pill 高 32，段高 24 → hover 底色上下各留 4px，
                // 不会顶到外框（文字行高会浮动，不定高就会贴边）。
                .h(px(24.0))
                .overflow_hidden()
                .rounded(px(5.0))
                .flex_shrink_0()
                .text_color(if is_last {
                    theme::text()
                } else {
                    theme::muted()
                })
                .hover(|s| s.bg(theme::hover_bg()));

            seg.interactivity().on_click(move |_, _window, cx| {
                // 阻断冒泡：别让外层「点击空白进入编辑」误触发。
                cx.stop_propagation();
                let app = seg_app.clone();
                let target = seg_path.clone();
                cx.spawn(async move |_cx| {
                    let _ = app.open_directory(&target).await;
                })
                .detach();
            });

            seg = if is_home {
                seg.child(icon(icons::HOUSE, 14.0, theme::muted()))
            } else {
                seg.child(text!(label))
            };
            pill = pill.child(seg);
        }
    }

    // 尾部空白：点击进入编辑态（Win11 行为）。
    let mut blank = div().id("addr-blank").flex_1().h_full().min_w(px(24.0));
    start_edit_on_click(&mut blank, entity);
    pill = pill.child(blank);

    // 铅笔按钮：显式的编辑入口。
    let mut edit_btn = div()
        .id("addr-edit")
        .flex()
        .items_center()
        .p(px(4.0))
        .rounded(px(5.0))
        .flex_shrink_0()
        .hover(|s| s.bg(theme::hover_bg()));
    start_edit_on_click(&mut edit_btn, entity);
    pill.child(edit_btn.child(icon(icons::PENCIL, 13.0, theme::muted())))
}

/// 给一个元素挂上「点击进入地址编辑态」的回调。
///
/// 泛型以同时接受 `Div` 与加了 ID 后的 `Stateful<Div>`。
fn start_edit_on_click<E: InteractiveElement>(el: &mut E, entity: &Entity<RootView>) {
    let entity = entity.clone();
    el.interactivity().on_click(move |_, _window, cx| {
        entity.update(cx, |v, cx| {
            v.begin_address_edit();
            cx.notify();
        });
    });
}

/// 把路径拆成可点击的层级段：`(显示名, 前缀路径, 是否 Home)`。
fn segments(path: &Path) -> Vec<(String, PathBuf, bool)> {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let mut out = Vec::new();
    let mut prefix = PathBuf::new();
    for comp in path.components() {
        if comp.as_os_str() == "/" {
            prefix.push("/");
        } else {
            prefix.push(comp);
        }
        let label = match comp {
            std::path::Component::RootDir => "Mac".to_string(),
            c => c.as_os_str().to_string_lossy().to_string(),
        };
        let is_home = home.as_ref().is_some_and(|h| h == &prefix);
        out.push((label, prefix.clone(), is_home));
    }
    out
}

/// 工具栏可触发的导航动作。
enum Nav {
    Back,
    Forward,
    Parent,
    Refresh,
}

/// 在应用上下文里派发一个导航命令，结果通过事件总线回灌到 UI。
fn spawn_nav(cx: &mut App, app: AppState, nav: Nav) {
    cx.spawn(async move |_cx| {
        let result = match nav {
            Nav::Back => app.go_back().await,
            Nav::Forward => app.go_forward().await,
            Nav::Parent => app.open_parent().await,
            Nav::Refresh => app.refresh().await,
        };
        if let Err(e) = result {
            eprintln!("导航失败: {e}");
        }
    })
    .detach();
}

/// 一个纯图标按钮；`enabled == false` 时置灰且不挂点击回调。
///
/// ⚠️ 必须有元素 ID：gpui 的 click 事件分发依赖 element_state，
/// 无 ID 的裸 div 拿不到 state，on_click 永远不会触发。
fn icon_button(
    id: &'static str,
    data: &'static [u8],
    enabled: bool,
    on_click: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    let mut button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(28.0))
        .rounded(px(6.0))
        .flex_shrink_0();

    if enabled {
        button = button.hover(|s| s.bg(theme::hover_bg()));
        // imperative API：`Div` 只实现 `InteractiveElement`（提供 `interactivity()`）。
        button
            .interactivity()
            .on_click(move |_, _window, cx| on_click(cx));
        button.child(icon(data, 16.0, theme::text()))
    } else {
        button.opacity(0.35).child(icon(data, 16.0, theme::muted()))
    }
}

/// 视图模式按钮：显示当前模式名，点击切到下一个模式。
fn view_mode_button(mode: ViewMode, entity: &Entity<RootView>) -> Stateful<Div> {
    let next = mode.next();
    let mut button = div()
        .id("view-mode")
        .flex()
        .items_center()
        .justify_center()
        .px(px(8.0))
        .h(px(28.0))
        .rounded(px(6.0))
        .flex_shrink_0()
        .text_size(px(12.0))
        .text_color(theme::muted())
        .hover(|s| s.bg(theme::hover_bg()));
    let cycle = entity.clone();
    button.interactivity().on_click(move |_, _window, cx| {
        cycle.update(cx, |v, cx| {
            v.panel_mut().view_mode = next;
            cx.notify();
        });
    });
    button.child(text!(mode.label().to_string()))
}
