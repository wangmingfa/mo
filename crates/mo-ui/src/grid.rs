//! grid：网格 / 画廊视图。
//!
//! 与列表共用同一套窗口懒加载（[`crate::listing`]）：一行放 `cols` 个单元，
//! 虚拟化的单位变成「单元行」，`item_count = ceil(count / cols)`。
//! 这样大目录在网格模式下同样只渲染可见单元。

use gpui_kit::base::Scrollbar;
use gpui_kit::*;
use mo_core::{Entry, MetadataState, ThumbnailState};

use crate::listing::{self, row_height, BUFFER};
use crate::panel::ViewMode;
use crate::RootView;

pub fn render(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    count: usize,
    cols: usize,
    mode: ViewMode,
    scroll: &UniformListScrollHandle,
) -> impl IntoElement {
    let cols = cols.max(1);
    let row_count = if count == 0 { 0 } else { count.div_ceil(cols) };
    let height = row_height(mode);
    let entity_c = entity.clone();

    let list = uniform_list("mo-grid", row_count, move |range, _window, cx| {
        let need_start = range.start.saturating_mul(cols).saturating_sub(BUFFER);
        let need_end = (range.end.saturating_mul(cols) + BUFFER).min(count);
        listing::ensure_window(&entity_c, pane, tab, need_start, need_end, &range, cx);

        let view = entity_c.read(cx);
        let Some(panel) = view.panel_at(pane, tab) else {
            return Vec::new();
        };
        let mut rows: Vec<AnyElement> = Vec::with_capacity(range.len());
        for r in range.clone() {
            let mut row = div().flex().flex_row().w_full().h(px(height)).gap(px(6.0));
            let mut any_missing = false;
            for c in 0..cols {
                let idx = r * cols + c;
                if idx >= count {
                    break;
                }
                let Some(entry) = panel.window.get(idx.wrapping_sub(panel.window_start)) else {
                    any_missing = true;
                    continue;
                };
                row = row.child(cell(
                    entry,
                    mode,
                    panel.selection.is_selected(&entry.id),
                    &entity_c,
                    pane,
                    tab,
                ));
            }
            if any_missing {
                row = row.child(div().flex_1().child(text!("…".to_string())));
            }
            rows.push(row.into_any_element());
        }
        rows
    })
    // ⚠️ 必需：列表项只在 prepaint 渲染，布局期 taffy 看到 0 子节点会把高算成 0。
    .flex_1()
    .px(px(12.0))
    .track_scroll(scroll)
    .debug_selector(|| "mo-grid".to_string());

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(list)
        .child(Scrollbar::vertical(scroll))
}

/// 一个网格单元：缩略图 / 图标在上，文件名在下（过长截断）。
#[allow(clippy::too_many_arguments)]
fn cell(
    entry: &Entry,
    mode: ViewMode,
    selected: bool,
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
) -> Stateful<Div> {
    let (thumb, icon_name) = match mode {
        ViewMode::Grid => (36.0, kind_icon(entry)),
        ViewMode::Gallery => (96.0, kind_icon(entry)),
        _ => (36.0, kind_icon(entry)),
    };

    let visual: AnyElement = match &entry.thumbnail {
        ThumbnailState::Loaded(p) => img(p.as_path())
            .w(px(thumb))
            .h(px(thumb))
            .rounded(px(4.0))
            .into_any_element(),
        ThumbnailState::Loading => text!("⏳".to_string()).into_any_element(),
        _ => div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(thumb))
            .h(px(thumb))
            .text_size(px(thumb * 0.6))
            .child(text!(icon_name.to_string()))
            .into_any_element(),
    };

    // 元数据未就绪时不显示大小，避免把「还没加载」误读成「空文件」。
    let sub = match &entry.metadata {
        MetadataState::Loaded(m) => crate::file_item::format_size(m.size),
        MetadataState::Loading => String::new(),
        MetadataState::Failed(_) => "—".to_string(),
    };

    let mut c = div()
        .id(format!("cell-{pane}-{tab}-{}", entry.id))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(4.0))
        .flex_1()
        .min_w_0()
        .h_full()
        .p(px(4.0))
        .rounded(px(6.0))
        .bg(if selected {
            crate::theme::selected_bg()
        } else {
            crate::theme::surface()
        });
    if !selected {
        c = c.hover(|s| s.bg(crate::theme::hover_bg()));
    }

    let id = entry.id;
    let entry_path = entry.path.clone();
    let click_entity = entity.clone();
    c.interactivity().on_click(move |ev, _window, cx| {
        if ev.click_count() >= 2 {
            click_entity.update(cx, |v, cx| v.open_entry(entry_path.clone(), cx));
            return;
        }
        let Some(app) = click_entity.update(cx, |v, _cx| {
            let p = v.panel_at_mut(pane, tab)?;
            p.selection.toggle(id);
            Some(p.app.clone())
        }) else {
            return;
        };
        cx.spawn(async move |_cx| {
            app.toggle(id).await;
        })
        .detach();
    });

    c.child(visual)
        .child(
            div()
                .w_full()
                .text_size(px(12.0))
                .text_color(crate::theme::text())
                .overflow_hidden()
                .truncate()
                .child(text!(entry.name.clone())),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(crate::theme::muted())
                .child(text!(sub)),
        )
}

fn kind_icon(entry: &Entry) -> &'static str {
    match entry.kind {
        mo_core::EntryKind::Directory => "📁",
        mo_core::EntryKind::File => "📄",
        mo_core::EntryKind::Symlink => "🔗",
        mo_core::EntryKind::Other => "❓",
    }
}
