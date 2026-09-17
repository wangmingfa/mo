use std::path::Path;

use gpui_kit::*;
use mo_app::AppState;

/// 侧边栏：快捷访问 / 书签。
///
/// 位置由 [`AppState::quick_locations`] 提供（基于 `dirs` 解析，存在才显示），
/// 点击即跳转；当前所在位置高亮（含子目录内）。
pub fn render(app: &AppState, current: &Option<std::path::PathBuf>) -> impl IntoElement {
    let locations = app.quick_locations();
    // 当前位置落在哪个快捷位置里（取匹配最深的一个）。
    let active = current.as_deref().and_then(|cur| {
        locations
            .iter()
            .filter(|(_, root)| is_within(cur, root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(_, root)| root.clone())
    });

    let mut panel = div()
        .flex()
        .flex_col()
        .w(px(188.0))
        .flex_shrink_0()
        .p(px(8.0))
        .gap(px(1.0))
        .bg(crate::theme::container())
        .border_r_1()
        .border_color(crate::theme::separator())
        .text_color(crate::theme::text())
        // 测试用（release no-op）：tests/layout.rs 断言侧边栏在中央区左侧
        .debug_selector(|| "mo-sidebar".to_string());

    panel = panel.child(
        div()
            .px(px(10.0))
            .pb(px(6.0))
            .pt(px(2.0))
            .text_size(px(11.0))
            .text_color(crate::theme::muted())
            .child(text!("快捷访问")),
    );

    for (label, path) in locations {
        let is_active = active.as_ref() == Some(&path);
        let app_click = app.clone();
        let mut item = div()
            .flex()
            .flex_row()
            .items_center()
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .text_size(px(13.0))
            .text_color(if is_active {
                crate::theme::accent()
            } else {
                crate::theme::text()
            });

        if is_active {
            item = item.bg(crate::theme::selected_bg());
        } else {
            item = item.hover(|s| s.bg(crate::theme::hover_bg()));
        }

        // imperative API：`Div` 只实现 `InteractiveElement`，点击回调走这里。
        // on_click 要求 `Fn`（可多次调用），闭包内只克隆、不消耗捕获值。
        item.interactivity().on_click(move |_, _window, cx| {
            let app = app_click.clone();
            let target = path.clone();
            cx.spawn(async move |_cx| {
                let _ = app.open_directory(&target).await;
            })
            .detach();
        });

        panel = panel.child(item.child(text!(label)));
    }

    panel
}

/// `current` 是否位于 `root` 之内（含相等）。
///
/// 用 `strip_prefix` 而不是字符串前缀，避免 `/Users/a/Downloads2`
/// 被误判为在 `/Users/a/Downloads` 里。
fn is_within(current: &Path, root: &Path) -> bool {
    if current == root {
        return true;
    }
    match current.strip_prefix(root) {
        Ok(rem) => !rem.as_os_str().is_empty(),
        Err(_) => false,
    }
}
