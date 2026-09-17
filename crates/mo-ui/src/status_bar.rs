use std::path::PathBuf;

use gpui_kit::*;

/// 状态栏：显示条目数量、过滤态、选择数、索引数与当前目录路径。
pub fn render(
    count: usize,
    path: &Option<PathBuf>,
    query: &str,
    selection_count: usize,
    indexed: usize,
    can_undo: bool,
    can_redo: bool,
) -> impl IntoElement {
    let label = path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut summary = if query.is_empty() {
        format!("{} 项 · {}", count, label)
    } else {
        format!("{} / 匹配「{}」· {}", count, query, label)
    };
    if selection_count > 0 {
        summary.push_str(&format!(" · 已选 {} 项", selection_count));
    }
    summary.push_str(&format!(" · 已索引 {} 项", indexed));
    if can_undo || can_redo {
        summary.push_str(&format!(
            " · {}⌘Z 撤销{}",
            if can_undo { "" } else { "（无）" },
            if can_redo { " · ⇧⌘Z 重做" } else { "" }
        ));
    }
    summary.push_str(" · ⌘⇧P 命令 · ⌘F 搜索 · Space 预览");

    div()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .p(px(6.0))
        .bg(gpui_kit::white())
        .child(text!(summary))
}
