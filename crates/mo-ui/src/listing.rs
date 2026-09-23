//! listing：多种视图共用的**窗口懒加载**与网格列数计算。
//!
//! 列表 / 网格 / 画廊都遵循同一套「窗口」机制：UI 侧只持有可见区的条目快照，
//! 滚动到哪补哪。抽出来是为了避免三种视图各写一遍、写出三份不一致的竞态。

use std::ops::Range;

use gpui_kit::{App, Entity};
use mo_app::{AppState, Grouping};

use crate::panel::ViewMode;
use crate::RootView;

/// 可见区上下各多取这么多**条目**：滚动时不会频繁出现占位，又不会一次抓太多。
pub(crate) const BUFFER: usize = 100;

/// 网格 / 画廊模式每个单元的目标宽度（用于按可用宽度算列数）。
pub(crate) fn cell_width(mode: ViewMode) -> f32 {
    match mode {
        ViewMode::Grid => 116.0,
        ViewMode::Gallery => 168.0,
        _ => 0.0,
    }
}

/// 每行托管的行数（行高度，uniform_list 要求行高固定）。
pub(crate) fn row_height(mode: ViewMode) -> f32 {
    match mode {
        ViewMode::Grid => 100.0,
        ViewMode::Gallery => 152.0,
        _ => 24.0,
    }
}

/// 可用宽度能放下几列（至少 1 列）。`cell_w` 传**缩放后**的单元宽
/// （[`Zoom::columns_for`] 委托这里，传入 `cell_width(mode) * factor`）。
pub(crate) fn columns_for(cell_w: f32, available: f32) -> usize {
    if cell_w <= 0.0 {
        return 1;
    }
    ((available - 24.0) / cell_w).floor().clamp(1.0, 12.0) as usize
}

/// 图标缩放倍率（配置里的 `ui.icon_scale`），**只作用于网格 / 画廊**。
///
/// 为什么做成一个值对象而不是给每个几何函数加一个 `scale: f32`：缩放要同时落在
/// **三样**东西上——方框（图标画多大）、单元宽（一行放几列）、行高（`uniform_list`
/// 的行高），三处各乘一次、各写各的，迟早有一条漏乘：漏了方框，图标溢出格子；
/// 漏了单元宽，图标互相压住；漏了行高，行与行重叠。这里只留一个入口，视图那边
/// 拿到 `Zoom` 后就不可能只乘一半。
///
/// 列表 / 列视图在自己的行高里**恒为 1.0**：那两种视图的行高是固定 24pt
/// （见 [`row_height`]），放大图标会牵动整行布局（列宽、行高、系统图标档位全要
/// 跟着改），不在这一档里做。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Zoom(pub(crate) f32);

impl Zoom {
    fn factor(self, mode: ViewMode) -> f32 {
        if matches!(mode, ViewMode::Grid | ViewMode::Gallery) {
            self.0
        } else {
            1.0
        }
    }

    /// 缩放后的单元宽（列数由它算出：放大 → 一行放得下更少）。
    pub(crate) fn cell_width(self, mode: ViewMode) -> f32 {
        cell_width(mode) * self.factor(mode)
    }

    /// 缩放后的行高（`uniform_list` 的行高必须与行元素 `.h()` 一致）。
    pub(crate) fn row_height(self, mode: ViewMode) -> f32 {
        row_height(mode) * self.factor(mode)
    }

    /// 缩放后的方框边长。
    pub(crate) fn visual_box(self, mode: ViewMode) -> f32 {
        visual_box(mode) * self.factor(mode)
    }

    /// 缩放后的位图槽位：**跟着方框一起缩放**，否则放大了还去要原来那档位图
    /// （96pt 的方框要 40px 的图 = 近 5 倍上采样，糊成一片）；反过来缩小时会白取
    /// 大档位图，白花主线程时间。档位判定见 `mo_app::icon_px_for_slot`。
    pub(crate) fn icon_slot(self, mode: ViewMode) -> f32 {
        icon_slot(mode) * self.factor(mode)
    }

    /// 缩放后的列数（至少 1 列）。
    pub(crate) fn columns_for(self, mode: ViewMode, available: f32) -> usize {
        columns_for(self.cell_width(mode), available)
    }
}

/// 单元里两行文字随缩放的系数：**阻尼**过。
///
/// 文字按同样的倍率放大会失衡（2× 时名字 24pt 比图标还抢眼），不缩放又会在放大后
/// 显得图标孤零零的。折中：随倍率走，但收在 `[0.9, 1.4]`——缩小时几乎看不出变化
/// （下限不设到 0.75 是因为 11pt 的「大小」那一行缩到 8pt 就糊了）。
pub(crate) fn zoom_text(scale: f32) -> f32 {
    if !scale.is_finite() {
        return 1.0;
    }
    scale.clamp(0.9, 1.4)
}

/// 网格 / 画廊里那个**方框**的边长：缩略图铺满它，系统图标也铺满它。
///
/// 「位图铺满、内置 SVG 缩一圈」是与列表行同一条约定（列表里槽位 16、描边 SVG 12）：
/// 描边图形的视觉边界比它的绘制框小，跟铺满的位图并排才显得一样大。
/// 列表 / 列视图没有这个方框，返回 0。
pub(crate) fn visual_box(mode: ViewMode) -> f32 {
    match mode {
        ViewMode::Grid => 36.0,
        ViewMode::Gallery => 96.0,
        _ => 0.0,
    }
}

/// 位图图标的**槽位边长（逻辑 pt）**——四个视图只有这一张表。
///
/// 两个消费者，都靠它对齐：
///
/// * [`crate::file_item::system_icon`] 把它交给 `AppState::file_icon`，由后者决定
///   这一档要取 40px 还是 128px 的位图（见 `mo_app::icon::icon_px_for_slot`）；
/// * 视图自己在**没有**系统图标时用它选内置 SVG 的绘制尺寸。
///
/// ⚠️ 槽位不是「图标画出来多大」，而是「这块地方多大」：网格里描边 SVG 是按方框的
/// 0.6 缩着画的（见 `grid::cell`），但它要的位图和缩略图一样铺满方框。
pub(crate) fn icon_slot(mode: ViewMode) -> f32 {
    match mode {
        ViewMode::List | ViewMode::Columns => crate::file_item::ICON_PX,
        ViewMode::Grid | ViewMode::Gallery => visual_box(mode),
    }
}

/// 保证面板的窗口覆盖 `[need_start, need_end)`；不覆盖就异步补窗。
///
/// * `visible` 是 uniform_list 传入的真实可见范围。gpui 每帧还会用单行 range
///   （如 `0..1`）调用渲染闭包测量行高，这类测量调用必须跳过，
///   否则测量请求与真实请求各自 spawn 的 fetch 会在落地时互相覆盖 window
///   → 窗口永不收敛 → 整屏占位符闪烁。
/// * 每个请求带上自己的范围，落地时只清除匹配的 `pending`，
///   避免吞掉期间新发出的请求。
pub(crate) fn ensure_window(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    need_start: usize,
    need_end: usize,
    visible: &Range<usize>,
    cx: &mut App,
) {
    if visible.len() <= 1 {
        return;
    }
    let mut request: Option<(AppState, Range<usize>, bool)> = None;
    entity.update(cx, |v, _cx| {
        let Some(p) = v.panel_at_mut(pane, tab) else {
            return;
        };
        // 本次补窗的空间：分组开启的列表视图按「行」（含组头）取，其余按条目取。
        // 与渲染那一帧用的 item_count 必须同源（都来自 sync_panel 的同一判定）。
        let grouped = p.view_mode == ViewMode::List && p.grouping != Grouping::None;
        let req = (need_start..need_end, grouped);
        if !p.covered(need_start, need_end) && p.pending.as_ref() != Some(&req) {
            p.pending = Some(req.clone());
            request = Some((p.app.clone(), req.0, req.1));
        }
    });

    let Some((app, r, grouped)) = request else {
        return;
    };
    let this = entity.clone();
    tracing::trace!(
        target: "mo_ui::window",
        pane, tab, need = ?r, visible = ?visible, grouped, "fetch spawn"
    );
    let spawned = std::time::Instant::now();
    cx.spawn(async move |cx| {
        let (dir_path, start, rows) = app.list_window(r.clone(), grouped).await;
        let elapsed = spawned.elapsed().as_millis();
        this.update(cx, |v, cx| {
            let Some(p) = v.panel_at_mut(pane, tab) else {
                return;
            };
            // 取回在途时可能已切换目录：旧目录的快照不能覆盖新目录的窗口。
            if p.path.as_deref() != Some(dir_path.as_path()) {
                tracing::warn!(
                    target: "mo_ui::window",
                    pane, tab, need = ?r, got = ?dir_path,
                    want = ?p.path, "fetch rejected: dir mismatch"
                );
                if p.pending.as_ref() == Some(&(r.clone(), grouped)) {
                    p.pending = None;
                }
                return;
            }
            p.window_start = start;
            p.window = rows;
            p.window_is_grouped = grouped;
            let cleared = p.pending.as_ref() == Some(&(r.clone(), grouped));
            if cleared {
                p.pending = None;
            }
            tracing::trace!(
                target: "mo_ui::window",
                pane, tab, need = ?r, win_start = start, win_len = p.window.len(),
                elapsed_ms = elapsed, cleared_pending = cleared, "fetch done"
            );
            cx.notify();
        });
        // 缩略图**不在这里**派发：窗口带着上下各 BUFFER(=100) 条的余量，按整窗口
        // 派发等于每进一个目录就白解一两百张大图（单张 80ms 级，目录越大越糟）。
        // 真正的派发点是渲染那一帧——只给看得见的行排队（见 `file_list` 里的
        // `want_thumbs`）。
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_scale_with_available_width() {
        assert_eq!(columns_for(cell_width(ViewMode::List), 800.0), 1);
        // 可用 824：扣 24 留白后 /116 ≈ 6 列
        assert_eq!(columns_for(cell_width(ViewMode::Grid), 824.0), 6);
        // 窄窗口退化为 1 列，而不是 0 列导致除零。
        assert_eq!(columns_for(cell_width(ViewMode::Grid), 40.0), 1);
        assert_eq!(columns_for(cell_width(ViewMode::Gallery), 824.0), 4);
    }

    /// **四个视图的槽位 → 位图档位**，穷举 `ViewMode::ALL`。
    ///
    /// 这是「系统图标四个视图统一」那条链路里唯一能自动验的一环：视图侧（本模块）
    /// 只有「我这个槽位多大」，选哪一档是 `mo_app::icon::icon_px_for_slot` 的事。
    /// 两边一分叉就出事——画廊那个 96pt 的方框要是拿到 40px 的位图，就是近 5 倍
    /// 上采样（糊成一片）；反过来列表那 16pt 的部位要了 128px 的图，则每进一个目录
    /// 都白花十倍主线程时间。
    #[test]
    fn every_view_mode_maps_to_the_expected_icon_bucket() {
        for mode in ViewMode::ALL {
            let want = match mode {
                // 行内小槽位（列表行高 24px 里那 16pt；列视图的行是同款行高）。
                ViewMode::List | ViewMode::Columns => mo_app::ICON_PX_SMALL,
                // 网格 / 画廊的位图铺满那个方框：36pt / 96pt 都在大档。
                ViewMode::Grid | ViewMode::Gallery => mo_app::ICON_PX_LARGE,
            };
            assert_eq!(
                mo_app::icon_px_for_slot(icon_slot(mode)),
                want,
                "{mode:?} 的图标槽位 {}pt 取到的位图档位不对",
                icon_slot(mode)
            );
        }
    }

    /// 缩放**只**作用于网格 / 画廊。
    ///
    /// 这是这一档功能的边界约定：列表 / 列视图的行高（24pt）、槽位（16pt）、
    /// 列数（恒 1）在任何倍率下都必须原样不动——那两种视图的几何与行内文字、
    /// 列宽计算绑在一起，跟着缩放会把整行布局一起牵动。
    #[test]
    fn zoom_only_touches_grid_and_gallery() {
        let z = Zoom(2.0);
        for mode in [ViewMode::List, ViewMode::Columns] {
            assert_eq!(z.row_height(mode), row_height(mode), "{mode:?} 行高不该变");
            assert_eq!(z.visual_box(mode), 0.0, "{mode:?} 没有方框");
            assert_eq!(z.icon_slot(mode), crate::file_item::ICON_PX);
            assert_eq!(z.columns_for(mode, 824.0), 1, "{mode:?} 恒单列");
        }
        for mode in [ViewMode::Grid, ViewMode::Gallery] {
            assert_eq!(z.visual_box(mode), visual_box(mode) * 2.0);
            assert_eq!(z.row_height(mode), row_height(mode) * 2.0);
            assert_eq!(
                z.icon_slot(mode),
                z.visual_box(mode),
                "{mode:?} 的位图槽位在**任何**倍率下都铺满方框"
            );
        }
    }

    /// 1.0× 与不缩放完全等价——默认配置下的渲染结果不能因为接了这个值对象而变。
    #[test]
    fn zoom_at_one_is_the_unzoomed_geometry() {
        for mode in ViewMode::ALL {
            assert_eq!(Zoom(1.0).row_height(mode), row_height(mode));
            assert_eq!(Zoom(1.0).visual_box(mode), visual_box(mode));
            assert_eq!(Zoom(1.0).icon_slot(mode), icon_slot(mode));
            assert_eq!(
                Zoom(1.0).columns_for(mode, 824.0),
                columns_for(cell_width(mode), 824.0)
            );
        }
    }

    /// 放大 → 一行放得下更少列，缩小 → 更多列。
    ///
    /// 列数漏乘缩放是这套几何里最容易犯的错：方框变大了、列数没变，相邻单元
    /// 就会互相压住（`cell` 是 `flex_1` + `min_w_0`，表现为名字被挤成省略号）。
    #[test]
    fn zooming_changes_how_many_columns_fit() {
        let avail = 824.0;
        let base = Zoom(1.0).columns_for(ViewMode::Grid, avail);
        let big = Zoom(mo_app::ICON_SCALE_MAX).columns_for(ViewMode::Grid, avail);
        let small = Zoom(mo_app::ICON_SCALE_MIN).columns_for(ViewMode::Grid, avail);
        assert!(big < base, "2× 时的列数应当更少：{big} vs {base}");
        assert!(small > base, "0.75× 时应当能多放几列：{small} vs {base}");
        assert!(big >= 1 && small >= 1, "再窄也至少 1 列，不能算出 0 列");
    }

    /// 倍率收口：坏值（NaN / 0 / 越界）不能让几何算成 0 或 NaN——
    /// 那会让方框边长变成 0（图标消失）或 NaN（整块布局塌掉）。
    #[test]
    fn bogus_scale_still_yields_usable_geometry() {
        for bad in [0.0, -1.0, f32::NAN, 1e9] {
            let z = Zoom(mo_app::clamp_icon_scale(bad));
            let box_px = z.visual_box(ViewMode::Grid);
            assert!(
                box_px.is_finite() && box_px > 0.0,
                "倍率 {bad} 算出的方框是 {box_px}"
            );
            assert!(
                z.columns_for(ViewMode::Grid, 824.0) >= 1,
                "倍率 {bad} 的列数塌成 0 了"
            );
        }
    }

    /// 文字倍率是**阻尼**的：缩小时不明显变小、放大时不被撑爆。
    #[test]
    fn label_scale_is_damped() {
        assert_eq!(zoom_text(1.0), 1.0);
        assert_eq!(zoom_text(mo_app::ICON_SCALE_MIN), 0.9);
        assert_eq!(zoom_text(mo_app::ICON_SCALE_MAX), 1.4);
        assert_eq!(zoom_text(f32::NAN), 1.0, "NaN 回落 1.0");
    }

    /// 位图槽位与方框的关系：网格 / 画廊**铺满方框**（和缩略图一样大），
    /// 列表 / 列视图没有方框（返回 0）——那种视图的方框替身是 `file_item::ICON_PX`。
    #[test]
    fn the_bitmap_slot_fills_the_box_in_grid_and_gallery() {
        for mode in [ViewMode::Grid, ViewMode::Gallery] {
            assert_eq!(
                icon_slot(mode),
                visual_box(mode),
                "{mode:?} 的位图槽位该铺满方框"
            );
            assert!(visual_box(mode) > 0.0, "{mode:?} 应该有方框");
        }
        for mode in [ViewMode::List, ViewMode::Columns] {
            assert_eq!(visual_box(mode), 0.0, "{mode:?} 不该有方框");
            assert_eq!(
                icon_slot(mode),
                crate::file_item::ICON_PX,
                "{mode:?} 的槽位该是行内那个小槽位"
            );
        }
    }
}
