//! list_view：列表类面板共享的**表现层**（表头 / 行底色 / 占位行）。
//!
//! 背景（用户四问，2026-09-24）：回收站没有表头、切视图没反应、点空白不清选
//! ——三个问题同一个根因：文件列表的能力全部长在浏览管线里（`file_list` /
//! `grid`，深度耦合 pane/tab 寻址、窗口快照、异步补窗、缩略图泵），每加一个
//! 列表类面板就得手写一遍、丢一遍能力。本模块把「**长什么样**」收成一份实现：
//!
//! * [`Column`] + [`header_row`]：表头（与 `file_list::header` 同视觉语言）；
//! * [`row_background`]：行底色规则（选中蓝 > 斑马纹 > 表面色）——数据行、
//!   占位行、回收站行三处必须逐字一致，否则斑马纹对不上缝；
//! * [`fill_row`]：「斑马纹铺满一屏」的占位行。
//!
//! 边界：虚拟化补窗、点击排序 / 拖列宽 / 拖列序、缩略图泵**仍归浏览管线**——
//! 那些行为深度绑定 `RootView` 的面板状态，等出现第三个消费者再把它们参数化
//! 收编，避免为一个场景过度抽象。

use gpui_kit::*;
use mo_core::SortKey;

use crate::theme;

/// 表头行高（与 `file_list::header` 一致）。
pub(crate) const HEADER_H: f32 = 26.0;
/// 数据行 / 占位行行高（与 `file_list` / 回收站一致，虚拟化列表的行高必须恒定）。
pub(crate) const ROW_H: f32 = 24.0;

/// 一列的定义。`width == 0.0` 表示弹性列（吃掉剩余空间，通常是名称列）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Column {
    /// 元素 / 测试定位键。
    pub key: &'static str,
    /// 表头文案（数据行同样按这列的宽度 / 对齐渲染，两侧必须同源）。
    pub label: &'static str,
    /// 固定宽度；0 = 弹性。
    pub width: f32,
    /// 数值 / 日期 / 类别列右对齐，名称列左对齐（与文件列表一致）。
    pub right_aligned: bool,
}

impl Column {
    /// 弹性列（名称）。
    pub(crate) fn flex(key: &'static str, label: &'static str) -> Self {
        Column {
            key,
            label,
            width: 0.0,
            right_aligned: false,
        }
    }

    /// 固定宽、右对齐列（日期 / 大小 / 种类）。
    pub(crate) fn fixed_right(key: &'static str, label: &'static str, width: f32) -> Self {
        Column {
            key,
            label,
            width,
            right_aligned: true,
        }
    }
}

/// 表头行：`file_list::header` 的**静态版**——同一套视觉（26px、container 底、
/// 下分隔线、11px 弱化字、水平 16px 内衬与数据行的 12+4 对齐），但不带点击排序 /
/// 拖列宽 / 拖列序（那些交互耦合 `RootView` 的表头状态机，见模块注释的边界）。
///
/// `id_prefix` 用来区分同一窗口里的多份表头（a11y NodeId 不能撞）。
pub(crate) fn header_row(id_prefix: &str, cols: &[Column]) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(HEADER_H))
        .px(px(16.0))
        .bg(theme::container())
        .border_b_1()
        .border_color(theme::separator())
        .text_size(px(11.0))
        .text_color(theme::muted())
        .debug_selector(|| format!("mo-{id_prefix}-header"));
    for c in cols {
        let mut cell = div().flex().flex_row().items_center().h_full().min_w_0();
        cell = if c.width > 0.0 {
            cell.w(px(c.width)).flex_shrink_0()
        } else {
            cell.flex_1()
        };
        cell = if c.right_aligned {
            cell.justify_end()
        } else {
            cell
        };
        row = row.child(cell.child(text!(c.label.to_string())));
    }
    row
}

/// 行底色规则：选中蓝底 > 斑马纹（奇数行）> 表面色。
///
/// ⚠️ `zebra` 开关是给文件列表的（用户可关）；回收站恒开。占位行必须与数据行
/// 用同一个函数，否则加载前后底色差半格，看起来整块列表在抖。
pub(crate) fn row_background(selected: bool, index: usize, zebra: bool) -> Rgba {
    if selected {
        theme::selected_bg()
    } else if zebra && index % 2 == 1 {
        theme::zebra()
    } else {
        theme::surface()
    }
}

/// 占位行：真实行数不够一屏时补足的「只有底色、没有任何内容」的行
/// （用户报过：列表只有两条时下面一片白，不像同一个应用）。
///
/// * 底色走 [`row_background`]，与数据行逐字一致；
/// * 行 ID 不能省：多条占位行共享无 ID 路径会撞 a11y NodeId
///   （`file_list` 占位行的同一条注释）；
/// * `selector` 是测试定位键（如 `mo-trash-ph-12`）。
pub(crate) fn fill_row(id: String, selector: String, index: usize) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .h(px(ROW_H))
        .px(px(4.0))
        .bg(row_background(false, index, true))
        .debug_selector(move || selector.clone())
}

/// 日期 / 大小 / 种类列的默认宽度。名称列弹性，不在此列。
///
/// 与 `ColId::default_width` 同值：同一套列语言，回收站与文件列表的表格观感一致。
pub(crate) fn default_col_width(key: SortKey) -> f32 {
    match key {
        SortKey::Name => 0.0,
        SortKey::Modified => 150.0,
        SortKey::Size => 80.0,
        SortKey::Kind => 100.0,
    }
}

#[cfg(test)]
mod tests {
    use super::{row_background, Column};
    use mo_core::SortKey;

    // 注意：不能 `use super::*`——模块顶部的 `use gpui_kit::*` 会把 gpui 的
    // `test` 模块带进来，遮蔽内置 `#[test]`（见 file_item tests 的同一条注释）。

    #[test]
    fn selected_rows_win_over_zebra() {
        // 选中行永远蓝底：奇数行（本该斑马）也一样。
        assert_eq!(
            row_background(true, 1, true),
            row_background(true, 0, false)
        );
    }

    #[test]
    fn zebra_applies_only_to_odd_rows_when_enabled() {
        assert_eq!(
            row_background(false, 1, true),
            row_background(false, 3, true)
        );
        assert_ne!(
            row_background(false, 0, true),
            row_background(false, 1, true)
        );
        // 开关关掉（或占位行之外的用法）恒为表面色。
        assert_eq!(
            row_background(false, 1, false),
            row_background(false, 0, false)
        );
    }

    #[test]
    fn column_helpers_match_file_list_defaults() {
        let name = Column::flex("name", "名称");
        assert_eq!(name.width, 0.0);
        assert!(!name.right_aligned);
        let size = Column::fixed_right("size", "大小", super::default_col_width(SortKey::Size));
        assert_eq!(size.width, 80.0);
        assert!(size.right_aligned);
    }
}
