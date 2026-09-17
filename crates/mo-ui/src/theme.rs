//! 浅色主题调色板。
//!
//! 数值取自 gpui 的 `Colors::light()`（`gpui-pre-0.3.5/src/colors.rs`），
//! 保证与框架自带配色体系一致；等以后接 `Colors::for_appearance(window)`
//! 支持深色模式时，只需把这个模块换成从 window 外观取色。

use gpui_kit::Rgba;

/// 主文字色。
pub fn text() -> Rgba {
    gpui_kit::rgb(0x252525)
}

/// 次要文字（分隔标签、提示）。
pub fn muted() -> Rgba {
    gpui_kit::rgb(0x8a8a8a)
}

/// 常规面板底色（工具栏 / 侧边栏 / 状态栏）。
pub fn container() -> Rgba {
    gpui_kit::rgb(0xf4f5f5)
}

/// 内容区底色（文件列表）。
pub fn surface() -> Rgba {
    gpui_kit::rgb(0xffffff)
}

/// 边框 / 分隔线。
pub fn separator() -> Rgba {
    gpui_kit::rgb(0xe6e6e6)
}

/// 选中行底色（浅蓝）。
pub fn selected_bg() -> Rgba {
    gpui_kit::rgb(0xdce7fb)
}

/// 悬停行 / 按钮底色（更浅的蓝灰）。
pub fn hover_bg() -> Rgba {
    gpui_kit::rgb(0xf0f4fa)
}

/// 强调色（当前聚焦侧边栏项等）。
pub fn accent() -> Rgba {
    gpui_kit::rgb(0x2a63d9)
}
