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

/// 记号取色（语法着色与 Markdown 渲染共用一张表）。
fn token_color(kind: mo_preview::TokenKind) -> gpui_kit::Rgba {
    use mo_preview::TokenKind as K;
    match kind {
        K::Keyword => theme::syntax_keyword(),
        K::String | K::InlineCode => theme::syntax_string(),
        K::Comment => theme::syntax_comment(),
        K::Number => theme::syntax_number(),
        K::Link | K::ListMarker => theme::accent(),
        K::Quote | K::Rule => theme::muted(),
        // 标题用强调色：正文是黑的，标题不换色只放大仍然会混在段落里。
        K::Heading(_) | K::Bold => theme::accent(),
        K::Punct | K::Plain => theme::text(),
    }
}

/// 记号字号（正文 13px；标题按级别放大，一级最大）。
fn token_size(kind: mo_preview::TokenKind) -> f32 {
    match kind {
        mo_preview::TokenKind::Heading(level) => match level {
            1 => 22.0,
            2 => 19.0,
            3 => 17.0,
            4 => 15.0,
            _ => 14.0,
        },
        _ => 13.0,
    }
}

/// 预览窗口的根视图。
pub struct PreviewWindow {
    preview: Preview,
    /// 文本预览的着色结果（**按行分组的记号**），换内容时算一次。
    ///
    /// 不在这里每帧重算：渲染每帧都跑，而着色是逐字符扫描——一个 128KB 的文件
    /// 每帧扫一遍等于把浮窗钉死。空 = 这段文本没着色（纯文本或超上限）。
    highlighted: Vec<Vec<mo_preview::Token>>,
    focus: FocusHandle,
    /// 主窗口视图：方向键要通过它移动列表焦点并换预览内容。
    root: Entity<RootView>,
}

/// 给一段预览文本做结构化着色（代码 / JSON / Markdown）。
///
/// 纯文本**不动**：它没有约定的记号，随手一个 `#` 或引号都会被当成结构，
/// 涂得五颜六色反而更难读——那类的正文就是给人读的散文。
fn highlight_for(kind: PreviewKind, text: &str) -> Vec<Vec<mo_preview::Token>> {
    match kind {
        PreviewKind::Code | PreviewKind::Json => mo_preview::highlight(text).unwrap_or_default(),
        PreviewKind::Markdown => mo_preview::markdown(text).unwrap_or_default(),
        _ => Vec::new(),
    }
}

impl Focusable for PreviewWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PreviewWindow {
    pub fn new(preview: Preview, root: Entity<RootView>, cx: &mut Context<Self>) -> Self {
        let highlighted = highlight_for(preview.kind, preview.text.as_deref().unwrap_or_default());
        Self {
            preview,
            highlighted,
            focus: cx.focus_handle(),
            root,
        }
    }

    /// 换预览内容（窗口复用 / 方向键翻页时）。
    ///
    /// 标题在这里一起改：内容换了标题没换，窗口管理器与 ⌘Tab 里就会指错文件。
    pub fn set_preview(&mut self, preview: Preview, window: &mut Window, cx: &mut Context<Self>) {
        let title = preview.title.clone();
        self.highlighted = highlight_for(preview.kind, preview.text.as_deref().unwrap_or_default());
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

    /// 换掉窗口里的文本（PDF 首页渲染不出来时用：把「正在渲染…」换成一句解释）。
    ///
    /// 只动文本、不动图片与标题——与 `set_image` 一样是「补内容」而不是「换对象」，
    /// 所以不需要也不应该动代际（`preview_seq`）。
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.preview.text = Some(text);
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
            // PDF 与图片共用这条分支：首页渲染出来之后，它和一张图片没有区别。
            PreviewKind::Image | PreviewKind::Pdf => {
                let inner: AnyElement = match p.image.clone() {
                    Some(path) => img(path)
                        .size_full()
                        .with_loading(|| centered_note("载入预览…"))
                        .with_fallback(|| centered_note("无法解码这张图片"))
                        .into_any_element(),
                    // 两种情况都「图还没到」，但给用户的说法不同：
                    // * PDF：首页还在渲染（mo-preview 给的占位文案，渲染失败时后台会
                    //   把它换成一句解释）；
                    // * 图片：降采样副本还在后台生成（见 `RootView::open_quick_look`）。
                    // 窗口先开、内容后到，别让窗口空着。
                    None => centered_note(if p.kind == PreviewKind::Pdf {
                        &text
                    } else {
                        "载入预览…"
                    }),
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
            // 代码 / JSON：按行渲染着色后的记号（行内多段不同色）。
            //
            // 一行一个 `flex_row`：行间仍是一个个块级盒子（滚动、换行照旧），
            // 行内才是多段同排。空行给一个空格撑住行高——空 `text!` 没有行盒，
            // 空行会直接塌掉，代码看起来会挤在一起。
            _ if !self.highlighted.is_empty() => div()
                .flex_1()
                .min_h_0()
                .p(px(12.0))
                .overflow_y_scrollbar()
                .children(self.highlighted.iter().map(|line| {
                    if line.iter().all(|t| t.text.is_empty()) {
                        return div().child(text!(" ".to_string())).into_any_element();
                    }
                    div()
                        .flex_row()
                        .children(line.iter().map(|t| {
                            // 色挂在包一层的小 div 上：`text!` 返回的是 `Text`，
                            // 它本身没有 `text_color`（那套样式方法在元素上）。
                            div()
                                .text_color(token_color(t.kind))
                                .text_size(px(token_size(t.kind)))
                                .child(text!(t.text.clone()))
                                .into_any_element()
                        }))
                        .into_any_element()
                }))
                .into_any_element(),
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
// `&str` 而不是 `&'static str`：PDF 的占位文案是运行时拼的（渲染失败时还会再换一句）。
fn centered_note(msg: &str) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(text!(msg.to_string()))
        .into_any_element()
}
