use gpui_kit::base::ElementExt;
use gpui_kit::base::Scrollbar;
use gpui_kit::*;
use mo_core::{Entry, SortDir, SortKey, ThumbnailState};

use crate::list_columns::{ColId, ColumnLayout};
use crate::listing::BUFFER;
use crate::panel::ViewMode;
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
        // ⚠️ 分栏时两个窗格各渲染一份表头：ID 必须带 pane/tab，
        // 否则两份表头下的文本会得到相同的 a11y NodeId。
        .id(format!("mo-file-list-header-{pane}-{tab}"))
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
            .id(format!("mo-header-cell-{pane}-{tab}-{i}"))
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
                .id(format!("mo-header-divider-{pane}-{tab}-{i}"))
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
    /// shift 连选：两端以 **FileId** 表达，由 app 侧解析成条目位。
    ///
    /// 早年传的是全局下标——分组开启后列表行号 ≠ 条目位（行流里混着组头），
    /// 下标在不同空间会指到不同条目；id 在任何空间下都指同一个文件。
    Range(mo_core::FileId, mo_core::FileId),
}

/// 分组头的中文标题（mo-core 只定键，文案归 UI）。
fn group_title(key: mo_app::GroupKey) -> &'static str {
    match key {
        mo_app::GroupKey::Folder => "文件夹",
        mo_app::GroupKey::Image => "图片",
        mo_app::GroupKey::Document => "文稿",
        mo_app::GroupKey::Media => "影音",
        mo_app::GroupKey::Archive => "压缩包",
        mo_app::GroupKey::Other => "其他",
        mo_app::GroupKey::Today => "今天",
        mo_app::GroupKey::Week => "最近 7 天",
        mo_app::GroupKey::Earlier => "更早",
    }
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
    /// 斑马纹开关（配置 `ui.zebra`）。
    pub zebra: bool,
    /// 斑马纹要铺到的**总行数**（≥ `count`）：条目不足一屏时，调用方把行数补到
    /// 视口装得下的行数，多出来的行没有数据——渲染闭包里 `window.get` 取不到
    /// 自然走占位斑马纹分支（`mo-file-ph-*`），与「窗口未就绪」的占位是同一种行。
    pub fill_to: usize,
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
    // 斑马纹开关：`chrome` 借着列布局，`move` 闭包只能带走标量。
    let zebra = chrome.zebra;
    // 实际渲染的行数：真实行数与「铺满一屏」补足行数取大。占位分支用 `count`
    // 判界（下标 ≥ count 的行一定是补足行），窗口补取仍只按真实行数来。
    let total = chrome.fill_to.max(count);
    let mut list = uniform_list("mo-file-list", total, {
        // 闭包整体 move 捕获 entity；先 clone 一份给闭包，保留外层 entity 供后续 on_mouse_down / on_prepaint 复用。
        let entity = entity.clone();
        move |range, _window, cx| {
            let need_start = range.start.saturating_sub(BUFFER);
            let need_end = (range.end + BUFFER).min(count);

            // 1) 保证窗口覆盖可见区，不覆盖则异步补窗（内部会跳过行的测量调用）。
            crate::listing::ensure_window(&entity, pane, tab, need_start, need_end, &range, cx);

            // 3) 渲染：只从窗口快照里取行。
            let mut rows: Vec<AnyElement> = Vec::with_capacity(range.len());
            // 这一帧「看得见的、还等着缩略图」的行——行渲染完统一派发（见下方注释）。
            let mut want_thumbs: Vec<Entry> = Vec::new();
            let view = entity.read(cx);
            let Some(panel) = view.panel_at(pane, tab) else {
                return rows;
            };
            // 只对**真实行**判断窗口覆盖；下标 ≥ count 的补足行没有数据可取，
            // 参与判断会让这条日志每帧都冒出来。
            let real_end = range.end.min(count);
            if range.start < real_end
                && (range.start..real_end).any(|i| {
                    panel
                        .window
                        .get(i.wrapping_sub(panel.window_start))
                        .is_none()
                })
            {
                tracing::debug!(
                    target: "mo_ui::window",
                    pane, tab, range = ?range, win_start = panel.window_start,
                    win_len = panel.window.len(), pending = ?panel.pending,
                    count, "rendering placeholder rows"
                );
            }
            for i in range.clone() {
                let offset = i.wrapping_sub(panel.window_start);
                let Some(row) = panel.window.get(offset) else {
                    // 窗口还没补上这一行（刚切目录、快速滚动、快照落地前）：画一条
                    // **只有底色、没有任何内容**的空行，也就是「斑马纹占位」。
                    //
                    // ⚠️ 这里曾经画的是 `…`：切到内容少的目录时会整屏省略号闪一下，
                    // 比空白更刺眼；快速滚动时更是滚一路闪一路。空行则安静得多——
                    // 真数据到位时是文字直接浮现在同一块底色上，连底色都不跳。
                    //
                    // 底色规则必须与下面的数据行**逐字一致**（同样受 `zebra` 开关
                    // 控制、同样 `px(4.0)`）：一旦不一致，占位期与加载完的底色会差
                    // 半格，看起来像整块列表在抖。
                    //
                    // ⚠️ 行 ID 不能省：`uniform_list` 的列表项没有逐项 ID，而可见区
                    // 通常同时有多条占位行；不给行 ID 的话它们会共享同一条元素 ID
                    // 路径 → 相同的 a11y NodeId → 辅助功能开启时 panic（启动快照未
                    // 回填满屏占位行，正是崩溃现场）。
                    rows.push(
                        div()
                            .id(format!("file-ph-{pane}-{tab}-{i}"))
                            .flex()
                            .flex_row()
                            .items_center()
                            .w_full()
                            .h(px(24.0))
                            .px(px(4.0))
                            .bg(if zebra && i % 2 == 1 {
                                crate::theme::zebra()
                            } else {
                                crate::theme::surface()
                            })
                            // 测试用（release no-op）：按绝对行号定位占位行，
                            // 断言「有底色、无内容、行高与数据行一致」。
                            .debug_selector(move || format!("mo-file-ph-{i}"))
                            .into_any_element(),
                    );
                    continue;
                };
                // 分组头行（仅分组开启的列表视图会有）：固定斑马底 + 弱化小字，
                // 行高与数据行一致（24px）——虚拟化列表的行高必须恒定，也保住
                // 框选 / 键盘导航的「行号 → 鼠标 y」换算不用分叉。
                let Some(entry) = row.entry() else {
                    let mo_app::WindowRow::Header(key) = row else {
                        unreachable!("窗口行非条目即组头");
                    };
                    rows.push(
                        div()
                            .id(format!("file-hd-{pane}-{tab}-{i}"))
                            .flex()
                            .flex_row()
                            .items_center()
                            .w_full()
                            .h(px(24.0))
                            .px(px(4.0))
                            .bg(crate::theme::zebra())
                            .debug_selector(move || format!("mo-file-hd-{i}"))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(crate::theme::muted())
                                    .child(text!(group_title(*key))),
                            )
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
                // 分栏对比：开着时按这一条的状态上色（相同不染）。
                let tint = panel
                    .diff
                    .as_ref()
                    .and_then(|m| m.get(&entry_path))
                    .copied();
                let mut row = div()
                    .id(format!("file-row-{pane}-{tab}-{i}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .h(px(24.0))
                    .px(px(4.0))
                    // Finder 列表视图：选中行蓝底；未选中按奇偶交替斑马纹（可关）。
                    // 对比色排在斑马纹**前面**：它比「奇偶行」信息量大得多。
                    .bg(if selected {
                        crate::theme::selected_bg()
                    } else if let Some(c) = crate::app::compare_tint(tint) {
                        c
                    } else if zebra && i % 2 == 1 {
                        crate::theme::zebra()
                    } else {
                        crate::theme::surface()
                    })
                    // 测试用（release no-op）：按绝对行号定位，断言首行相对列表顶部的留白。
                    .debug_selector(move || format!("mo-file-row-{i}"));

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
                        // `is_dir` 来自 `entry.kind`（列表模型），不是 `Path::is_dir()`
                        // ——远程目录在本地磁盘上不存在，见 `RootView::open_entry`。
                        entity_click
                            .update(cx, |v, cx| v.open_entry(entry_path.clone(), is_dir, cx));
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
                            // 窗口里的条目序 = 全局序的连续切片，本地先用它算；
                            // app 侧回灌走 id 端点（分组行空间下下标会错位）。
                            let ordered: Vec<mo_core::FileId> =
                                p.window_entries().map(|e| e.id).collect();
                            let clicked = ordered.iter().position(|x| *x == id)?;
                            if let Some(a) = p.selection.anchor() {
                                if let Some(ai) = ordered.iter().position(|x| *x == a) {
                                    p.selection.clear();
                                    p.selection.select_range(&ordered, ai, clicked);
                                    p.selection.set_anchor(a);
                                    return Some((p.app.clone(), SelSync::Range(a, id)));
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
                        SelSync::Range(anchor, clicked) => {
                            cx.spawn(async move |_cx| {
                                // select_between 只 insert 不清空，连选前先清掉旧选区，
                                // 否则 app 侧选择会累积、导致后续复制 / 移动选错文件。
                                app.clear_selection().await;
                                app.select_between(anchor, clicked).await;
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
                            v.open_context_menu(
                                Some((ctx_path.clone(), is_dir)),
                                x,
                                y,
                                pane,
                                tab,
                                cx,
                            );
                        });
                        cx.stop_propagation();
                    });

                let app = view.panel_at(pane, tab).map(|p| p.app.clone());
                let tag_color = app.as_ref().and_then(|a| a.tag_of(&entry.path));
                // 缩略图：**只为这一行真的要画缩略图、且还没人要过**的时候排一次队。
                //
                // 放在渲染路径上（而不是「窗口抓回来时」）是有意的：缩略图的语义就是
                // 「把看得见的这几行画出来」，而窗口带着上下各 BUFFER(=100) 条的余量——
                // 按整个窗口派发等于每进一个目录就白解一两百张大图（实测单张 80ms 级），
                // 目录越大白解得越多，正是「内容多的目录会卡」。请求本身是幂等的
                // （在跑 / 已生成 / 生成失败的都会被调度器跳过），所以每帧调也没关系，
                // 好处是滚动时新露出来的行立刻能排上队。
                if matches!(entry.thumbnail, ThumbnailState::Idle) && entry.supports_thumbnail() {
                    want_thumbs.push(entry.clone());
                }
                // 系统图标（访达同款 PNG）：判据与取图都收在 `file_item::system_icon` 里
                // ——**四个视图共用同一条链路**（列视图 / 网格 / 画廊见各自的 mod 文档），
                // 那边一句话说清了「有缩略图的行不问」「查表命不中只记账」。
                //
                // 槽位按 `listing::icon_slot` 那张表取：系统图标是光栅图，16pt 的行有
                // 40px 的位图就够了，不必替它去取画廊（96pt 方框）要的那一档。
                let system_icon = crate::file_item::entry_system_icon(
                    app.as_ref(),
                    entry,
                    crate::listing::icon_slot(ViewMode::List),
                );
                rows.push(
                    row.child(crate::file_item::view(
                        entry,
                        selected,
                        tag_color,
                        &row_cols,
                        system_icon,
                    ))
                    .into_any_element(),
                );
            }
            // 缩略图：这一帧看得见的、还等着的那几行，统一派发。
            //
            // 幂等（在跑 / 已生成 / 生成失败的都会被调度器跳过），所以每帧调也无所谓；
            // 关键是对象正好是**看得见的那些行**，而不是整窗口那两百条——见上面
            // `want_thumbs` 处的注释。
            if !want_thumbs.is_empty() {
                if let Some(a) = view.panel_at(pane, tab).map(|p| p.app.clone()) {
                    a.thumbs().request(a.clone(), want_thumbs);
                }
            }
            rows
        }
    })
    // ⚠️ 必需：`uniform_list` 的列表项只在 prepaint 阶段渲染，布局阶段 taffy
    // 看到的是「没有子节点」的元素，身高算出来是 0。不显式给它确定高度
    // （flex_1 / size_full / h(...)），整个文件列表就会被压成 0 高。
    .flex_1()
    // 四周留白：行的 hover / 选中底色不顶到窗口边缘（Finder 式呼吸感）。
    // 上下这 12pt 与网格同源（见 `grid::PAD`、devlog §25）：`uniform_list` 的 padding
    // 四个方向都吃——top 加到条目起点、上下都算进滚动内容高度，所以滚到底最后一行
    // 下面也留得出来，不是只把首行往下推。原来这里只有 `px`，首行顶着表头、
    // 末行贴着状态栏。
    .p(px(12.0))
    // 滚轮 / 触控板滚动经此 handle 走，滚动条拖动也写回同一 handle。
    .track_scroll(scroll)
    // 测试用：让 tests/layout.rs 能读到这个元素的实际尺寸（release 下 no-op）。
    .debug_selector(|| "mo-file-list".to_string());

    // 框选起点：只在列表**空白区**按下才启动橡皮筋（点到条目走单选 / 拖拽）。
    // 拖动过程与收尾在 `RootView` 的窗口级 `on_mouse_move` / `on_mouse_up` 里处理。
    let entity_box = entity.clone();
    list.interactivity()
        .on_mouse_down(MouseButton::Left, move |ev, _window, cx| {
            let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
            let extend = ev.modifiers.platform || ev.modifiers.control || ev.modifiers.shift;
            entity_box.update(cx, |v, cx| {
                v.start_box_selection_if_empty(pane, tab, x, y, extend, cx)
            });
        });

    // 滚动条作为兄弟节点覆盖在列表右侧（容器 relative），
    // 与 gpui-component List 的做法一致：overlay 而非挤压内容宽度。
    // 表头固定在滚动区上方，不随内容滚动（Finder 列表视图行为）。
    // 框选几何：prepaint 把列表内容区左上角（窗口坐标）回写，供把鼠标 y 折算成行下标。
    // 挂在包裹列表的 div 上（UniformList 自身没有 on_prepaint），其 bounds 即列表区左上角。
    let entity_origin = entity.clone();
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
                .on_prepaint(move |bounds, _window, cx| {
                    let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                    let h = f32::from(bounds.size.height);
                    entity_origin.update(cx, |v, cx| {
                        // 高度变化才 notify：首帧拿到真实高度后下一帧才能把
                        // 斑马纹补满一屏；此后每帧相等，不会造成重绘循环。
                        if v.set_list_origin(pane, tab, x, y, h) {
                            cx.notify();
                        }
                    });
                })
                .child(list)
                .child(Scrollbar::vertical(scroll)),
        )
}
