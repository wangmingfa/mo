//! listing：多种视图共用的**窗口懒加载**与网格列数计算。
//!
//! 列表 / 网格 / 画廊都遵循同一套「窗口」机制：UI 侧只持有可见区的条目快照，
//! 滚动到哪补哪。抽出来是为了避免三种视图各写一遍、写出三份不一致的竞态。

use std::ops::Range;

use gpui_kit::{App, Entity};
use mo_app::AppState;

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

/// 可用宽度能放下几列（至少 1 列）。
pub(crate) fn columns_for(mode: ViewMode, available: f32) -> usize {
    let w = cell_width(mode);
    if w <= 0.0 {
        return 1;
    }
    ((available - 24.0) / w).floor().clamp(1.0, 12.0) as usize
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
    let mut request: Option<(AppState, Range<usize>)> = None;
    entity.update(cx, |v, _cx| {
        let Some(p) = v.panel_at_mut(pane, tab) else {
            return;
        };
        if !p.covered(need_start, need_end) && p.pending.as_ref() != Some(&(need_start..need_end)) {
            p.pending = Some(need_start..need_end);
            request = Some((p.app.clone(), need_start..need_end));
        }
    });

    let Some((app, r)) = request else {
        return;
    };
    let this = entity.clone();
    tracing::trace!(
        target: "mo_ui::window",
        pane, tab, need = ?r, visible = ?visible, "fetch spawn"
    );
    let spawned = std::time::Instant::now();
    cx.spawn(async move |cx| {
        let (dir_path, start, entries) = app.visible_window(r.clone()).await;
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
                if p.pending.as_ref() == Some(&r) {
                    p.pending = None;
                }
                return;
            }
            p.window_start = start;
            p.window = entries;
            let cleared = p.pending.as_ref() == Some(&r);
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
        assert_eq!(columns_for(ViewMode::List, 800.0), 1);
        // 可用 824：扣 24 留白后 /116 ≈ 6 列
        assert_eq!(columns_for(ViewMode::Grid, 824.0), 6);
        // 窄窗口退化为 1 列，而不是 0 列导致除零。
        assert_eq!(columns_for(ViewMode::Grid, 40.0), 1);
        assert_eq!(columns_for(ViewMode::Gallery, 824.0), 4);
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
