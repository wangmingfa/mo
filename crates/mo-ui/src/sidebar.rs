use gpui_kit::*;

/// 侧边栏：快捷访问 / 书签（第一版为静态占位，后续接 `NavigationState.bookmarks`）。
pub fn render() -> impl IntoElement {
    div()
        .flex_col()
        .w(px(200.0))
        .p(px(8.0))
        .gap(px(4.0))
        .bg(gpui_kit::white())
        .child(text!("快捷访问"))
        .child(text!("🏠 Home"))
        .child(text!("📥 Downloads"))
        .child(text!("🖥 Desktop"))
}
