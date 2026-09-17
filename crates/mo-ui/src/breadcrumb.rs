use std::path::PathBuf;

use gpui_kit::*;

/// 地址栏 / 面包屑：展示当前目录路径。
pub fn render(path: &Option<PathBuf>) -> impl IntoElement {
    let label = path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "—".to_string());
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .px(px(8.0))
        .py(px(6.0))
        .bg(crate::theme::surface())
        .border_b_1()
        .border_color(crate::theme::separator())
        .text_color(crate::theme::muted())
        .child(text!(format!("📍 {}", label)))
}
