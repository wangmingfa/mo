use gpui_kit::base::Scrollbar;
use gpui_kit::*;

use crate::listing::BUFFER;
use crate::RootView;

/// 文件列表：`UniformList` 虚拟化 + 窗口懒加载。
///
/// 两层机制缺一不可：
/// * **虚拟化**保证只渲染可见行的 element；
/// * **窗口懒加载**保证 UI 侧也只持有可见区的条目快照——
///   十万条目的目录若每次同步都克隆整份列表，光克隆就能卡住主线程，
///   虚拟化的收益会被全部吃掉。
///
/// 滚出窗口的行先显示占位，异步取回后自动补上。
///
/// 滚动条：`UniformListScrollHandle` 同时驱动滚轮与 `Scrollbar::vertical`
/// 覆盖层（它实现了 `ScrollbarHandle`）。handle 由面板持有——
/// 每帧新建会把滚动位置清零。
///
/// `pane` / `tab` 指明渲染的是哪个标签页：多标签页与分栏共享同一个实现。
pub fn render(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    count: usize,
    scroll: &UniformListScrollHandle,
) -> impl IntoElement {
    let entity = entity.clone();
    let list = uniform_list("mo-file-list", count, move |range, _window, cx| {
        let need_start = range.start.saturating_sub(BUFFER);
        let need_end = (range.end + BUFFER).min(count);

        // 1) 保证窗口覆盖可见区，不覆盖则异步补窗（内部会跳过行的测量调用）。
        crate::listing::ensure_window(&entity, pane, tab, need_start, need_end, &range, cx);

        // 3) 渲染：只从窗口快照里取行。
        let mut rows: Vec<AnyElement> = Vec::with_capacity(range.len());
        let view = entity.read(cx);
        let Some(panel) = view.panel_at(pane, tab) else {
            return rows;
        };
        if range.clone().any(|i| {
            panel
                .window
                .get(i.wrapping_sub(panel.window_start))
                .is_none()
        }) {
            tracing::debug!(
                target: "mo_ui::window",
                pane, tab, range = ?range, win_start = panel.window_start,
                win_len = panel.window.len(), pending = ?panel.pending,
                count, "rendering placeholder rows"
            );
        }
        for i in range.clone() {
            let offset = i.wrapping_sub(panel.window_start);
            let Some(entry) = panel.window.get(offset) else {
                rows.push(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .w_full()
                        .h(px(24.0))
                        .child(text!("…".to_string()))
                        .into_any_element(),
                );
                continue;
            };

            let id = entry.id;
            let entry_path = entry.path.clone();
            let selected = panel.selection.is_selected(&id);
            let is_dir = entry.kind.is_dir();
            let entity_click = entity.clone();

            // ⚠️ 必须有元素 ID：gpui 的 click 事件分发依赖 element_state，
            // 无 ID 的裸 div 拿不到 state，on_click 回调永远不会注册。
            // 用全列表绝对索引保证滚动后 ID 稳定。
            let mut row = div()
                .id(format!("file-row-{pane}-{tab}-{i}"))
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
            row.interactivity().on_click(move |ev, _window, cx| {
                // 双击（click_count >= 2）：进入目录 / 预览文件，与 Enter 同语义。
                if ev.click_count() >= 2 {
                    entity_click.update(cx, |v, cx| v.open_entry(entry_path.clone(), cx));
                    return;
                }
                // 单击：本地立即反馈，再异步同步 app 侧（app 侧是唯一事实来源）。
                let Some((app, selected_app)) = entity_click.update(cx, |v, _cx| {
                    let p = v.panel_at_mut(pane, tab)?;
                    p.selection.toggle(id);
                    Some((p.app.clone(), id))
                }) else {
                    return;
                };
                cx.spawn(async move |_cx| {
                    app.toggle(selected_app).await;
                })
                .detach();
            });

            // 拖拽：按下记源、抬起结算。跨窗格拖拽落在别处时由窗格级兜底。
            let entity_down = entity.clone();
            let down_path = entry.path.clone();
            row.interactivity()
                .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                    entity_down.update(cx, |v, _cx| {
                        v.begin_drag(pane, tab, down_path.clone(), id);
                    });
                });
            let entity_up = entity.clone();
            let up_path = entry.path.clone();
            row.interactivity()
                .on_mouse_up(MouseButton::Left, move |ev, _window, cx| {
                    // 按住 ⌥（Windows / Linux 上是 Alt）拖 = 移动，否则复制。
                    let alt = ev.modifiers.alt;
                    entity_up.update(cx, |v, cx| {
                        v.drop_on_entry(pane, tab, up_path.clone(), is_dir, alt, cx);
                    });
                });

            let tag_color = view
                .panel_at(pane, tab)
                .map(|p| p.app.clone())
                .and_then(|app| app.tag_of(&entry.path));
            rows.push(
                row.child(crate::file_item::view(entry, selected, tag_color))
                    .into_any_element(),
            );
        }
        rows
    })
    // ⚠️ 必需：`uniform_list` 的列表项只在 prepaint 阶段渲染，布局阶段 taffy
    // 看到的是「没有子节点」的元素，身高算出来是 0。不显式给它确定高度
    // （flex_1 / size_full / h(...)），整个文件列表就会被压成 0 高。
    .flex_1()
    // 列表左右留白：行 hover 背景不顶到窗口边缘（Finder 式呼吸感）。
    .px(px(12.0))
    // 滚轮 / 触控板滚动经此 handle 走，滚动条拖动也写回同一 handle。
    .track_scroll(scroll)
    // 测试用：让 tests/layout.rs 能读到这个元素的实际尺寸（release 下 no-op）。
    .debug_selector(|| "mo-file-list".to_string());

    // 滚动条作为兄弟节点覆盖在列表右侧（容器 relative），
    // 与 gpui-component List 的做法一致：overlay 而非挤压内容宽度。
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(list)
        .child(Scrollbar::vertical(scroll))
}
