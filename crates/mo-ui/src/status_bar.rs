use std::path::PathBuf;

use gpui_kit::*;

use crate::RootView;

/// 状态栏左侧那一串数字。
///
/// 打包成结构而不是九个参数：参数个数一多，调用点就把「已选」和「已索引」
/// 传反过还看不出来（clippy 的 7 参上限也是这么来的）。
pub struct Stats {
    pub count: usize,
    pub selection_count: usize,
    pub indexed: usize,
    pub can_undo: bool,
    pub can_redo: bool,
    /// 暂存区里的条数：**始终**显示入口（哪怕为 0）——暂存区没有别的常驻入口，
    /// 藏起来就没人找得到；有内容时上底色，一眼看得出「里面还有东西」。
    pub staged: usize,
}

/// 状态栏：左侧条目统计，右侧暂存区入口 + 快捷键提示（当前路径在地址栏，不再重复）。
pub fn render(
    stats: Stats,
    _path: &Option<PathBuf>,
    query: &str,
    entity: &Entity<RootView>,
) -> impl IntoElement {
    let Stats {
        count,
        selection_count,
        indexed,
        can_undo,
        can_redo,
        staged,
    } = stats;
    let mut left = if query.is_empty() {
        format!("{count} 项")
    } else {
        // `query` 现在是「输入即定位」的前缀缓冲（不是过滤），措辞随之改。
        format!("{count} 项 · 定位「{query}」")
    };
    if selection_count > 0 {
        left.push_str(&format!(" · 已选 {selection_count}"));
    }
    if can_undo || can_redo {
        left.push_str(" · 可撤销");
    }

    let mut right = format!("已索引 {indexed}");
    if can_undo || can_redo {
        right.push_str(&format!(" · {} 撤销", crate::keys::hint("edit.undo")));
        if can_redo {
            right.push_str(&format!(" · {} 重做", crate::keys::hint("edit.redo")));
        }
    }
    right.push_str(&format!(
        " · {} 命令 · Space 预览",
        crate::keys::hint("palette.open")
    ));

    // 暂存区入口：点了开合底部抽屉。
    let toggle = entity.clone();
    let mut chip = div()
        // ⚠️ 必须有元素 ID：无 ID 的裸 div 拿不到 element_state，on_click 永远不触发。
        .id("statusbar-staging")
        .flex()
        .flex_row()
        .items_center()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .text_size(px(11.0))
        .child(text!(format!("暂存区 {staged}")))
        .debug_selector(|| "mo-statusbar-staging".to_string());
    if staged > 0 {
        chip = chip
            .bg(crate::theme::accent())
            .text_color(crate::theme::text());
    } else {
        chip = chip
            .text_color(crate::theme::muted())
            .hover(|s| s.bg(crate::theme::hover_bg()));
    }
    chip.interactivity().on_click(move |_, _window, cx| {
        toggle.update(cx, |v, cx| v.toggle_staging(cx));
    });

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
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(chip.test_support())
                .child(text!(right)),
        )
}
