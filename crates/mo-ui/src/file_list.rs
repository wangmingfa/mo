use std::ops::Range;

use gpui_kit::*;
use mo_app::AppState;

use crate::RootView;

/// 可见区上下各多取这么多行：滚动时不会频繁出现占位，又不会一次抓太多。
const BUFFER: usize = 100;

/// 文件列表：`UniformList` 虚拟化 + 窗口懒加载。
///
/// 两层机制缺一不可：
/// * **虚拟化**保证只渲染可见行的 element；
/// * **窗口懒加载**保证 UI 侧也只持有可见区的条目快照——
///   十万条目的目录若每次同步都克隆整份列表，光克隆就能卡住主线程，
///   虚拟化的收益会被全部吃掉。
///
/// 滚出窗口的行先显示占位，异步取回后自动补上。
pub fn render(entity: &Entity<RootView>, count: usize) -> impl IntoElement {
    let entity = entity.clone();
    uniform_list("mo-file-list", count, move |range, _window, cx| {
        let need_start = range.start.saturating_sub(BUFFER);
        let need_end = (range.end + BUFFER).min(count);

        // 1) 判断当前窗口是否覆盖可见区，不覆盖则记下要补的范围。
        let mut request: Option<(AppState, Range<usize>)> = None;
        entity.update(cx, |v, _cx| {
            let covered = !v.window.is_empty()
                && need_start >= v.window_start
                && need_end <= v.window_start + v.window.len();
            if !covered && v.pending.as_ref() != Some(&(need_start..need_end)) {
                v.pending = Some(need_start..need_end);
                request = Some((v.app.clone(), need_start..need_end));
            }
        });

        // 2) 异步补窗口（只取这一屏），顺带为可见条目请求缩略图。
        if let Some((app, r)) = request {
            let this = entity.clone();
            let app_task = app.clone();
            cx.spawn(async move |cx| {
                let (start, entries) = app.visible_window(r).await;
                let for_thumbs = entries.clone();
                let _ = this.update(cx, |v, cx| {
                    v.window_start = start;
                    v.window = entries;
                    v.pending = None;
                    cx.notify();
                });
                // 只为进入窗口的条目生成缩略图，绝不「打开目录就全量生成」。
                app_task.thumbs().request(app_task.clone(), for_thumbs);
            })
            .detach();
        }

        // 3) 渲染：只从窗口快照里取行。
        let view = entity.read(cx);
        let mut rows = Vec::with_capacity(range.len());
        for i in range.clone() {
            let offset = i.wrapping_sub(view.window_start);
            let Some(entry) = view.window.get(offset) else {
                rows.push(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .w_full()
                        .h(px(24.0))
                        .child(text!("…".to_string())),
                );
                continue;
            };

            let id = entry.id;
            let selected = view.selection.is_selected(&id);
            let entity_click = entity.clone();

            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .h(px(24.0))
                .px(px(4.0))
                .bg(if selected {
                    crate::theme::selected_bg()
                } else {
                    crate::theme::surface()
                });

            if !selected {
                // fluent `hover` 在 `InteractiveElement` 上，`Div` 实现了它。
                row = row.hover(|s| s.bg(crate::theme::hover_bg()));
            }

            // `Div` 只实现 `InteractiveElement`（提供 `interactivity()`），
            // fluent `on_click` 在 `StatefulInteractiveElement`（Div 未实现），
            // 因此点击回调走 imperative API。
            row.interactivity().on_click(move |_, _window, cx| {
                // 本地立即反馈，再异步同步 app 侧（app 侧是唯一事实来源）。
                let app = entity_click.update(cx, |v, _cx| {
                    v.selection.toggle(id);
                    v.app.clone()
                });
                cx.spawn(async move |_cx| {
                    app.toggle(id).await;
                })
                .detach();
            });

            rows.push(row.child(crate::file_item::view(entry, selected)));
        }
        rows
    })
    // ⚠️ 必需：`uniform_list` 的列表项只在 prepaint 阶段渲染，布局阶段 taffy
    // 看到的是「没有子节点」的元素，身高算出来是 0。不显式给它确定高度
    // （flex_1 / size_full / h(...)），整个文件列表就会被压成 0 高。
    .flex_1()
    // 测试用：让 tests/layout.rs 能读到这个元素的实际尺寸（release 下 no-op）。
    .debug_selector(|| "mo-file-list".to_string())
}
