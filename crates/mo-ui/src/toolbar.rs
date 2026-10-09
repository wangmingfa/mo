use std::path::{Path, PathBuf};

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::Sizable as _;
use gpui_kit::*;
use mo_app::AppState;

use crate::app::SettingsTab;
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

/// 工具栏：**导航图标（后退 / 前进 / 上一级 / 刷新）+** Win11 风格地址栏
/// （面包屑 / 可编辑）+ 视图模式组。
///
/// 刷新排在地址栏**左边**、与 ←→↑ 挨着：它和那三个同属「走导航历史 / 重读当前
/// 目录」这一类动作，摆在一起才读得出是一组；地址栏右侧则整块留给视图模式那一组
/// （互斥、有外框，自成一体）。
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
    address: Option<&Entity<InputState>>,
    view_mode: ViewMode,
    in_trash: bool,
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
            let entity = entity.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), &entity, Nav::Back)
        }))
        .child(icon_button(
            "nav-forward",
            icons::ARROW_RIGHT,
            can_forward,
            {
                let app = app.clone();
                let entity = entity.clone();
                move |cx: &mut App| spawn_nav(cx, app.clone(), &entity, Nav::Forward)
            },
        ))
        .child(icon_button("nav-parent", icons::ARROW_UP, true, {
            let app = app.clone();
            let entity = entity.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), &entity, Nav::Parent)
        }))
        .child(icon_button("nav-refresh", icons::ROTATE_CW, true, {
            let app = app.clone();
            let entity = entity.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), &entity, Nav::Refresh)
        }))
        .child(address_bar(
            app,
            entity,
            path,
            address_editing,
            address,
            in_trash,
        ))
        // 视图模式：四个模式**平铺**成一排图标按钮（Finder 工具栏那组），当前模式高亮。
        .child(view_mode_buttons(view_mode, entity, in_trash))
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

// ---- 「应用菜单」按钮 + 浮层 ----------------------------------------------

/// 「应用菜单」按钮（汉堡）：贴在**标签条行末**——Chrome 同位。点开一个吸附在
/// 按钮下方的 popover（设置 / 扩展程序 / 命令面板等入口）。浮层本体由 `app.rs`
/// 的 render 挂在根容器末尾（画在最上层，同右键菜单 / 橡皮筋一套坐标约定）。
/// 按钮只管开合语义：
/// - 按下（on_mouse_down）：记下「按下那一刻菜单是否开着」，开着就立即收起
///   （浮层的 `on_mouse_down_out` 在同一次按下里也会再触发一次，幂等无害）；
///   并**止泡**——macOS 顶栏整行挂着 `attach_titlebar_drag`，按下汉堡不该顺手
///   发起窗口拖动。
/// - 抬起（on_click）：只有「按下时是关着的」才打开——否则这次按下已经把
///   菜单关掉了，再 toggle 就成了「怎么点都关不掉」。
///
/// Windows 兼容：本函数由 `render_top_row` 插在 `drag_strip` / `window_controls`
/// **之前**——Win 的最小化 / 最大化 / 关闭贴行末右缘，汉堡在它们左边，永不重叠。
pub(crate) fn app_menu_button(entity: &Entity<RootView>, menu_open: bool) -> impl IntoElement {
    let mut menu = div()
        .id("app-menu")
        .flex()
        .items_center()
        .justify_center()
        .size(px(28.0))
        .rounded(px(6.0))
        .mx(px(4.0))
        .flex_shrink_0()
        .text_color(theme::text())
        .hover(|s| s.bg(theme::hover_bg()))
        .debug_selector(|| "mo-app-menu-btn".to_string());
    if menu_open {
        menu = menu.bg(theme::hover_bg());
    }
    let press_entity = entity.clone();
    let click_entity = entity.clone();
    menu.interactivity()
        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
            // ⚠️ 止泡：外层（顶栏整行）也挂了 on_mouse_down（macOS 拖拽）。
            press_entity.update(cx, |v, cx| v.note_menu_press(cx));
            cx.stop_propagation();
        });
    menu.interactivity().on_click(move |_, _window, cx| {
        click_entity.update(cx, |v, cx| v.app_menu_clicked(cx));
    });
    menu.child(icon(icons::MENU, 16.0, theme::text()))
}

/// 浮层锚点：贴**窗口右缘**（按钮在标签条行末，浮层右对齐到按钮下），顶部对齐
/// **标签条行**底缘之外——按钮就在那一行，浮层必须吸附在它正下方。
///
/// 顶栏两行各钉死 [`TOOLBAR_HEIGHT`]（标签条 y=0..48、工具栏 y=48..96，见
/// `app.rs::render_top_row` 注释）。汉堡在第 1 行，所以 Y = 48 + 2；
/// ⚠️ 挪按钮时这里必须跟着改——锚点是常量推导，编译器不会提醒它错位。
/// 与右键菜单同一套**窗口坐标**约定——浮层由 `app.rs` 的 render 挂在根容器末尾。
pub(crate) const APP_MENU_MARGIN: f32 = 8.0;
pub(crate) const APP_MENU_Y: f32 = TOOLBAR_HEIGHT + 2.0;

/// 浮层左缘 x：视口宽 - 菜单宽 - 右缘留白（右对齐）。
pub(crate) fn app_menu_x(viewport_w: f32) -> f32 {
    viewport_w - crate::context_menu::MENU_W - APP_MENU_MARGIN
}

/// 「应用菜单」浮层里的一行。抽成枚举是为了**可单测**：行文案 / 图标可以列全，
/// 点击落到哪个动作也能在 headless 里逐行断言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppMenuAction {
    /// 打开统一设置窗口（「界面」页）。
    Settings,
    /// 打开扩展管理器独立窗口。
    Extensions,
    /// 打开命令面板。
    CommandPalette,
}

impl AppMenuAction {
    /// 行文案、图标与右侧键位提示（提示留空则不画）。键位提示与 `keys::BINDINGS`
    /// 的默认值一致——那是可重映射的运行时真值，这里只是菜单上的静态装饰。
    pub(crate) fn row(self) -> (&'static str, &'static [u8], &'static str) {
        match self {
            AppMenuAction::Settings => ("设置", icons::SETTINGS, "⌘,"),
            AppMenuAction::Extensions => ("扩展程序", icons::EXTENSION, ""),
            AppMenuAction::CommandPalette => ("命令面板", icons::SEARCH, "⇧⌘P"),
        }
    }

    /// 该行的 debug_selector（`&'static str`：headless 的 `debug_bounds` 只认
    /// 静态名，不能现拼字符串）。
    pub(crate) fn selector(self) -> &'static str {
        match self {
            AppMenuAction::Settings => "mo-app-menu-label-settings",
            AppMenuAction::Extensions => "mo-app-menu-label-extensions",
            AppMenuAction::CommandPalette => "mo-app-menu-label-cmdpalette",
        }
    }
}

/// 「应用菜单」浮层全部行，按展示顺序。测试逐行断言用。
pub(crate) const APP_MENU_ENTRIES: [AppMenuAction; 3] = [
    AppMenuAction::CommandPalette,
    AppMenuAction::Extensions,
    AppMenuAction::Settings,
];

/// 渲染「应用菜单」浮层（设置 / 扩展程序 / 命令面板）。
///
/// 由 `app.rs` 的 render 在**根容器末尾**挂上（`menu_open` 时才调）——绝对定位
/// 画在最上层、命中链最前，与右键菜单 / 橡皮筋同套路。视觉与右键菜单一致
/// （surface 底 + 分隔线描边 + 大圆角 + 阴影，行高 / 内衬复用 context_menu 常量）。
/// 点行：先收起浮层再执行动作（浮层遮着的窗口不该同时变）。
pub(crate) fn render_app_menu(entity: &Entity<RootView>, viewport_w: f32) -> impl IntoElement {
    use crate::context_menu::{ITEM_H, ITEM_RADIUS, MENU_W, PAD};

    let out_entity = entity.clone();
    let mut wrapper = div()
        .absolute()
        .left(px(app_menu_x(viewport_w)))
        .top(px(APP_MENU_Y))
        .w(px(MENU_W))
        // 阻止点击穿透到底下的文件行；`on_mouse_down_out`：点浮层外（含工具栏
        // 其它位置）收起。按钮自己的收起语义在按钮的按下 / 抬起两段里（见
        // `render` 内注释），这里不用管按钮。
        .occlude()
        .debug_selector(|| "mo-app-menu".to_string());
    wrapper
        .interactivity()
        .on_mouse_down_out(move |_, _window, cx| {
            out_entity.update(cx, |v, cx| v.close_menu_popover(cx));
        });

    let mut panel = div()
        .id("mo-app-menu-panel")
        .w_full()
        .flex()
        .flex_col()
        .p(px(PAD))
        .gap(px(1.0))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::divider())
        .rounded(px(crate::context_menu::PANEL_RADIUS))
        .shadow_lg()
        .text_size(px(13.0))
        .text_color(theme::text());

    for action in APP_MENU_ENTRIES {
        let (label, glyph, hint) = action.row();
        let row_entity = entity.clone();
        let mut row = div()
            .id(("mo-app-menu-item", action as usize))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(ITEM_H))
            .px(px(10.0))
            .rounded(px(ITEM_RADIUS))
            .hover(|s| s.bg(theme::hover_bg()))
            .child(icon(glyph, 14.0, theme::muted()))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    // 测试用（release no-op）：按动作定位每一行菜单项（静态名）。
                    .debug_selector(|| action.selector().to_string())
                    .child(text!(label)),
            );
        if !hint.is_empty() {
            row = row.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .child(text!(hint)),
            );
        }
        // 点击动作先挂、`test_support()` 最后包（headless 的 click 只认被观察元素）。
        row.interactivity().on_click(move |_, _window, cx| {
            row_entity.update(cx, |v, cx| {
                v.close_menu_popover(cx);
                match action {
                    AppMenuAction::Settings => v.open_settings(SettingsTab::Layout, cx),
                    AppMenuAction::Extensions => v.open_extensions_picker(cx),
                    AppMenuAction::CommandPalette => v.open_command_palette(cx),
                }
            });
        });
        panel = panel.child(row.test_support());
    }

    wrapper.child(panel)
}

/// 标签条尾部（「＋」之后）那条占满剩余宽度的拖拽带。
///
/// 为什么需要它：顶栏是自绘的 [`TOOLBAR_HEIGHT`] = 48px，Mac 上 AppKit
/// 只认**原生标题栏那一条带**（≈28pt），两者高度不一致时总有一截没人负责
/// 拖拽（详见 [`attach_titlebar_drag`]）。Windows 侧更实际：Wayland / X11 下
/// 自绘标题栏必须自己接，这里统一交给 Helper。
///
/// 按平台分两条路：
/// - **Windows**：打 [`WindowControlArea::Drag`]（命中测试返回 `HTCAPTION`），
///   拖拽 / 双击最大化交系统。
/// - **macOS / Linux**：`WindowControlArea` 无处落地（`gpui-pre-macos` 的
///   `on_hit_test_window_control` 是空实现），改调 [`Window::start_window_move`]，
///   内部走 `performWindowDragWithEvent:`，手感与原生标题栏一致。
///
/// ⚠️ 该矩形必须与任何可点击控件互不重叠——否则重叠处被判成标题栏、
/// 子控件收不到点击（Windows 的命中测试按祖先优先）。
pub fn drag_filler() -> impl IntoElement {
    let mut filler = div()
        .id("titlebar-drag-filler")
        .h_full()
        .min_w(px(0.0))
        .flex_grow(1.0)
        .flex_basis(px(0.0))
        // 测试用（release no-op）：tests/layout.rs 断言它吃满「＋」右侧的剩余宽度。
        .debug_selector(|| "mo-titlebar-drag".to_string());

    if cfg!(target_os = "windows") {
        filler = filler.window_control_area(WindowControlArea::Drag);
    }

    attach_titlebar_drag(filler)
}

/// 标题栏按下时应采取的窗口动作。
///
/// 抽成纯函数是为了**可单测**：Mac 接管标题栏后，单击「拖」与双击「缩放」的语义
/// 全由这里决定，藏进闭包里就没人能验证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlebarAction {
    /// 双击 → 缩放（最大化 / 还原）：补回 macOS 原生标题栏的双击语义。
    Zoom,
    /// 单击按住 → 开始拖拽窗口。
    Drag,
    /// 交给系统：Windows 的 `HTCAPTION` 已包含拖拽与双击最大化，应用层不插手。
    System,
}

/// 决定标题栏上一次按下要干什么。
///
/// `click_count` 来自 gpui 的 [`MouseDownEvent::click_count`]：连第二次按下时
/// 它是 2，据此把双击还原成缩放，而不是再发起一次拖拽。
pub fn titlebar_action(is_windows: bool, click_count: usize) -> TitlebarAction {
    if is_windows {
        TitlebarAction::System
    } else if click_count > 1 {
        TitlebarAction::Zoom
    } else {
        TitlebarAction::Drag
    }
}

/// 给顶栏（或其中一段带子）挂上自营的标题栏拖拽。
///
/// 为什么要自营：macOS 主窗口开了 `WindowOptions::app_owns_titlebar_drag`，
/// AppKit 不再参与标题栏——**好处**是消掉「点标题栏先等一拍判断是不是双击」
/// 的那段延迟；**代价**是拖拽与双击缩放都得自己实现，漏挂一处那块就彻底拖不动。
///
/// Windows 不挂：`WindowControlArea::Drag` 让系统命中测试直接返回 `HTCAPTION`，
/// 拖拽 / 双击最大化都由系统照顾，比自己拼更稳。
pub fn attach_titlebar_drag<E: InteractiveElement>(mut d: E) -> E {
    if cfg!(target_os = "windows") {
        return d;
    }
    d.interactivity()
        .on_mouse_down(MouseButton::Left, |ev, window, cx| {
            // 鼠标事件内层先于外层冒泡：这里已经处理完，不要再往上传，
            // 否则外层的同类处理器会重复发起一次拖拽。
            cx.stop_propagation();
            match titlebar_action(cfg!(target_os = "windows"), ev.click_count) {
                TitlebarAction::Zoom => window.zoom_window(),
                TitlebarAction::Drag => window.start_window_move(),
                TitlebarAction::System => {}
            }
        });
    d
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
            if is_maximized {
                "win-restore"
            } else {
                "win-maximize"
            },
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

/// 地址栏：面包屑模式（每段可点击跳转）⇄ 编辑模式（真实文本输入框）。
fn address_bar(
    app: &AppState,
    entity: &Entity<RootView>,
    path: &Option<PathBuf>,
    editing: bool,
    input: Option<&Entity<InputState>>,
    in_trash: bool,
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

    // 回收站模式：中央区已经是回收站面板，地址栏不再回显进面板前的目录
    // （用户报「进回收站地址栏没变」）——画一枚静态「回收站」胶囊，不可编辑。
    if in_trash {
        return pill.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px(px(6.0))
                .h(px(24.0))
                .text_color(theme::muted())
                .child(icon(icons::TRASH, 12.0, theme::muted()))
                .child(text!("回收站"))
                // 测试探针：回收站模式地址栏（区别于普通面包屑）。
                .debug_selector(|| "mo-address-trash".to_string()),
        );
    }

    if editing {
        // 编辑态交给框架的真实输入框：选区 / 光标定位 / 双击选词 / ⌘A / 剪切复制
        // 粘贴 / 撤销 / 中文输入法全归它，本层只负责「把它摆进胶囊里」。
        //
        // `appearance(false)` + `bordered(false)`：外框、底色、圆角仍由上面的
        // pill 画，别让输入组件自带的 shadcn 边框叠一层进来。
        // 尺寸给 `small()`（24px 高，与面包屑段一致）并显式钉死字号 13px，
        // 与列表 / 侧边栏同号；内边距清零，左右留白交给这一层。
        let mut area = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .px(px(6.0));
        if let Some(state) = input {
            area = area.child(
                Input::new(state)
                    .appearance(false)
                    .bordered(false)
                    .small()
                    .text_size(px(13.0))
                    .p(px(0.0)),
            );
        }
        return pill.child(area);
    }

    // 面包屑模式：一段一个可点击胶囊，中间夹「›」分隔符。
    if let Some(p) = path {
        // **正在看**远程时：地址栏回显完整 URL（当前远程路径替换原路径）。
        // 用 `remote_url()`（**正在浏览**的那条）而不是 `live_connections()`（活着的
        // 全部连接）——切回本地后连接还活着，那时地址栏该显示本地路径，而不是拿本地
        // 路径去拼一个远程 URL。
        // 整段可点击进入编辑（填新路径即在远程内跳转，填 `scheme://` 则切服务器）。
        if let Some(url) = app.remote_url() {
            // `address_at` 不回显用户名：地址栏要的是「哪台机器的哪个目录」，
            // 用户名只参与登录（用户明确要求过别显示）。
            let full = url.address_at(&p.display().to_string());
            let entity_edit = entity.clone();
            let mut seg = div()
                .id(("crumb", 0usize))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(5.0))
                .h(px(24.0))
                .overflow_hidden()
                .rounded(px(5.0))
                .flex_shrink_0()
                .text_color(theme::text())
                .hover(|s| s.bg(theme::hover_bg()));
            seg.interactivity().on_click(move |_, window, cx| {
                cx.stop_propagation();
                entity_edit.update(cx, |v, cx| v.begin_address_edit(window, cx));
            });
            seg = seg.child(div().flex_1().min_w_0().truncate().child(text!(full)));
            pill = pill.child(seg);
        } else {
            let segs = segments(p);
            let n = segs.len();
            for (i, (label, prefix)) in segs.into_iter().enumerate() {
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

                // 段一律显示原始名称文本（不再给 Home 段换房子图标）。
                seg = seg.child(text!(label));
                pill = pill.child(seg);
            }
        }
    }

    // 尾部空白：点击进入编辑态（Win11 行为）——地址栏唯一的编辑入口，
    // 不再另设铅笔按钮。
    let mut blank = div().id("addr-blank").flex_1().h_full().min_w(px(24.0));
    start_edit_on_click(&mut blank, entity);
    pill.child(blank)
}

/// 给一个元素挂上「点击进入地址编辑态」的回调。
///
/// 泛型以同时接受 `Div` 与加了 ID 后的 `Stateful<Div>`。
fn start_edit_on_click<E: InteractiveElement>(el: &mut E, entity: &Entity<RootView>) {
    let entity = entity.clone();
    el.interactivity().on_click(move |_, window, cx| {
        // 创建 / 聚焦输入框需要 `Window`（InputState::new / focus 都要）。
        entity.update(cx, |v, cx| v.begin_address_edit(window, cx));
    });
}

/// 把路径拆成可点击的层级段：`(显示名, 前缀路径)`。
///
/// Windows 的层级与资源管理器一致：**此电脑 › 盘符 › 各级目录**——
/// * 首段固定「此电脑」，指向空路径哨兵（mo-fs 据此列盘符，见
///   `LocalFileSystem::read_dir_blocking`）；
/// * `Component::Prefix`（`C:`）与紧跟的 `RootDir` 合并为一段
///   「C:」（前缀路径归一为 `C:\`），否则会把 macOS 的根标签
///   「Mac」错误地混进 Windows 路径里。
///
/// macOS 维持原样：根目录 `/` 显示为「Mac」。
fn segments(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();

    // 「此电脑」是 Windows 面包屑的固定第一级。
    #[cfg(target_os = "windows")]
    out.push(("此电脑".to_string(), PathBuf::new()));

    let mut prefix = PathBuf::new();
    let mut comps = path.components().peekable();
    while let Some(comp) = comps.next() {
        match comp {
            // 盘符前缀（含 UNC）：与紧随的根分隔符合并成一段。
            std::path::Component::Prefix(_) => {
                prefix.push(comp.as_os_str());
                if matches!(comps.peek(), Some(std::path::Component::RootDir)) {
                    comps.next();
                    // 「C:」单独作为前缀是「相对当前目录」语义（join 会得 C:Users），
                    // 必须带上根分隔符归一成「C:\」。
                    prefix.push(std::path::MAIN_SEPARATOR.to_string());
                }
                let label = comp.as_os_str().to_string_lossy().to_string();
                out.push((label, prefix.clone()));
            }
            // unix 根：保持历史行为（根 = Mac）。
            std::path::Component::RootDir => {
                prefix.push("/");
                // Windows 正常走不到这里：盘符路径在 Prefix 分支合并掉了，无盘符根
                // 也已在 `resolve_address_input` 归一成「正在看的盘」。万一真走到
                // （历史 / 书签里存了无盘符路径），显示分隔符本身，别把 macOS 的
                // 「Mac」泄漏过来（2026-09-29 用户截图里的「此电脑 › Mac」就是它）。
                #[cfg(target_os = "windows")]
                out.push(("\\".to_string(), prefix.clone()));
                #[cfg(not(target_os = "windows"))]
                out.push(("Mac".to_string(), prefix.clone()));
            }
            _ => {
                prefix.push(comp);
                // 照抄磁盘上的目录名。2026-09-29 用户决定：地址栏显示**原样名字**
                //（「主目录」这类中文标签只留给侧边栏），是什么文件夹就显示什么。
                let label = crate::path_label::last_segment(&prefix);
                out.push((label, prefix.clone()));
            }
        }
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
fn spawn_nav(cx: &mut App, app: AppState, entity: &Entity<RootView>, nav: Nav) {
    // 工具栏导航从次级视图（回收站 / 全局搜索…）发起时，先把面板退回浏览态
    // ——与侧栏同一约定：导航 = 离开次级视图。
    entity.update(cx, |v, cx| v.leave_secondary_view(cx));
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
        .flex_shrink_0()
        // 测试用（release no-op）：按 `mo-icon-<id>` 取几何，用来钉住**这一排按钮
        // 的先后顺序**（刷新在地址栏左边，见 tests/layout.rs）。挪动顺序时别漏了它。
        .debug_selector(move || format!("mo-icon-{id}"));

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

/// 视图模式按钮组的固定高度：与地址栏那枚胶囊取同一值，工具栏两处控件等高。
pub(crate) const GROUP_HEIGHT: f32 = 32.0;

/// 外框内衬：高亮块与描边之间的留白。
pub(crate) const GROUP_PAD: f32 = 2.0;

/// 组内单枚按钮的边长（`GROUP_HEIGHT` 减掉上下内衬与描边）。
pub(crate) const BUTTON_SIZE: f32 = 26.0;

/// 组内相邻按钮的间距。
pub(crate) const BUTTON_GAP: f32 = 2.0;

/// 高亮指示块的弹簧参数：略欠阻尼（ζ ≈ 0.76），滑到位带一点点回弹，手感与系统
/// 分段控件一致；ω₀ ≈ 26 rad/s，最远的第 4 段（位移 84px）约 250ms 落定。
const SLIDE_SPRING: SpringConfig = SpringConfig::new(700.0, 40.0, 1.0);

/// 弹簧落定公差（px）。默认的 0.001 对 84px 位移要跑 600ms —— 肉眼早就到位了，
/// 却还在逐帧请求重绘、白烧 300ms 的电；放到半像素，看不出停在哪一步。
const SLIDE_EPSILON: f32 = 0.5;

/// 第 `index` 段按钮左边缘相对**内容区**起点的偏移。
///
/// 与 `gap` 共用 [`BUTTON_GAP`]，所以指示块和按钮一定落在同一列上 ——
/// 二者错一次 1px 就会被 `view_mode_buttons_are_tiled_with_exactly_one_active`
/// 的「同位」断言抓住。
pub(crate) fn segment_offset(index: usize) -> f32 {
    index as f32 * (BUTTON_SIZE + BUTTON_GAP)
}

/// 视图模式按钮组：四个模式**平铺**成一排图标按钮，当前模式高亮。
///
/// 单个按钮是「点击切到下一个模式」的循环式（原来那个显示模式名的按钮），
/// 用户看不出还有哪些模式、也点不准想去的那个。现在按 [`ViewMode::ALL`] 平铺
/// （顺序与 `⌘1..⌘4` 一致），点哪个切哪个。
///
/// 高亮**不是画在按钮上**的，而是下面一个独立的滑块（[`SLIDE_SPRING`] 驱动）：
/// 切换视图时它从旧的一段滑到新的一段 —— 读起来是「我把指针挪到了第几段」，
/// 而不是「这里刚换了张图」。底色用 `theme::accent()` 而不是 `hover_bg()`：
/// 工具栏底色是 `container()`（浅色下 `#f6f6f7`），而 `hover_bg()` 是 `#f0f0f1`
/// —— 差 6/255，看不出「当前在哪个视图」。图标色同时从 `muted()` 提到 `text()`，
/// 未选中的才压暗。
///
/// **整组共用一条外框**（`separator()` 描边 + `surface()` 底 + [`GROUP_PAD`] 内衬），
/// 读起来是「一个四段的切换控件」而不是四个各自独立的按钮 —— 没有框时它们和旁边
/// 的刷新 / 搜索按钮长得一模一样，看不出这四个是**互斥**的一组。外框用的是地址栏
/// 那枚胶囊同一套描边（`separator()` + `rounded(8)`），工具栏里两处控件因此统一。
fn view_mode_buttons(
    mode: ViewMode,
    entity: &Entity<RootView>,
    in_trash: bool,
) -> impl IntoElement {
    let active_index = ViewMode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
    let mut group = div()
        .id("view-mode-group")
        // 指示块是 absolute：需要一个定位上下文。
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(BUTTON_GAP))
        // 内衬：让高亮块与描边之间留出呼吸，也让「选中块」看起来是**嵌在**框里。
        .p(px(GROUP_PAD))
        .h(px(GROUP_HEIGHT))
        .rounded(px(8.0))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::separator())
        // 地址栏是弹性的，按钮组必须固定：窗口窄时不许被压扁。
        .flex_shrink_0()
        .debug_selector(|| "mo-view-mode-group".to_string())
        // 高亮块**先**画（gpui 按 child 顺序绘制，先画的在下层），四枚按钮压在上面 ——
        // 反过来那块灰底会盖掉选中那枚的图标。
        //
        // 它是全局唯一的一个元素（`ElementId` 固定），`with_spring` 因此跨帧保留
        // **位置与速度**：切换视图时不重建，只把 target 挪一格，块自己从旧位置滑
        // 过去（连点两下也不会跳，速度接着用）。首帧直接停在 target（框架约定），
        // 所以进应用时没有入场滑动。`reduce_motion` 下框架会直接跳到 target。
        .child(
            div()
                .absolute()
                .top(px(GROUP_PAD))
                .size(px(BUTTON_SIZE))
                .rounded(px(6.0))
                .bg(theme::accent())
                .debug_selector(|| "mo-view-indicator".to_string())
                .with_spring(
                    ElementId::Name("view-mode-indicator".into()),
                    SpringAnimation::new(SLIDE_SPRING)
                        .with_epsilon(SLIDE_EPSILON)
                        .to(px(GROUP_PAD + segment_offset(active_index))),
                    |el, x| el.left(x),
                ),
        );

    for (index, m) in ViewMode::ALL.into_iter().enumerate() {
        let active = index == active_index;
        let key = m.key();
        // 回收站里列视图无意义（平铺列表没有层级可逐级展开）：置灰 + 不接点击。
        // 其余三枚照常——回收站面板有自己的视图状态（`trash_view_mode`）。
        let disabled = in_trash && m == ViewMode::Columns;
        let mut button = div()
            // ⚠️ 每个按钮都要唯一 ID：同一 `text!`/`svg` 站点会重复渲染多次，
            // 没有 ID 链就会产生相同的 a11y NodeId。
            .id(format!("view-mode-{key}"))
            .test_support()
            .flex()
            .items_center()
            .justify_center()
            .size(px(BUTTON_SIZE))
            .rounded(px(6.0))
            .flex_shrink_0()
            // 测试用（release no-op）：断言四个按钮平铺且只有一个有底色。
            .debug_selector(move || format!("mo-view-mode-{key}"));
        // 选中的那枚**不画**底色：高亮归下层那个会滑动的指示块，否则会叠出
        // 「一块跟着滑、一块死死钉在原处」的双影。
        if !active && !disabled {
            button = button.hover(|s| s.bg(theme::hover_bg()));
        }
        if !disabled {
            let target = entity.clone();
            button.interactivity().on_click(move |_, _window, cx| {
                target.update(cx, |v, cx| {
                    // 回收站面板开着时切的是回收站的视图，不是背后那个目录的。
                    let current = if v.is_in_trash() {
                        v.trash_view_mode
                    } else {
                        v.panel().view_mode
                    };
                    if current != m {
                        v.set_view_mode_for_active_surface(m);
                        cx.notify();
                    }
                });
                // 点按钮属于「点进控件」：别让事件继续冒泡到顶栏的拖拽 / 双击缩放。
                cx.stop_propagation();
            });
        }
        group = group.child(button.child(icons::icon(
            icons::view_mode_icon(m),
            16.0,
            if active {
                theme::text()
            } else if disabled {
                // 禁用档：比未选中再暗一截，一眼读出「点不动」。
                crate::theme::separator()
            } else {
                theme::muted()
            },
        )));
    }
    group
}

#[cfg(test)]
mod tests {
    // 显式列出，不能用 `use super::*`：那会把 `gpui_kit::*` 一并引进来，
    // 其中的 `test` 模块会让 `#[test]` 属性解析成它自己…… 报
    // “recursion limit reached while expanding #[test]”。
    use super::{segments, titlebar_action, TitlebarAction};

    /// Mac 自营标题栏：单击拖窗口，双击缩放（补回 AppKit 原本管的双击语义）。
    #[test]
    fn mac_single_click_drags_and_double_click_zooms() {
        assert_eq!(titlebar_action(false, 1), TitlebarAction::Drag);
        assert_eq!(titlebar_action(false, 2), TitlebarAction::Zoom);
        assert_eq!(titlebar_action(false, 3), TitlebarAction::Zoom);
    }

    /// Windows 一律交系统：`HTCAPTION` 已经包含拖拽与双击最大化。
    #[test]
    fn windows_hands_everything_to_the_system() {
        assert_eq!(titlebar_action(true, 1), TitlebarAction::System);
        assert_eq!(titlebar_action(true, 2), TitlebarAction::System);
    }

    /// 面包屑显示磁盘上的**原样目录名**：已知文件夹（主目录 / 桌面…）不再换成
    /// 侧栏那套中文标签——2026-09-29 用户决定，地址栏里「是什么名字就显示什么」。
    #[test]
    fn breadcrumbs_show_the_real_folder_name() {
        // 拿真机的已知文件夹表问，不写死路径（桌面在有的机器上被重定向到别的盘）。
        for (path, label) in mo_app::known_folder_labels() {
            let segs = segments(path);
            let (last, _) = segs.last().expect("非空路径至少有一段");
            assert_ne!(
                last.as_str(),
                *label,
                "{path:?} 的面包屑不该再显示中文标签「{label}」"
            );
            assert_eq!(
                last.as_str(),
                crate::path_label::last_segment(path),
                "{path:?} 的面包屑应显示目录本名"
            );
        }
    }
}
