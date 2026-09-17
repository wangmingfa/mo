use gpui_kit::*;
use mo_app::AppState;

/// 工具栏：后退 / 前进 / 上级 / 刷新。
///
/// 按钮只负责**发命令**（调用 [`AppState`] 的导航方法），不负责刷新列表：
/// 状态变化由事件总线广播，UI 快照在 `RootView` 里统一同步。
pub fn render(app: &AppState, can_back: bool, can_forward: bool) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .p(px(8.0))
        .bg(crate::theme::container())
        .border_b_1()
        .border_color(crate::theme::separator())
        // 测试用（release no-op）：tests/layout.rs 断言工具栏只有一行高
        .debug_selector(|| "mo-toolbar".to_string())
        .child(nav_button("⬅ 后退", can_back, {
            let app = app.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), Nav::Back)
        }))
        .child(nav_button("➡ 前进", can_forward, {
            let app = app.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), Nav::Forward)
        }))
        .child(nav_button("⬆ 上级", true, {
            let app = app.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), Nav::Parent)
        }))
        .child(nav_button("🔄 刷新", true, {
            let app = app.clone();
            move |cx: &mut App| spawn_nav(cx, app.clone(), Nav::Refresh)
        }))
        .child(text!("Mo"))
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

/// 一个可点击的工具栏按钮；`enabled == false` 时置灰且不挂点击回调。
fn nav_button(label: &'static str, enabled: bool, on_click: impl Fn(&mut App) + 'static) -> Div {
    let mut button = div()
        .flex()
        .flex_row()
        .items_center()
        .p(px(6.0))
        .bg(gpui_kit::white())
        .text_color(gpui_kit::black());

    if enabled {
        // imperative API：`Div` 只实现 `InteractiveElement`（提供 `interactivity()`）。
        button
            .interactivity()
            .on_click(move |_, _window, cx| on_click(cx));
    } else {
        button = button.opacity(0.35);
    }

    button.child(text!(label))
}
