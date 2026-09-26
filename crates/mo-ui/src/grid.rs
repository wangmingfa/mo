//! grid：网格 / 画廊视图。
//!
//! 与列表共用同一套窗口懒加载（[`crate::listing`]）：一行放 `cols` 个单元，
//! 虚拟化的单位变成「单元行」，`item_count = ceil(count / cols)`。
//! 这样大目录在网格模式下同样只渲染可见单元。

use gpui_kit::base::Scrollbar;
use gpui_kit::*;
use mo_core::{Entry, MetadataState, ThumbnailState};

use crate::listing::{self, Zoom, BUFFER};
use crate::panel::ViewMode;

/// 网格内容四周的留白（与 `columns_for` 里扣掉的 24pt 对得上）。
const PAD: f32 = 12.0;

/// 内置描边 SVG 画成方框边长的几成：描边图形的**视觉**边界比它的绘制框小
/// （`icon()` 的 viewBox 自带留白），缩一圈才和铺满方框的位图（缩略图 / 系统图标）
/// 显得一样大——与列表行 `file_item::GLYPH_PX` 同一条约定。
const ICON_IN_BOX: f32 = 0.6;

/// 把本地选择变化同步到 `AppState` 的动作（语义同 `file_list`）。
enum SelSync {
    Select(mo_core::FileId),
    Toggle(mo_core::FileId),
    /// shift 连选：两端以 FileId 表达（见 `file_list::SelSync::Range` 的说明）。
    Range(mo_core::FileId, mo_core::FileId),
}
use crate::RootView;

#[allow(clippy::too_many_arguments)]
pub fn render(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    count: usize,
    cols: usize,
    mode: ViewMode,
    scroll: &UniformListScrollHandle,
    zoom: Zoom,
) -> impl IntoElement {
    let cols = cols.max(1);
    let row_count = if count == 0 { 0 } else { count.div_ceil(cols) };
    // ⚠️ 行高与下面每行 `.h(px(height))`、`cell` 里的方框必须来自**同一个** `zoom`：
    // `uniform_list` 的行高是靠渲染一行量出来的，三处各算各的就会行行重叠。
    let height = zoom.row_height(mode);
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
                let Some(mo_app::WindowRow::Entry(entry)) =
                    panel.window.get(idx.wrapping_sub(panel.window_start))
                else {
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
                // 系统图标（访达同款 PNG）：与列表行**同一条链路**
                // （`file_item::system_icon`，里面已含「有缩略图的行不问」）。
                // 槽位是本模式的方框边长：网格 / 画廊的位图和缩略图一样铺满方框。
                let system_icon = crate::file_item::entry_system_icon(
                    Some(&panel.app),
                    entry,
                    zoom.icon_slot(mode),
                );
                row = row.child(cell(
                    entry,
                    mode,
                    zoom,
                    panel.selection.is_selected(&entry.id),
                    panel
                        .diff
                        .as_ref()
                        .and_then(|m| m.get(&entry.path))
                        .copied(),
                    &entity_c,
                    pane,
                    tab,
                    idx,
                    system_icon,
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
    zoom: Zoom,
    selected: bool,
    // 分栏对比时这一格的状态（`None` = 没在对比 / 两侧相同）。
    tint: Option<mo_diff::TreeStatus>,
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    global_idx: usize,
    system_icon: Option<std::sync::Arc<mo_core::Bitmap>>,
) -> Stateful<Div> {
    let visual = visual(entry, mode, zoom, selected, system_icon);
    // 文字随缩放走但阻尼（见 `listing::zoom_text`）：方框可以 2×，名字不能。
    let label_k = listing::zoom_text(zoom.0);

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
        } else if let Some(c) = crate::app::compare_tint(tint) {
            c
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
                let ordered: Vec<mo_core::FileId> = p.window_entries().map(|e| e.id).collect();
                let clicked = ordered.iter().position(|x| *x == id)?;
                if let Some(a) = p.selection.anchor() {
                    if let Some(ai) = ordered.iter().position(|x| *x == a) {
                        p.selection.clear();
                        p.selection.select_range(&ordered, ai, clicked);
                        p.selection.set_anchor(a);
                        // 端点走 id：网格窗口永远在条目空间，但 id 与列表视图共用
                        // 同一条回灌路径（`select_between`），不特判。
                        return Some((p.app.clone(), SelSync::Range(a, id)));
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
            SelSync::Range(anchor, clicked) => {
                cx.spawn(async move |_cx| {
                    app.clear_selection().await;
                    app.select_between(anchor, clicked).await;
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
                .text_size(px(12.0 * label_k))
                .text_color(if selected {
                    crate::theme::selected_text()
                } else {
                    crate::theme::text()
                })
                .overflow_hidden()
                .truncate()
                // 测试用（release no-op）：断言名称相对 cell 居中。
                .debug_selector(move || format!("mo-grid-name-{global_idx}"))
                .child(text!(entry.display_name().to_string())),
        )
        .child(
            div()
                .text_size(px(11.0 * label_k))
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

/// 单元上半部分那块**方框**里的东西：缩略图 / 系统图标 / 内置 SVG 三选一。
///
/// 抽出来是为了单独渲染断言（`#[cfg(test)] mod tests` 里的探针）——`cell()` 要
/// `Entity<RootView>`，不好直接摆到测试里。
///
/// ⚠️ **三种情况占同样大的方框**：缩略图和系统图标都是异步到的（晚一两帧才浮现），
/// 方框要是跟着内容变大小，文件名就会在那两帧之间上下抖一下。所以外层这个
/// `mo-grid-visual` 容器是固定 `visual_box` 见方的，变的只是它里面装什么。
///
/// 位图（缩略图 / 系统图标）铺满方框、描边 SVG 缩一圈（[`ICON_IN_BOX`]）——与列表
/// 行的 `file_item`（位图 16 / 描边 12）是同一条约定。位图源是同步的
/// （`ImageSource::Render`，见 `crate::bitmap`）——「晚一两帧浮现」还在（泵节拍），
/// 但浮现的那一刻不会再有一帧空槽。
fn visual(
    entry: &Entry,
    mode: ViewMode,
    zoom: Zoom,
    selected: bool,
    system_icon: Option<std::sync::Arc<mo_core::Bitmap>>,
) -> AnyElement {
    let box_px = zoom.visual_box(mode);
    let raster = |src: gpui_kit::ImageSource| {
        img(src)
            .w(px(box_px))
            .h(px(box_px))
            .rounded(px(4.0))
            // 测试用（release no-op）：区分「画的是位图」还是「画的是描边图」。
            .debug_selector(|| "mo-grid-raster".to_string())
            .into_any_element()
    };
    // 这格要画的位图：缩略图优先，其次系统图标（与列表行同一条链路，见
    // `file_item::entry_system_icon`），都没有才退回内置 SVG。
    let raster_source = match &entry.thumbnail {
        ThumbnailState::Loaded(b) => crate::bitmap::image_source(b),
        ThumbnailState::Loading => None,
        _ => system_icon.as_ref().and_then(crate::bitmap::image_source),
    };
    let inner: AnyElement = match raster_source {
        Some(src) => raster(src),
        // `Loading` 态画「⏳」占位（生产从不置位，行为保留）；其余空态画描边图。
        None if matches!(entry.thumbnail, ThumbnailState::Loading) => {
            text!("⏳".to_string()).into_any_element()
        }
        None => crate::icons::icon(
            kind_icon(entry),
            box_px * ICON_IN_BOX,
            if selected {
                crate::theme::selected_text()
            } else {
                crate::theme::text()
            },
        )
        .debug_selector(|| "mo-grid-glyph".to_string())
        .into_any_element(),
    };
    div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(box_px))
        .h(px(box_px))
        .flex_shrink_0()
        // 测试用（release no-op）：本文件的单测摆一格出来，断言三种内容方框一样大。
        .debug_selector(|| "mo-grid-visual".to_string())
        .child(inner)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // ⚠️ 不能 `use super::*`：会把 `gpui_kit::*` 一并 glob 进来，它的 `test` 与
    // `#[test]` 属性撞名（同 `file_item.rs` 里那条注释）。
    use super::{visual, ICON_IN_BOX};
    use crate::listing::Zoom;
    use crate::panel::ViewMode;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        div, px, size, Context, IntoElement, ParentElement, Render, Styled, TestAppContext,
        VisualTestContext, Window,
    };
    use mo_core::{Bitmap, Entry, EntryKind, FileId, MetadataState, ThumbnailState};
    use std::path::PathBuf;
    use std::sync::Arc;

    /// 一张 2×2 的测试位图。位图源是同步的 `ImageSource::Render`，不再需要
    /// 「真 PNG 文件」——`img(path)` 时代要落盘一张可解码的图，位图时代直接给像素。
    fn test_bitmap() -> Arc<Bitmap> {
        Arc::new(Bitmap::from_rgba(2, 2, vec![0xAA; 16]).expect("2×2 合法"))
    }

    fn entry(thumbnail: ThumbnailState) -> Entry {
        let mut e = Entry::new(
            FileId::new(1, 1),
            "a.txt".to_string(),
            EntryKind::File,
            PathBuf::from("/tmp/a.txt"),
        );
        e.thumbnail = thumbnail;
        e.metadata = MetadataState::Loading;
        e
    }

    /// 摆一格出来（只画上半部分那块方框）。
    struct Probe(Entry, ViewMode, Zoom, Option<Arc<Bitmap>>);

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .p(px(12.0))
                .child(visual(&self.0, self.1, self.2, false, self.3.clone()))
        }
    }

    /// 渲染一格，分别读出方框 / 位图 / 描边图三者的几何（逻辑像素）。
    ///
    /// 三者分开读而不是「谁在就取谁」：这条用例要同时钉住「**该**出现位图」和
    /// 「**不该**出现位图」两个方向。
    type Bounds = gpui_kit::Bounds<gpui_kit::Pixels>;

    /// 方框里该出现的是什么。
    #[derive(Clone, Copy)]
    enum Inner {
        /// 铺满方框的位图（缩略图 / 系统图标）。
        Bitmap,
        /// 缩一圈的内置描边 SVG。
        Glyph,
        /// 都不是（正在出缩略图的那一格画的是「⏳」占位）。
        Placeholder,
    }

    fn probe_bounds_at(
        entry: Entry,
        mode: ViewMode,
        zoom: Zoom,
        system_icon: Option<Arc<Bitmap>>,
    ) -> (Bounds, Option<Bounds>, Option<Bounds>) {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        // 窗口要放得下 2× 的画廊方框（96 × 2 + 四周 12 × 2 = 216），不然量到的是被裁剪的尺寸。
        let window = cx.open_window(size(px(320.), px(320.)), |_, _cx| {
            Probe(entry, mode, zoom, system_icon)
        });
        let mut cx = VisualTestContext::from_window(window.into(), &cx);
        cx.update(|window, cx| window.render_frame(cx));
        (
            cx.debug_bounds("mo-grid-visual").expect("方框没渲染"),
            cx.debug_bounds("mo-grid-raster"),
            cx.debug_bounds("mo-grid-glyph"),
        )
    }

    /// 摆一格出来，不缩放（原来那条用例的入口）。
    fn probe_bounds(
        entry: Entry,
        mode: ViewMode,
        system_icon: Option<Arc<Bitmap>>,
    ) -> (Bounds, Option<Bounds>, Option<Bounds>) {
        probe_bounds_at(entry, mode, Zoom(1.0), system_icon)
    }

    /// 边长的逻辑像素值。
    fn side(b: &Bounds) -> f32 {
        f32::from(b.size.width)
    }

    /// 方框里装什么：**缩略图 / 系统图标都是位图且铺满方框，内置 SVG 缩一圈**，
    /// 而四种情况下**方框本身一样大**。
    ///
    /// 两件事各有一条理由：
    ///
    /// * 方框定死（`visual_box` 见方）——缩略图和系统图标都是异步到的（晚一两帧才
    ///   浮现），方框要是跟着内容变大小，文件名就会在那两帧之间上下跳一下；网格一屏
    ///   几十格一起跳，比列表里那种左右抖更扎眼。正在出缩略图那一格画的「⏳」也算在
    ///   这条里（占位再小也得占满方框）；
    /// * 位图铺满、描边缩一圈（`ICON_IN_BOX`）——与列表行 `file_item`（位图 16 /
    ///   描边 12）同一条约定，不然同一份条目在列表和网格里看着一大一小。
    ///
    /// 顺带钉住**系统图标那条分支真的走位图**：同一份 `Entry`（没有缩略图）给了
    /// 系统图标就该出现位图、没给就该出现描边图——只改「画多大」不改「画哪条路」
    /// 是骗不过 `mo-grid-raster` / `mo-grid-glyph` 这两个判据的。
    #[test]
    fn the_visual_box_holds_a_full_bleed_bitmap_or_a_smaller_glyph() {
        for mode in [ViewMode::Grid, ViewMode::Gallery] {
            let expect = crate::listing::visual_box(mode);
            let cases: [(&str, Entry, Option<Arc<Bitmap>>, Inner); 4] = [
                (
                    "缩略图",
                    entry(ThumbnailState::Loaded(test_bitmap())),
                    None,
                    Inner::Bitmap,
                ),
                (
                    "系统图标",
                    entry(ThumbnailState::Idle),
                    Some(test_bitmap()),
                    Inner::Bitmap,
                ),
                ("内置 SVG", entry(ThumbnailState::Idle), None, Inner::Glyph),
                (
                    "加载占位",
                    entry(ThumbnailState::Loading),
                    None,
                    Inner::Placeholder,
                ),
            ];
            for (what, e, sys, inner_kind) in cases {
                let (frame, raster, glyph) = probe_bounds(e, mode, sys);
                assert_eq!(
                    (side(&frame), f32::from(frame.size.height)),
                    (expect, expect),
                    "{mode:?} 里「{what}」那块方框不是 {expect}×{expect}"
                );
                match inner_kind {
                    Inner::Bitmap => {
                        let raster = raster
                            .unwrap_or_else(|| panic!("{mode:?} 里「{what}」该画位图，却没画"));
                        assert!(
                            glyph.is_none(),
                            "{mode:?} 里「{what}」画了位图还叠了一张描边图"
                        );
                        assert_eq!(
                            side(&raster),
                            expect,
                            "{mode:?} 里「{what}」这个位图没铺满方框（{expect}pt）"
                        );
                    }
                    Inner::Glyph => {
                        assert!(
                            raster.is_none(),
                            "{mode:?} 里「{what}」没有系统图标，不该画位图（会挂一张加载不出来的空图）"
                        );
                        let glyph =
                            glyph.unwrap_or_else(|| panic!("{mode:?} 里「{what}」该退回内置 SVG"));
                        let want = expect * ICON_IN_BOX;
                        // 容差 0.51：gpui 会把尺寸对齐到设备像素格，21.6pt 量出来是 21.5。
                        assert!(
                            (side(&glyph) - want).abs() < 0.51,
                            "{mode:?} 里「{what}」该按方框的 {ICON_IN_BOX} 缩着画（{want}pt），实际 {}pt",
                            side(&glyph)
                        );
                    }
                    Inner::Placeholder => {
                        assert!(
                            raster.is_none() && glyph.is_none(),
                            "{mode:?} 里「{what}」画的是占位，不该有位图也不该有描边图"
                        );
                    }
                }
            }
        }
    }

    /// 缩放**真的落在渲染上**：同一份条目，方框与位图按倍率等比变化。
    ///
    /// 这条是这一档功能的硬判据——`Zoom` 算得再对，视图只要没把它接上（或只接了
    /// 方框没接位图），用户看到的就是「按了 ⌘+ 没反应」或「图标跟方框对不上」。
    /// 所以这里量的是**画出来的** bounds，不是算出来的数。
    #[test]
    fn zooming_scales_the_painted_box_and_bitmap() {
        for mode in [ViewMode::Grid, ViewMode::Gallery] {
            let base = crate::listing::visual_box(mode);
            for k in [0.75, 1.5, 2.0] {
                let (frame, raster, glyph) = probe_bounds_at(
                    entry(ThumbnailState::Loaded(test_bitmap())),
                    mode,
                    Zoom(k),
                    None,
                );
                assert!(glyph.is_none(), "{mode:?} 有缩略图不该画描边图");
                let want = base * k;
                // 容差 0.51：gpui 把尺寸对齐到设备像素格（36 × 0.75 = 27 量出来是 26.5~27）。
                assert!(
                    (side(&frame) - want).abs() < 0.51,
                    "{mode:?} 在 {k}× 下方框该是 {want}pt，实际 {}pt",
                    side(&frame)
                );
                let raster = raster.expect("有缩略图就该画位图");
                assert!(
                    (side(&raster) - want).abs() < 0.51,
                    "{mode:?} 在 {k}× 下位图该铺满 {want}pt 的方框，实际 {}pt",
                    side(&raster)
                );
            }
        }
    }
}
