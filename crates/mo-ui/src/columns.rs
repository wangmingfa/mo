//! columns：Miller 列视图（逐级展开当前选中目录）。
//!
//! 数据来自 [`AppState::list_dir`]——**不走主目录模型**，因此切到列视图
//! 不会污染导航栈 / 监听目标；离开列视图时这些列数据也随之弃用。
//! 单击行会把 **app 选择**整替成该行（`RootView::column_row_clicked`，§40），
//! 好让 F2 / 复制 / 删除这些选择模型命令打到用户真正点的那一条。
//!
//! 主列表用的是「虚拟化 + 窗口懒加载」，这里每列默认最多渲染
//! [`MAX_PER_COLUMN`] 条：列视图一次只展示一级目录，且更大的收益在于
//! 逐级下钻而非滚动，因此对超大目录做上限提示更实际。

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::Sizable as _;
use gpui_kit::*;
use mo_app::AppState;
use mo_core::EntryKind;
use std::path::{Path, PathBuf};

use crate::panel::{ColumnData, ViewMode};
use crate::{theme, RootView};

/// 单列最多渲染的条目数（超出给出「省略」提示）。
const MAX_PER_COLUMN: usize = 2000;

/// 单列宽度与行高。
const COLUMN_WIDTH: f32 = 210.0;
const ROW_HEIGHT: f32 = 24.0;

/// 列头高度：**固定一行**，不随路径长度变化。
pub(crate) const HEAD_HEIGHT: f32 = 22.0;

#[allow(clippy::too_many_arguments)]
pub fn render(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    columns_data: &[ColumnData],
    app: &AppState,
    // 分栏对比时这一页的条目状态（按路径查）；`None` = 没在对比。
    diff: Option<&std::collections::HashMap<PathBuf, mo_diff::TreeStatus>>,
    // 行内改名进行中的 `(路径, 输入框)`：命中的那一行把名字换成输入框
    //（与 `file_list` 同一套编辑器，§40 缺口 2）。
    renaming: Option<(&PathBuf, &Entity<InputState>)>,
    // 拖拽悬停认领的落点路径（§42）：命中的目录行高亮。列视图没有 draw 期
    // 闭包可读 `RootView`，由 `render_pane` 算好传进来（与 list / grid 同一判据）。
    drag_target: Option<&Path>,
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
        row = row.child(column_box(
            entity,
            pane,
            tab,
            i,
            data,
            app,
            diff,
            renaming,
            drag_target,
        ));
    }
    row
}

/// 一个列的标题与内容。
#[allow(clippy::too_many_arguments)]
fn column_box(
    entity: &Entity<RootView>,
    pane: usize,
    tab: usize,
    index: usize,
    data: &ColumnData,
    app: &AppState,
    diff: Option<&std::collections::HashMap<PathBuf, mo_diff::TreeStatus>>,
    renaming: Option<(&PathBuf, &Entity<InputState>)>,
    // 拖拽悬停认领的落点路径（§42）：命中的目录行高亮。
    drag_target: Option<&Path>,
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

    // 列头＝这一列是哪一级（目录最后一段），不是完整路径：210px 宽 + 11px 字号下
    // 完整路径要折成 3–4 行，而相邻列的前缀本来就重复；折行还会让各列头部高度不一、
    // 列内容的起始线参差。取名字的规则见 `crate::path_label`。
    col = col.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(HEAD_HEIGHT))
            .px(px(8.0))
            .border_b_1()
            .border_color(theme::separator())
            .text_size(px(11.0))
            .text_color(theme::muted())
            // `truncate` 兜住超长名字：列头必须保持单行（高度已钉成 `HEAD_HEIGHT`）。
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(text!(crate::path_label::last_segment(&data.path))),
            )
            .debug_selector(move || format!("mo-col-head-{pane}-{tab}-{index}")),
    );

    let mut body = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scrollbar();
    for (i, e) in data.entries.iter().take(MAX_PER_COLUMN).enumerate() {
        let selected = data.cursor == i;
        // 目录判据来自列快照（`e.kind`）：远程条目在本地磁盘上不存在，
        // `Path::is_dir()` 会把远程目录判成文件。
        let is_dir = matches!(e.kind, EntryKind::Directory);
        // 拖拽落点高亮（§42）：目录行 + 抬起**必定**传输（判据在认领侧）。
        let drop_lit = is_dir && drag_target == Some(e.path.as_path());
        let mut line = div()
            .id(format!("col-{pane}-{tab}-{index}-{i}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px(px(6.0))
            .bg(if drop_lit {
                theme::hover_bg()
            } else if selected {
                theme::selected_bg()
            } else if let Some(c) =
                crate::app::compare_tint(diff.and_then(|m| m.get(&e.path)).copied())
            {
                c
            } else {
                theme::surface()
            })
            .text_color(if selected {
                theme::selected_text()
            } else {
                theme::text()
            })
            // 测试用（release no-op）：定位「第 `index` 列第 `i` 行」（拖拽 / drop 投事件要坐标）。
            .debug_selector(move || format!("mo-col-row-{pane}-{tab}-{index}-{i}"));
        if !selected {
            line = line.hover(|s| s.bg(theme::hover_bg()));
        }
        // 行内改名进行中且正是这一行：名字格换输入框（末尾渲染处），点这一行
        // **外面**任何地方提交收场——`on_mouse_down_out` 盯整个行 div，点在本行
        // 内部（含输入框自己）不触发（与 `file_list.rs` :507 同款接线，§40 缺口 2）。
        let inline_input = renaming.filter(|(p, _)| **p == e.path).map(|(_, i)| i);
        if inline_input.is_some() {
            let entity_out = entity.clone();
            line = line.on_mouse_down_out(move |_ev, window, cx| {
                crate::dialogs::commit_inline_rename(&entity_out, window, cx);
            });
        }
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
                    v.open_entry(entry_path.clone(), is_dir, cx);
                    return;
                }
                // 单击：挪本列 cursor + 把 app 选择整替成这一行（§40 缺口 1）。
                v.column_row_clicked(pane, tab, index, i, entry_path.clone(), cx);
                if is_dir {
                    // 逐级下钻：选中目录就展开它的子列（替换掉更深层的列）。
                    v.load_column(cx, entry_path.clone(), Some(index), pane, tab);
                }
                cx.notify();
            });
        });
        // 拖拽：按下记源、抬起结算——与列表行 / 网格单元同款接线（§36~§38）。
        // 列视图条目没有选区概念，起点走单源版 `begin_drag_single`。
        let drag_down_entity = entity.clone();
        let drag_down_path = e.path.clone();
        line.interactivity()
            .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                drag_down_entity.update(cx, |v, cx| {
                    // 点别处 = 行内改名提交收场（点编辑行自身算挪光标，判据在
                    // `end_inline_rename_on_click` 里）；与 `file_list` / `grid` 同款。
                    v.end_inline_rename_on_click(Some(drag_down_path.as_path()), window, cx);
                    v.begin_drag_single(pane, tab, drag_down_path.clone(), ev, cx);
                });
            });
        // 拖拽悬停认领（§42）：只有目录行是有效落点（与 `file_list` 行级同款）。
        if is_dir {
            let hv_entity = entity.clone();
            let hv_path = e.path.clone();
            line.interactivity().on_mouse_move(move |ev, _window, cx| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                hv_entity.update(cx, |v, cx| {
                    v.note_drag_entry_hover(&hv_path, true, x, y, cx)
                });
            });
        }
        let drag_up_entity = entity.clone();
        let drag_up_path = e.path.clone();
        line.interactivity()
            .on_mouse_up(MouseButton::Left, move |ev, _window, cx| {
                // 按住 Alt（mac 上是 ⌥）拖 = 移动，否则复制。
                let alt = ev.modifiers.alt;
                drag_up_entity.update(cx, |v, cx| {
                    v.drop_on_entry(pane, tab, drag_up_path.clone(), is_dir, alt, cx);
                });
            });
        // 从系统拖文件进来：**只有目录行接得住**；非目录行不注册监听，让事件
        // 冒泡到窗格兜底（判据同 `file_list.rs` / `grid.rs`：`can_drop` 拒绝会吞事件）。
        if is_dir {
            let os_entity = entity.clone();
            let os_dest = e.path.clone();
            line = line
                .drag_over::<ExternalPaths>(|style, _, _window, _cx| style.bg(theme::hover_bg()));
            line.interactivity()
                .on_drop::<ExternalPaths>(move |paths, _window, cx| {
                    let paths = paths.paths().to_vec();
                    let dest = os_dest.clone();
                    os_entity.update(cx, |v, cx| {
                        v.drop_os_paths_on_entry(paths, pane, dest, cx);
                    });
                });
        }
        // 图标：与列表 / 网格 / 画廊**同一条链路**（`file_item::system_icon`）。
        //
        // 列视图的条目来自 `AppState::list_dir`（不走主目录模型），缩略图状态一律是
        // 初始态——所以这里只可能是「系统图标 or 内置 SVG」两种，不涉及缩略图。
        // 槽位取自 `listing::icon_slot` 那张表（列视图与列表行同为 16pt），取位图时
        // 也就只问小档（40px）。
        let slot = crate::listing::icon_slot(ViewMode::Columns);
        // 系统图标是后台备好的内存位图，`ImageSource::Render` 同步上屏（不走
        // `img(path)` 的异步读盘，那一格不会空着等）。
        let raster_source =
            crate::file_item::system_icon(Some(app), &e.path, e.kind.is_dir(), slot)
                .as_ref()
                .and_then(crate::bitmap::image_source);
        line = line.child(match raster_source {
            Some(src) => img(src)
                .w(px(slot))
                .h(px(slot))
                .flex_shrink_0()
                .into_any_element(),
            None => crate::icons::icon(
                crate::icons::icon_for_kind_and_name(e.kind, &e.name),
                slot,
                if selected {
                    theme::selected_text()
                } else {
                    theme::text()
                },
            )
            .into_any_element(),
        });
        // 名字：编辑态这一行换真输入框（与 `file_item::view` 的 `inline_input`
        // 同款拼法——外层不拉 `h_full`，让 Input 保持自然高、垂直居中）。
        line = line.child(match inline_input {
            Some(state) => div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .debug_selector(|| "mo-inline-rename".to_string())
                .child(
                    Input::new(state)
                        .appearance(false)
                        .bordered(false)
                        .small()
                        .text_size(px(13.0))
                        .p(px(0.0)),
                )
                .into_any_element(),
            None => div()
                .flex_1()
                .truncate()
                .child(text!(mo_core::display_name(&e.name).to_string()))
                .into_any_element(),
        });
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
