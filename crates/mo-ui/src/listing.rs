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
    let app_task = app.clone();
    tracing::info!(
        target: "mo_ui::window",
        pane, tab, need = ?r, visible = ?visible, "fetch spawn"
    );
    let spawned = std::time::Instant::now();
    cx.spawn(async move |cx| {
        let (dir_path, start, entries) = app.visible_window(r.clone()).await;
        let elapsed = spawned.elapsed().as_millis();
        let for_thumbs = entries.clone();
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
            tracing::info!(
                target: "mo_ui::window",
                pane, tab, need = ?r, win_start = start, win_len = p.window.len(),
                elapsed_ms = elapsed, cleared_pending = cleared, "fetch done"
            );
            cx.notify();
        });
        // 只为进入窗口的条目生成缩略图，绝不「打开目录就全量生成」。
        app_task.thumbs().request(app_task.clone(), for_thumbs);
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
}
