//! 快速预览的**独立窗口**。
//!
//! 预览不再挤在主窗口的模态卡片里：macOS 的 Quick Look 本来就是独立浮窗，
//! 独立窗口还能边看预览边操作文件列表。窗口由 [`crate::RootView::show_preview`]
//! 懒开——已开着就换内容并置前，不重复开第二个。
//!
//! 窗口**标题就是文件名**，主体**只有预览内容**——不显示大小、类型、路径这些
//! 元信息：那些在主窗口里本来就看得见，重复一遍只会挤掉正文。
//!
//! 键盘：
//! * Esc / Space 关闭本窗口（与主窗口的模态习惯一致）；
//! * ← ↑ / → ↓ 在目录里**逐个切换预览对象**——焦点仍走 app 侧的选择模型，
//!   所以主列表的高亮与滚动会跟着一起走（Finder 的 Quick Look 行为）。

use std::path::PathBuf;
use std::time::Duration;

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

    /// 换预览内容（窗口复用 / 方向键翻页时）。
    ///
    /// 标题在这里一起改：内容换了标题没换，窗口管理器与 ⌘Tab 里就会指错文件。
    pub fn set_preview(&mut self, preview: Preview, window: &mut Window, cx: &mut Context<Self>) {
        let title = preview.title.clone();
        self.preview = preview;
        window.set_window_title(&title);
        cx.notify();
    }

    /// 降采样副本到了：把窗口里的「载入预览…」占位换成真图（窗口已经开着）。
    ///
    /// 与 `set_preview` 的区别：那只换图片路径，标题与文本都不动——文件名没变，
    /// 变的只是「这张图现在有合适的尺寸可显示了」。翻页时同理：`set_preview` 先把
    /// 窗口切到新文件（图先空着、显示占位），等新那一张的副本到了再由这里补上。
    /// 是否还属于当前预览由代际校验决定，见 [`crate::RootView::set_preview_image`]；
    /// 这里只管渲染。
    pub fn set_image(&mut self, image: PathBuf, cx: &mut Context<Self>) {
        self.preview.image = Some(image);
        cx.notify();
    }
}

impl Render for PreviewWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.preview.clone();
        let text = p.text.clone().unwrap_or_default();

        // 主体只有内容本身：图片加载一个降采样后的副本（见
        // `AppState::preview_image_scaled`），object_fit 默认 Contain 适配容器。
        // 图片容器 flex_1 + min_h_0：没有确定高度时 Contain 无从适配。
        // 文本自己带内边距，图片铺满。
        //
        // 加载 / 失败都给占位：gpui 要过 LOADING_DELAY(200ms) 才肯显占位，
        // 期间是一片空白；解码失败（损坏 / 不支持的格式）也不能让窗口空着。
        let body: AnyElement = match p.kind {
            PreviewKind::Image => {
                let inner: AnyElement = match p.image.clone() {
                    Some(path) => img(path)
                        .size_full()
                        .with_loading(|| centered_note("载入预览…"))
                        .with_fallback(|| centered_note("无法解码这张图片"))
                        .into_any_element(),
                    // 降采样副本还在后台生成（见 `RootView::open_quick_look`）：
                    // 窗口先开、内容后到，别让窗口空着。
                    None => centered_note("载入预览…"),
                };
                // 入场动画：内容从 8 成大小长到满 + 淡入，对应访达快速预览的展开感。
                // gpui 这版没有 element 级 `scale`（只有 `opacity`），所以「缩放」用
                // **相对尺寸**驱动——图片是 Contain 适配容器，容器从小变大，
                // 观感就是图片从中心长出来。
                //
                // `with_animation` 按 `ElementId` 记住进度、oneshot 只播一次
                // （换内容/翻页不会重播），并且自动尊重系统「减弱动态效果」。
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(inner)
                            .with_animation(
                                ElementId::Name("preview.enter".into()),
                                Animation::new(Duration::from_millis(200))
                                    .with_easing(ease_out_quint()),
                                |el, t| {
                                    el.w(relative(0.86 + 0.14 * t))
                                        .h(relative(0.86 + 0.14 * t))
                                        .opacity(t)
                                },
                            ),
                    )
                    .into_any_element()
            }
            _ => div()
                .flex_1()
                .min_h_0()
                .p(px(12.0))
                .overflow_y_scrollbar()
                .child(text!(text))
                .into_any_element(),
        };

        let mut root = div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
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

/// 预览里的居中提示（载入中 / 解码失败）。占满容器，免得提示缩在角落。
fn centered_note(msg: &'static str) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(text!(msg.to_string()))
        .into_any_element()
}
