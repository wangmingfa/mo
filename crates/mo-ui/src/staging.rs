//! 暂存区抽屉（Staging Tray）。
//!
//! 贴在中央区下方、状态栏上方的一条常驻抽屉：把「从好几个目录里各挑几个文件，
//! 最后一起处理」这种事变成一次收集 + 一次投递。
//!
//! 与剪贴板的分工写在 `mo-app::staging` 的模块文档里：**剪贴板是替换 + 立刻
//! 粘贴，这里是追加 + 攒够再做**。UI 上对应的两个差别也就从这里来——
//! 收集后**不清空**（可以接着去下一个目录），复制后**也不清空**（可以再往
//! 另一个目录放一份），只有移动会清空（源已经不在原处了）。
//!
//! 抽屉开关是 UI 态（`RootView::staging_open`），清单本身在 `AppState` 里
//! （进程级共享），两边别搞反：开关是每个窗口各自的，内容是全局一份。

use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_app::{AppState, StagedEntry};
use mo_core::EntryKind;

use crate::{icons, theme, RootView};

/// 一行高度：与文件列表的 `listing::row_height` 对齐（24px），抽屉里也别另起一套。
const ROW_H: f32 = 24.0;
/// 列表区最多显示多少高度（超出则在抽屉内滚动，不撑高整窗）。
const ROWS_MAX_H: f32 = 120.0;

/// 抽屉本体。
pub fn render_tray(app: &AppState, entity: &Entity<RootView>) -> impl IntoElement {
    let entries = app.staged();
    let count = entries.len();

    let mut panel = div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .bg(theme::container())
        .border_t_1()
        .border_color(theme::separator())
        .text_color(theme::text())
        .debug_selector(|| "mo-staging-tray".to_string());

    // ── 标题行：条数 + 投递动作 ──────────────────────────────────────────
    let mut header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(10.0))
        .h(px(26.0))
        .text_size(px(11.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme::muted())
                .child(text!(format!("暂存区（{count}）"))),
        );

    if count > 0 {
        header = header
            .child(tray_button(
                entity,
                "staging-copy",
                "复制到此",
                |v, cx| v.paste_staged(cx, false),
            ))
            .child(tray_button(
                entity,
                "staging-move",
                "移动到此",
                |v, cx| v.paste_staged(cx, true),
            ))
            .child(tray_button(entity, "staging-clear", "清空", |v, cx| {
                v.clear_staged(cx)
            }));
    }
    header = header.child(tray_button(entity, "staging-close", "收起", |v, cx| {
        v.toggle_staging(cx)
    }));
    panel = panel.child(header);

    // ── 条目区 ──────────────────────────────────────────────────────────
    if entries.is_empty() {
        panel = panel.child(
            div()
                .px(px(10.0))
                .py(px(10.0))
                .text_size(px(11.0))
                .text_color(theme::muted())
                .child(text!("还没有收集任何文件 —— 选中文件后按 ⌘⇧S 收集到这里")),
        );
        return panel;
    }

    let mut rows = div()
        .id("staging-rows")
        .flex()
        .flex_col()
        .max_h(px(ROWS_MAX_H))
        .overflow_y_scrollbar()
        .border_t_1()
        .border_color(theme::separator())
        .debug_selector(|| "mo-staging-rows".to_string());

    for (ix, e) in entries.iter().enumerate() {
        rows = rows.child(staging_row(e, ix, entity));
    }
    panel.child(rows)
}

/// 一行：图标 + 文件名 + 来源目录 + 移除。
fn staging_row(e: &StagedEntry, ix: usize, entity: &Entity<RootView>) -> Stateful<Div> {
    let name = e
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| e.path.display().to_string());
    let from = e.from.display().to_string();
    let kind = if e.is_dir {
        EntryKind::Directory
    } else {
        EntryKind::File
    };

    // ⚠️ 行尾的「×」是嵌套按钮：它自己有 on_click，虽然本行没有点击处理，
    // 但保留 stop_propagation 是为了将来给整行加「跳到该文件」时不会把
    // 移除动作一起触发（context_menu 的二级菜单踩过同一个坑）。
    let remove_entity = entity.clone();
    let remove_path = e.path.clone();
    let mut remove = div()
        .id(("staging-remove", ix))
        .w(px(16.0))
        .h(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .text_size(px(11.0))
        .text_color(theme::muted())
        .hover(|s| s.bg(theme::hover_bg()).text_color(theme::text()))
        .child(text!("×"))
        .debug_selector(move || format!("mo-staging-remove-{ix}"));
    remove.interactivity().on_click(move |_, _window, cx| {
        cx.stop_propagation();
        let path = remove_path.clone();
        remove_entity.update(cx, |v, cx| v.unstage(path, cx));
    });

    div()
        .id(("staging-row", ix))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .h(px(ROW_H))
        .text_size(px(12.0))
        .hover(|s| s.bg(theme::hover_bg()))
        .debug_selector(move || format!("mo-staging-row-{ix}"))
        .child(icons::icon(
            icons::icon_for_kind_and_name(kind, &name),
            14.0,
            theme::muted(),
        ))
        // 文件名优先占空间，来源目录是次要信息（窄窗口下先被挤掉）。
        .child(div().flex_1().min_w_0().truncate().child(text!(name)))
        .child(
            div()
                .max_w(px(220.0))
                .text_size(px(10.0))
                .text_color(theme::muted())
                .truncate()
                .child(text!(from)),
        )
        .child(remove.test_support())
}

/// 抽屉里的小按钮。
///
/// `run` 用 `Fn` 而不是 `FnOnce`：`on_click` 可能被框架多次调用，捕获进去的
/// 闭包得能重复执行。
fn tray_button<F>(
    entity: &Entity<RootView>,
    id: &'static str,
    label: &'static str,
    run: F,
) -> Stateful<Div>
where
    F: Fn(&mut RootView, &mut Context<RootView>) + 'static,
{
    let e = entity.clone();
    let mut b = div()
        .id(id)
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(theme::separator())
        .text_size(px(11.0))
        .text_color(theme::text())
        .hover(|s| s.bg(theme::hover_bg()))
        .child(text!(label))
        .debug_selector(move || format!("mo-{id}"));
    b.interactivity().on_click(move |_, _window, cx| {
        e.update(cx, |v, cx| run(v, cx));
    });
    b
}
