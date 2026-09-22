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

/// 网格内容四周的留白（与 `columns_for` 里扣掉的 24pt 对得上）。
const PAD: f32 = 12.0;

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
        // 这一帧真的画出来的、还等着缩略图的单元——行渲染完统一派发（见下方注释）。
        let mut want_thumbs: Vec<Entry> = Vec::new();
        for r in range.clone() {
            let mut row = div().flex().flex_row().w_full().h(px(height)).gap(px(6.0));
            for c in 0..cols {
                let idx = r * cols + c;
                if idx >= count {
                    break;
                }
                let Some(entry) = panel.window.get(idx.wrapping_sub(panel.window_start)) else {
                    // 窗口还没补上这一格（刚切目录 / 快速滚动）：画一个**只有底色**
                    // 的骨架格，尺寸 / 圆角 / 内边距与 `cell()` 完全一致，别让网格在
                    // 数据到达前后跳一下。与列表视图的占位行同一条约定：不画 `…`。
                    // ⚠️ 同样要有唯一 ID（多个格子同时缺数据，a11y NodeId 会撞）。
                    row = row.child(
                        div()
                            .id(format!("grid-ph-{pane}-{tab}-{idx}"))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .p(px(4.0))
                            .rounded(px(6.0))
                            .bg(crate::theme::zebra())
                            .debug_selector(move || format!("mo-grid-ph-{idx}")),
                    );
                    continue;
                };
                if matches!(entry.thumbnail, ThumbnailState::Idle) && entry.supports_thumbnail() {
                    want_thumbs.push(entry.clone());
                }
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
            rows.push(row.into_any_element());
        }
        // 缩略图：这一帧看得见的、还等着的那几格，统一派发。
        //
        // 为什么在渲染路径上派发（而不是「窗口抓回来时」）：窗口带着上下各
        // BUFFER(=100) **条目**的余量，按整窗口派发等于每进一个目录就白解几百张
        // 大图（实测单张 80ms 级），目录越大白解得越多——四核被占住，界面跟着卡。
        // 请求是幂等的（在跑 / 已生成 / 失败都会被调度器跳过），每帧调也无所谓。
        if !want_thumbs.is_empty() {
            panel.app.thumbs().request(panel.app.clone(), want_thumbs);
        }
        rows
    })
    // ⚠️ 必需：列表项只在 prepaint 渲染，布局期 taffy 看到 0 子节点会把高算成 0。
    .flex_1()
    // 四周留白：首行不顶工具栏，滚到底最后一行下面也留得出来。
    // 上下这 12pt 由 list 元素自己算进滚动内容高度（`padding.top` 加到条目起点、
    // 上下都算进 content 高度与 scroll_max），所以底部那截是真留出来的，
    // 不是只把首行往下推。原来这里只有 `px`（左右），上下是贴边的。
    .p(px(PAD))
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
        })
        // 测试用（release no-op）：按序号定位某个单元（守留白与居中的布局断言）。
        .debug_selector(move || format!("mo-grid-cell-{global_idx}"));
    if !selected {
        c = c.hover(|s| s.bg(crate::theme::hover_bg()));
    }

    let id = entry.id;
    let entry_path = entry.path.clone();
    // 右键：对着这个格子弹上下文菜单。`stop_propagation` 防止继续冒泡到
    // 窗格容器，把菜单换成「空白处」版本。
    let ctx_entity = entity.clone();
    let ctx_path = entry.path.clone();
    // 目录判据来自列表模型（`entry.kind`）：远程条目在本地磁盘上不存在，
    // `Path::is_dir()` 会把远程目录判成文件（见 `RootView::open_entry`）。
    let is_dir = matches!(entry.kind, mo_core::EntryKind::Directory);
    c.interactivity()
        .on_mouse_down(MouseButton::Right, move |ev, _window, cx| {
            let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
            ctx_entity.update(cx, |v, cx| {
                v.open_context_menu(Some((ctx_path.clone(), is_dir)), x, y, pane, tab, cx);
            });
            cx.stop_propagation();
        });
    let click_entity = entity.clone();
    c.interactivity().on_click(move |ev, _window, cx| {
        if ev.click_count() >= 2 {
            click_entity.update(cx, |v, cx| v.open_entry(entry_path.clone(), is_dir, cx));
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
            // 名称：作为 cell（`flex_col` + `items_center`）里的一个**收缩到内容宽**的
            // 块，靠 `items_center` 水平居中——与图标同一套机制。
            // ⚠️ 别改成 `w_full` + `text_center()`：那样盒子被拉满整格，居中与否量不出来
            // （实测名字仍然贴左）。gpui 的 `TextLayout::paint` 是按
            // `window.text_style().text_align` 在盒子内对齐的，这条路在这里不生效。
            // `max_w_full` + `truncate` 让长名字截成省略号，而不是溢出到隔壁格。
            div()
                .max_w_full()
                .text_size(px(12.0))
                .text_color(if selected {
                    crate::theme::selected_text()
                } else {
                    crate::theme::text()
                })
                .overflow_hidden()
                .truncate()
                // 测试用（release no-op）：断言名称相对 cell 居中。
                .debug_selector(move || format!("mo-grid-name-{global_idx}"))
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
