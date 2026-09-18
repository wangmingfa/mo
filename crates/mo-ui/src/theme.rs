//! 浅色主题调色板。
//!
//! 数值取自 gpui 的 `Colors::light()`（`gpui-pre-0.3.5/src/colors.rs`），
//! 保证与框架自带配色体系一致；等以后接 `Colors::for_appearance(window)`
//! 支持深色模式时，只需把这个模块换成从 window 外观取色。

use gpui_kit::Rgba;

/// 主文字色。
pub fn text() -> Rgba {
    gpui_kit::rgb(0x1d1d1f)
}

/// 次要文字（分隔标签、提示、未激活项）。
pub fn muted() -> Rgba {
    gpui_kit::rgb(0x86868b)
}

/// 常规面板底色（工具栏 / 侧边栏 / 状态栏）——比纯白略深一档，衬托内容区。
pub fn container() -> Rgba {
    gpui_kit::rgb(0xf6f6f7)
}

/// 内容区底色（文件列表）。
pub fn surface() -> Rgba {
    gpui_kit::rgb(0xffffff)
}

/// 边框 / 分隔线。
pub fn separator() -> Rgba {
    gpui_kit::rgb(0xe8e8ea)
}

/// 选中行底色——用户指定的蓝（RGB 41,99,217）。
/// 配合 [`selected_text`] 白字使用，保证选中高亮下的对比度。
pub fn selected_bg() -> Rgba {
    gpui_kit::rgb(0x2963d9)
}

/// 选中行上的文字色（饱和蓝底用白字，保证对比度，对齐 Finder）。
pub fn selected_text() -> Rgba {
    gpui_kit::rgb(0xffffff)
}

/// 悬停行 / 按钮底色（中性灰，极简风：悬停不带色相）。
pub fn hover_bg() -> Rgba {
    gpui_kit::rgb(0xf0f0f1)
}

/// 列表斑马纹（奇数行底色）——Finder 列表视图的交替浅灰，比 surface 略深一档。
pub fn zebra() -> Rgba {
    gpui_kit::rgb(0xf7f7f8)
}

/// 强调色（激活项 / 焦点边框 / 强调文字）——用户指定的中性灰，比 199 淡 10%（RGB 205,205,205）。
/// 文件列表的**选中高亮**不在其列，仍走 [`selected_bg`] 的蓝。
pub fn accent() -> Rgba {
    gpui_kit::rgb(0xcdcdcd)
}
