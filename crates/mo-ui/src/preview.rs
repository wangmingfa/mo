//! 快速预览的**独立窗口**。
//!
//! 预览不再挤在主窗口的模态卡片里：macOS 的 Quick Look 本来就是独立浮窗，
//! 独立窗口还能边看预览边操作文件列表。窗口由 [`crate::RootView::show_preview`]
//! 懒开——已开着就换内容并置前，不重复开第二个。
//!
//! 键盘：
//! * Esc / Space 关闭本窗口（与主窗口的模态习惯一致）；
//! * ← ↑ / → ↓ 在目录里**逐个切换预览对象**——焦点仍走 app 侧的选择模型，
//!   所以主列表的高亮与滚动会跟着一起走（Finder 的 Quick Look 行为）。

use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_preview::{Preview, PreviewKind};

use crate::theme;
use crate::RootView;

/// 预览窗口的根视图。
pub struct PreviewWindow {
    preview: Preview,
    focus: FocusHandle,
    /// 主窗口视图：方向键要通过它移动列表焦点并换预览内容。
    root: Entity<RootView>,
}

impl Focusable for PreviewWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PreviewWindow {
    pub fn new(preview: Preview, root: Entity<RootView>, cx: &mut Context<Self>) -> Self {
        Self {
            preview,
            focus: cx.focus_handle(),
            root,
        }
    }

    /// 换预览内容（窗口复用时）。
    pub fn set_preview(&mut self, preview: Preview, cx: &mut Context<Self>) {
        self.preview = preview;
        cx.notify();
    }
}

impl Render for PreviewWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.preview.clone();
        let text = p.text.clone().unwrap_or_default();

        // 主体：图片直接加载原图（gpui 解码，object_fit 默认 Contain 适配
        // 容器），其余是纯文本。图片容器 flex_1 + min_h_0：没有确定高度时
        // Contain 无从适配。
        let body: Div = match p.kind {
            PreviewKind::Image if p.image.is_some() => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(6.0))
                .child(text!(format!("🖼 {}（{} 字节）", p.title, p.size)))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .items_center()
                        .justify_center()
                        .child(img(p.image.clone().unwrap()).size_full()),
                ),
            _ => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(4.0))
                .child(text!(format!("{} · {} 字节", p.title, p.size)))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(text!(text)),
                ),
        };

        let mut root = div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .p(px(12.0))
            .bg(theme::surface())
            .text_color(theme::text())
            .text_size(px(13.0))
            .child(body);

        // Esc / Space 关闭；方向键在目录里逐个切换预览对象。
        let step_root = self.root.clone();
        root.interactivity()
            .on_key_down(move |ev, window, cx| match ev.keystroke.key.as_str() {
                "escape" | "space" => window.remove_window(),
                "up" | "arrowup" | "left" | "arrowleft" => {
                    step_root.update(cx, |v, cx| v.preview_step(-1, cx));
                }
                "down" | "arrowdown" | "right" | "arrowright" => {
                    step_root.update(cx, |v, cx| v.preview_step(1, cx));
                }
                _ => {}
            });

        // 让按键落到本窗口视图上。
        if !self.focus.is_focused(window) {
            cx.focus_self(window);
        }

        root
    }
}
