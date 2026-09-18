use gpui_kit::base::Scrollbar;
use gpui_kit::*;
use mo_core::{SortDir, SortKey};

use crate::list_columns::{ColId, ColumnLayout};
use crate::listing::BUFFER;
use crate::RootView;

/// 表头单元格之间的水平间距（必须与数据行的 `gap` 一致，否则列会错位）。
pub(crate) const HEADER_GAP: f32 = 8.0;

/// 拖动超过这么多像素才算「拖动列」，否则算「点了一下表头」。
pub(crate) const DRAG_THRESHOLD: f32 = 4.0;

/// 列分隔线的**命中区**宽度：线本身只有 1px，靠它才抓得住。
const DIVIDER_HIT_W: f32 = 7.0;
/// 分隔线的常态线宽 / 拖动中的线宽。
const DIVIDER_W: f32 = 1.0;
const DIVIDER_W_ACTIVE: f32 = 2.0;
/// 分隔线的高度：表头 26px 里上下各留 6px 间距，线不顶边（命中区仍满高）。
const DIVIDER_H: f32 = 14.0;

/// Finder 列表视图式表头：列序 / 列宽全部来自 `cols`。
///
/// 三种交互：
/// * **点击**列头 → 切换排序（同一列再点一次翻转升降序）；
/// * **拖动**列头 → 调整列顺序；
/// * **拖动**列间的分隔线（每列左缘那条竖线，光标变 ⇔）→ 调整列宽。
///
/// ⚠️ 名称列是弹性列，首帧渲染前它的实际宽度是未知的，所以拖动落点判定
/// 依赖 `on_children_prepainted` 回写到 `RootView.header_cells` 的真实 bounds
/// （上一帧的值，拖动态下足够准）。
pub(crate) fn header(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    cols: &ColumnLayout,
    sort: (SortKey, SortDir),
    dragging: Option<ColId>,
    resizing: Option<ColId>,
) -> impl IntoElement {
    let order = cols.order.clone();
    // ⚠️ 两层结构不是冗余：`on_children_prepainted` 只存在于 `Div` 上，
    // 而带 `.id()` 的元素会变成 `Stateful<Div>`（拿不到该方法）。
    // 于是外层的 Stateful 行负责鼠标事件，内层的裸 Div 负责 Cells 的测量。
    // 单元格上的事件依旧冒泡到外层，不影响三种交互。
    let mut outer = div()
        .id("mo-file-list-header")
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .h(px(26.0))
        .px(px(16.0))
        .bg(crate::theme::container())
        .border_b_1()
        .border_color(crate::theme::separator())
        .text_size(px(11.0))
        .text_color(crate::theme::muted())
        .debug_selector(|| "mo-file-list-header".to_string());

    // 拖动过程与收尾都放在表头行这一层：指针在表头内任意位置移动 / 抬起
    // 都能收到事件（单元格只负责「按下时记录起点」）。
    let entity_move = entity.clone();
    outer.interactivity().on_mouse_move(move |ev, _window, cx| {
        let x = f32::from(ev.position.x);
        entity_move.update(cx, |v, cx| {
            if v.header_mouse_move(x) {
                cx.notify();
            }
        });
    });
    let entity_up = entity.clone();
    outer
        .interactivity()
        .on_mouse_up(MouseButton::Left, move |ev, _window, cx| {
            let x = f32::from(ev.position.x);
            entity_up.update(cx, |v, cx| v.header_mouse_up(x, cx));
        });

    // 内层：单元格容器，prepaint 时把每列真实 bounds 写回 RootView。
    let entity_paint = entity.clone();
    let order_for_paint = order.clone();
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_1()
        .h_full()
        .min_w_0()
        .gap(px(HEADER_GAP))
        .on_children_prepainted(move |bounds, _window, cx| {
            let cells: Vec<(ColId, Bounds<Pixels>)> =
                order_for_paint.iter().copied().zip(bounds).collect();
            entity_paint.update(cx, |v, _cx| v.set_header_cells(pane, tab, cells));
        });

    for (i, col) in order.iter().copied().enumerate() {
        let sorted = sort.0 == col.sort_key();
        let mut cell = div()
            .id(("mo-header-cell", i))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .h_full()
            .flex_shrink_0()
            .rounded(px(3.0))
            .debug_selector(move || format!("mo-header-cell-{}", col_key(col)))
            .hover(|s| s.bg(crate::theme::hover_bg()));
        // 正在被拖动换位的那一列：常亮底色 + 深色字，让用户知道「搬的是它」。
        if dragging == Some(col) {
            cell = cell
                .bg(crate::theme::hover_bg())
                .text_color(crate::theme::text());
        }
        cell = if col.is_flex() {
            // 弹性列不设 min_w：数据行的名称列同样没有下限，
            // 两边收缩规则必须一致，否则窄窗格下表头与数据行会错位。
            cell.flex_1().min_w(px(0.0))
        } else {
            cell.w(px(cols.width(col)))
        };
        if col.is_right_aligned() {
            cell = cell.justify_end();
        }
        cell = cell.child(text!(col.title().to_string()));
        if sorted {
            // 排序指示箭头：升序 ↑、降序 ↓（与数据行同色，不额外抢眼）。
            cell = cell.child(crate::icons::icon(
                if sort.1 == SortDir::Asc {
                    crate::icons::ARROW_UP
                } else {
                    crate::icons::ARROW_DOWN
                },
                10.0,
                crate::theme::text(),
            ));
        }

        // 按下列头 = 可能要拖列（收尾时若没移动就是点击排序）。
        // ⚠️ 顺序敏感：分隔条是单元格的子节点，内层先派发并写入 Resizing，
        // 这里再判断已有拖拽态就不再覆盖，避免调整列宽被误当成拖列。
        let entity_cell = entity.clone();
        cell.interactivity()
            .on_mouse_down(MouseButton::Left, move |ev, _window, cx| {
                let x = f32::from(ev.position.x);
                entity_cell.update(cx, |v, _cx| v.header_mouse_down_cell(pane, tab, col, x));
            });

        // 分隔线挂在**左缘**（第一列没有）：4 列就有 3 条线，
        // 「名称 | 修改日期」这条也在——挂在右缘的方案里它是缺的。
        // 线本身常显（拖动的把手必须先看得见），外圈 7px 是命中区。
        if i > 0 {
            let active = resizing == Some(col);
            let line_w = if active { DIVIDER_W_ACTIVE } else { DIVIDER_W };
            let line_color = if active {
                crate::theme::muted()
            } else {
                crate::theme::divider()
            };
            let entity_divider = entity.clone();
            let mut divider = div()
                .id(("mo-header-divider", i))
                .absolute()
                .top(px(0.0))
                // 间隙中心 = 本列左缘 - HEADER_GAP/2，命中区以它为对称轴居中。
                .left(px(-(HEADER_GAP / 2.0 + DIVIDER_HIT_W / 2.0)))
                .h_full()
                .w(px(DIVIDER_HIT_W))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .cursor(CursorStyle::ResizeLeftRight)
                .debug_selector(move || format!("mo-header-divider-{}", col_key(col)));
            divider
                .interactivity()
                .on_mouse_down(MouseButton::Left, move |ev, _window, cx| {
                    let x = f32::from(ev.position.x);
                    // 按下即重绘一次：分隔线切到「拖动中」的加粗态。
                    entity_divider.update(cx, |v, cx| {
                        v.header_mouse_down_divider(col, x);
                        cx.notify();
                    });
                });
            // 线体本身上下内缩、不顶边；命中区（divider）仍是满高，
            // 视觉上更轻，抓取手感不受影响。
            let line = div()
                .w(px(line_w))
                .h(px(DIVIDER_H))
                .bg(line_color)
                .debug_selector(move || format!("mo-header-divider-line-{}", col_key(col)));
            cell = cell.child(divider.child(line));
        }
        row = row.child(cell);
    }
    outer.child(row)
}

/// 列的可读键名（仅用于 debug selector / 测试定位）。
pub(crate) fn col_key(col: ColId) -> &'static str {
    match col {
        ColId::Name => "name",
        ColId::Date => "date",
        ColId::Size => "size",
        ColId::Kind => "kind",
    }
}

/// 把本地选择变化同步到 `AppState` 的动作。
///
/// 本地 `panel.selection` 立即反馈渲染，远端 `app` 侧是操作（复制 / 移动 / 删除）
/// 的唯一事实来源，因此每一项本地改动都要异步回灌一次。
enum SelSync {
    /// 单选替换。
    Select(mo_core::FileId),
    /// cmd/ctrl 切换这一项。
    Toggle(mo_core::FileId),
    /// shift 连选：`from`/`to` 为可见列表的全局下标。
    Range(usize, usize),
}

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
/// 列表视图的「外壳」状态：列布局 + 排序 + 正在拖动的列。
///
/// 打包传递而不是逐个当参数：`render` 已经有 5 个位置参数，再加 3 个会撞上
/// clippy 的 `too_many_arguments`。
pub struct ListChrome<'a> {
    pub cols: &'a ColumnLayout,
    pub sort: (SortKey, SortDir),
    /// 正在被拖动换位的列（整列高亮）。
    pub dragging: Option<ColId>,
    /// 正在被拖动的那条列分隔线（右侧的那一列）——加粗显示。
    pub resizing: Option<ColId>,
}

/// `pane` / `tab` 指明渲染的是哪个标签页：多标签页与分栏共享同一个实现。
/// `chrome` 是列布局（顺序 + 宽度）与排序态，表头与数据行共用，
/// 用户拖列 / 点表头后下一帧即生效。
pub fn render(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    count: usize,
    scroll: &UniformListScrollHandle,
    chrome: ListChrome<'_>,
) -> impl IntoElement {
    let entity = entity.clone();
    let header = header(
        &entity,
        pane,
        tab,
        chrome.cols,
        chrome.sort,
        chrome.dragging,
        chrome.resizing,
    );
    // 数据行用的列布局：克隆一份小结构（4 个列 + 4 个宽度），每帧成本可忽略。
    let row_cols = chrome.cols.clone();
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
                // Finder 列表视图：选中行蓝底；未选中按奇偶交替斑马纹。
                .bg(if selected {
                    crate::theme::selected_bg()
                } else if i % 2 == 1 {
                    crate::theme::zebra()
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
                // 修饰键决定选择语义（Finder / 资源管理器一致）：
                // 无修饰 = 单选替换；cmd/ctrl = 切换多选；shift = 从锚点连选。
                let mods = ev.modifiers();
                let multi = mods.platform || mods.control;
                let shift = mods.shift;

                let Some((app, sync)) = entity_click.update(cx, |v, _cx| {
                    let p = v.panel_at_mut(pane, tab)?;
                    if shift {
                        // 以锚点为起点延伸到点击项；锚点缺失则退化为单选。
                        let ordered: Vec<mo_core::FileId> = p.window.iter().map(|e| e.id).collect();
                        let clicked = ordered.iter().position(|x| *x == id)?;
                        if let Some(a) = p.selection.anchor() {
                            if let Some(ai) = ordered.iter().position(|x| *x == a) {
                                p.selection.clear();
                                p.selection.select_range(&ordered, ai, clicked);
                                p.selection.set_anchor(a);
                                return Some((
                                    p.app.clone(),
                                    SelSync::Range(p.window_start + ai, p.window_start + clicked),
                                ));
                            }
                        }
                        p.selection.select(id);
                        Some((p.app.clone(), SelSync::Select(id)))
                    } else if multi {
                        // cmd/ctrl 点击：在现有选择上切换这一项。
                        p.selection.toggle(id);
                        Some((p.app.clone(), SelSync::Toggle(id)))
                    } else {
                        // 普通点击：单选替换，取消其余选中。
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
                            // select_range 只 insert 不清空，连选前先清掉旧选区，
                            // 否则 app 侧选择会累积、导致后续复制 / 移动选错文件。
                            app.clear_selection().await;
                            app.select_range(from, to).await;
                        })
                        .detach();
                    }
                }
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

            // 右键：对着这一行弹上下文菜单。必须 `stop_propagation`，
            // 否则事件继续冒泡到窗格容器，菜单会被随即替换成「空白处」版本。
            let entity_ctx = entity.clone();
            let ctx_path = entry.path.clone();
            row.interactivity()
                .on_mouse_down(MouseButton::Right, move |ev, _window, cx| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    entity_ctx.update(cx, |v, cx| {
                        v.open_context_menu(Some((ctx_path.clone(), is_dir)), x, y, pane, tab, cx);
                    });
                    cx.stop_propagation();
                });

            let tag_color = view
                .panel_at(pane, tab)
                .map(|p| p.app.clone())
                .and_then(|app| app.tag_of(&entry.path));
            rows.push(
                row.child(crate::file_item::view(
                    entry, selected, tag_color, &row_cols,
                ))
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
    // 表头固定在滚动区上方，不随内容滚动（Finder 列表视图行为）。
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(header)
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(list)
                .child(Scrollbar::vertical(scroll)),
        )
}
