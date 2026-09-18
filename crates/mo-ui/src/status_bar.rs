use std::path::PathBuf;

use gpui_kit::*;

/// 状态栏：左侧条目统计，右侧快捷键提示（当前路径在地址栏，不再重复）。
pub fn render(
    count: usize,
    _path: &Option<PathBuf>,
    query: &str,
    selection_count: usize,
    indexed: usize,
    can_undo: bool,
    can_redo: bool,
) -> impl IntoElement {
    let mut left = if query.is_empty() {
        format!("{count} 项")
    } else {
        format!("{count} 项 · 匹配「{query}」")
    };
    if selection_count > 0 {
        left.push_str(&format!(" · 已选 {selection_count}"));
    }
    if can_undo || can_redo {
        left.push_str(" · 可撤销");
    }

    let mut right = format!("已索引 {indexed}");
    if can_undo || can_redo {
        right.push_str(" · ⌘Z 撤销");
        if can_redo {
            right.push_str(" · ⇧⌘Z 重做");
        }
    }
    right.push_str(" · ⌘⇧P 命令 · Space 预览");

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px(px(10.0))
        .h(px(26.0))
        .flex_shrink_0()
        .bg(crate::theme::container())
        .border_t_1()
        .border_color(crate::theme::separator())
        .text_color(crate::theme::muted())
        // 状态栏只有 26px 高，显式压到 11px，避免落到默认字号（约 14px）显得过大。
        .text_size(px(11.0))
        // 测试用（release no-op）：tests/layout.rs 断言状态栏贴着窗口底部
        .debug_selector(|| "mo-statusbar".to_string())
        .child(text!(left))
        .child(text!(right))
}
