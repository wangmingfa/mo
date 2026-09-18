//! columns：Miller 列视图（逐级展开当前选中目录）。
//!
//! 数据来自 [`AppState::list_dir`]——**不走主目录模型**，因此切到列视图
//! 不会污染导航栈 / 选择 / 监听目标；离开列视图时这些列数据也随之弃用。
//!
//! 主列表用的是「虚拟化 + 窗口懒加载」，这里每列默认最多渲染
//! [`MAX_PER_COLUMN`] 条：列视图一次只展示一级目录，且更大的收益在于
//! 逐级下钻而非滚动，因此对超大目录做上限提示更实际。

use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_core::EntryKind;

use crate::panel::ColumnData;
use crate::{theme, RootView};

/// 单列最多渲染的条目数（超出给出「省略」提示）。
const MAX_PER_COLUMN: usize = 2000;

/// 单列宽度与行高。
const COLUMN_WIDTH: f32 = 210.0;
const ROW_HEIGHT: f32 = 24.0;

pub fn render(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    columns_data: &[ColumnData],
) -> impl IntoElement {
    let mut row = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w_0()
        .px(px(12.0))
        .gap(px(8.0))
        // 列数超过视口宽度时可横向滚动（多列列的宽度超过窗格 → 横向滚动条）。
        .overflow_x_scrollbar();
    if columns_data.is_empty() {
        return row.child(
            div()
                .text_color(theme::muted())
                .child(text!("正在读取目录…".to_string())),
        );
    }
    for (i, data) in columns_data.iter().enumerate() {
        row = row.child(column_box(entity, pane, tab, i, data));
    }
    row
}

/// 一个列的标题与内容。
fn column_box(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    index: usize,
    data: &ColumnData,
) -> Stateful<Div> {
    let mut col = div()
        // ⚠️ 多列并存，且列头 / 空列 / 截断提示文本都挂在无 ID 的容器上：
        // 列容器必须有唯一 ID，否则同一 `text!` 站点在各列重复出现时
        // 会产生重复的 a11y NodeId。
        .id(format!("col-box-{pane}-{tab}-{index}"))
        .flex()
        .flex_col()
        .w(px(COLUMN_WIDTH))
        .flex_shrink_0()
        .h_full()
        .rounded(px(6.0))
        .border_1()
        .border_color(theme::separator())
        .bg(theme::surface());

    col = col.child(
        div()
            .px(px(8.0))
            .py(px(4.0))
            .border_b_1()
            .border_color(theme::separator())
            .text_size(px(11.0))
            .text_color(theme::muted())
            .child(text!(data.path.display().to_string())),
    );

    let mut body = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scrollbar();
    for (i, e) in data.entries.iter().take(MAX_PER_COLUMN).enumerate() {
        let selected = data.cursor == i;
        let mut line = div()
            .id(format!("col-{pane}-{tab}-{index}-{i}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(6.0))
            .bg(if selected {
                theme::selected_bg()
            } else {
                theme::surface()
            })
            .text_color(if selected {
                theme::selected_text()
            } else {
                theme::text()
            });
        if !selected {
            line = line.hover(|s| s.bg(theme::hover_bg()));
        }
        let is_dir = matches!(e.kind, EntryKind::Directory);
        // 右键：对着这一行弹上下文菜单（`stop_propagation` 防止冒泡到窗格容器）。
        let ctx_entity = entity.clone();
        let ctx_path = e.path.clone();
        line.interactivity()
            .on_mouse_down(MouseButton::Right, move |ev, _window, cx| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                ctx_entity.update(cx, |v, cx| {
                    v.open_context_menu(Some((ctx_path.clone(), is_dir)), x, y, pane, tab, cx);
                });
                cx.stop_propagation();
            });
        let click_entity = entity.clone();
        let entry_path = e.path.clone();
        line.interactivity().on_click(move |ev, _window, cx| {
            click_entity.update(cx, |v, cx| {
                if ev.click_count() >= 2 {
                    v.open_entry(entry_path.clone(), cx);
                    return;
                }
                v.set_column_cursor(pane, tab, index, i);
                if is_dir {
                    // 逐级下钻：选中目录就展开它的子列（替换掉更深层的列）。
                    v.load_column(cx, entry_path.clone(), Some(index), pane, tab);
                }
                cx.notify();
            });
        });
        line = line
            .child(crate::icons::icon(
                crate::icons::icon_for_kind_and_name(e.kind, &e.name),
                16.0,
                if selected {
                    theme::selected_text()
                } else {
                    theme::text()
                },
            ))
            .child(div().flex_1().truncate().child(text!(e.name.clone())));
        body = body.child(line);
    }
    if data.entries.len() > MAX_PER_COLUMN {
        body = body.child(
            div()
                .px(px(6.0))
                .py(px(4.0))
                .text_size(px(11.0))
                .text_color(theme::muted())
                .child(text!(format!(
                    "…省略 {} 条",
                    data.entries.len() - MAX_PER_COLUMN
                ))),
        );
    }
    if data.entries.is_empty() {
        body = body.child(
            div()
                .px(px(6.0))
                .py(px(4.0))
                .text_color(theme::muted())
                .child(text!("（空）".to_string())),
        );
    }
    col.child(body)
}
