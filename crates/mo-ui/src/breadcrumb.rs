use std::path::PathBuf;

use gpui_kit::*;

/// 地址栏 / 面包屑：展示当前目录路径。
pub fn render(path: &Option<PathBuf>) -> impl IntoElement {
    let label = path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "—".to_string());
    div()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .p(px(6.0))
        .bg(gpui_kit::white())
        .child(text!(format!("📍 {}", label)))
}
