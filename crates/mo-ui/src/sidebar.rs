use std::path::Path;

use gpui_kit::*;
use mo_app::AppState;

use crate::RootView;

/// 侧边栏：快捷访问 / 书签。
///
/// 位置由 [`AppState::quick_locations`] 提供（基于 `dirs` 解析，存在才显示），
/// 点击即跳转；当前所在位置高亮（含子目录内）。
pub fn render(
    app: &AppState,
    current: &Option<std::path::PathBuf>,
    entity: &Entity<RootView>,
) -> impl IntoElement {
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

    for (ix, (label, path)) in locations.into_iter().enumerate() {
        let is_active = active.as_ref() == Some(&path);
        let app_click = app.clone();
        // ⚠️ 必须有元素 ID：无 ID 的裸 div 拿不到 element_state，on_click 永远不触发。
        let mut item = div()
            .id(("sidebar-loc", ix))
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

    // 书签区（`~/Library/Application Support/mo/config.json` 里的
    // `sidebar_bookmarks`，命令面板「添加 / 移除书签」维护）。
    let bookmarks = app.bookmarks();
    if !bookmarks.is_empty() {
        panel = panel.child(
            div()
                .px(px(10.0))
                .pb(px(6.0))
                .pt(px(10.0))
                .text_size(px(11.0))
                .text_color(crate::theme::muted())
                .child(text!("书签")),
        );
    }
    for (ix, path) in bookmarks.into_iter().enumerate() {
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let is_active = current.as_deref() == Some(path.as_path());
        let app_click = app.clone();
        let app_del = app.clone();
        let entity_del = entity.clone();
        let target = path.clone();
        let del = path.clone();

        let mut item = div()
            .id(format!("sidebar-bm-{ix}"))
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
        item.interactivity().on_click(move |_, _window, cx| {
            let app = app_click.clone();
            let target = target.clone();
            cx.spawn(async move |_cx| {
                let _ = app.open_directory(&target).await;
            })
            .detach();
        });

        // 移除按钮：常显但弱化，避免为「悬停才出现」再引入一套 hover 状态。
        let mut remove = div()
            .id(format!("sidebar-bm-del-{ix}"))
            .ml_auto()
            .pl(px(6.0))
            .text_size(px(12.0))
            .text_color(crate::theme::muted())
            .child(text!("✕".to_string()));
        remove.interactivity().on_click(move |_, _window, cx| {
            app_del.remove_bookmark(&del);
            let e = entity_del.clone();
            e.update(cx, |_, cx| cx.notify());
        });

        panel = panel.child(item.child(text!(label)).child(remove));
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
