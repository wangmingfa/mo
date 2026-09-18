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

/// 把本地选择变化同步到 `AppState` 的动作（语义同 `file_list`）。
enum SelSync {
    Select(mo_core::FileId),
    Toggle(mo_core::FileId),
    Range(usize, usize),
}
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
                    idx,
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
    global_idx: usize,
) -> Stateful<Div> {
    let (thumb, icon_data) = match mode {
        ViewMode::Grid => (36.0, kind_icon(entry)),
        ViewMode::Gallery => (96.0, kind_icon(entry)),
        _ => (36.0, kind_icon(entry)),
    };
    let icon_color = if selected {
        crate::theme::selected_text()
    } else {
        crate::theme::text()
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
            .child(crate::icons::icon(icon_data, thumb * 0.6, icon_color).into_any_element())
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
    // 右键：对着这个格子弹上下文菜单。`stop_propagation` 防止继续冒泡到
    // 窗格容器，把菜单换成「空白处」版本。
    let ctx_entity = entity.clone();
    let ctx_path = entry.path.clone();
    let ctx_is_dir = matches!(entry.kind, mo_core::EntryKind::Directory);
    c.interactivity()
        .on_mouse_down(MouseButton::Right, move |ev, _window, cx| {
            let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
            ctx_entity.update(cx, |v, cx| {
                v.open_context_menu(Some((ctx_path.clone(), ctx_is_dir)), x, y, pane, tab, cx);
            });
            cx.stop_propagation();
        });
    let click_entity = entity.clone();
    c.interactivity().on_click(move |ev, _window, cx| {
        if ev.click_count() >= 2 {
            click_entity.update(cx, |v, cx| v.open_entry(entry_path.clone(), cx));
            return;
        }
        // 修饰键决定选择语义：无修饰 = 单选替换；cmd/ctrl = 切换多选；shift = 连选。
        let mods = ev.modifiers();
        let multi = mods.platform || mods.control;
        let shift = mods.shift;

        let Some((app, sync)) = click_entity.update(cx, |v, _cx| {
            let p = v.panel_at_mut(pane, tab)?;
            if shift {
                let ordered: Vec<mo_core::FileId> = p.window.iter().map(|e| e.id).collect();
                let clicked = ordered.iter().position(|x| *x == id)?;
                if let Some(a) = p.selection.anchor() {
                    if let Some(ai) = ordered.iter().position(|x| *x == a) {
                        p.selection.clear();
                        p.selection.select_range(&ordered, ai, clicked);
                        p.selection.set_anchor(a);
                        return Some((
                            p.app.clone(),
                            SelSync::Range(p.window_start + ai, global_idx),
                        ));
                    }
                }
                p.selection.select(id);
                Some((p.app.clone(), SelSync::Select(id)))
            } else if multi {
                p.selection.toggle(id);
                Some((p.app.clone(), SelSync::Toggle(id)))
            } else {
                p.selection.select(id);
                Some((p.app.clone(), SelSync::Select(id)))
            }
        }) else {
            return;
        };

        match sync {
            SelSync::Select(id) => {
                cx.spawn(async move |_cx| {
                    app.select(id).await;
                })
                .detach();
            }
            SelSync::Toggle(id) => {
                cx.spawn(async move |_cx| {
                    app.toggle(id).await;
                })
                .detach();
            }
            SelSync::Range(from, to) => {
                cx.spawn(async move |_cx| {
                    app.clear_selection().await;
                    app.select_range(from, to).await;
                })
                .detach();
            }
        }
    });

    c.child(visual)
        .child(
            div()
                .w_full()
                .text_size(px(12.0))
                .text_color(if selected {
                    crate::theme::selected_text()
                } else {
                    crate::theme::text()
                })
                .overflow_hidden()
                .truncate()
                .child(text!(entry.name.clone())),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(if selected {
                    crate::theme::selected_text()
                } else {
                    crate::theme::muted()
                })
                .child(text!(sub)),
        )
}

fn kind_icon(entry: &Entry) -> &'static [u8] {
    crate::icons::entry_icon(entry)
}
