use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_app::AppState;
use mo_operations::{OperationHandle, OperationStatus};

use crate::{theme, RootView};

/// 左下角统一任务区（状态栏上方，仿 GNOME Files / Nautilus）。
///
/// 所有耗时操作（删除 / 复制 / 移动 / 远程传输……）都汇进 `OperationManager`
/// 的同一份快照，在这里统一呈现。**折叠**＝当前任务一行 + 底部通栏细进度条；
/// **展开**＝原地变成一张卡片：标题行（任务数 + 收起）+ 全部任务列表——
/// 不是悬空的浮层，也没有第二份重复内容。点标题 / 点面板外 / Esc 收起。
///
/// 数据来自 `OperationManager::snapshot()`（经 `tab.ops` 快照），由事件总线驱动刷新。
/// 面板整体绝对定位，**不占布局**：有没有任务，文件区高度都不变。
///
/// `open` 由调用方（`RootView`）传入：渲染函数拿不到 cx，读不了实体状态。
pub fn render_overlay(
    ops: &[OperationHandle],
    app: &AppState,
    open: bool,
    entity: &Entity<RootView>,
) -> impl IntoElement {
    if ops.is_empty() {
        // 与正常分支同型（Stateful<Div>），`impl IntoElement` 两个分支必须同型。
        return div().id("mo-ops-empty");
    }

    // 外层 wrapper 挂 `on_mouse_down_out`：点面板外收起。展开 / 折叠是同一张
    // 卡片，不存在「两个元素必须同包一个 wrapper」的问题了。
    let out = entity.clone();
    let mut wrapper = div()
        .id("mo-ops-overlay")
        .absolute()
        // 状态栏 26px，再留 6px 空隙；贴左下角。
        .left(px(10.0))
        .bottom(px(32.0))
        .occlude()
        .debug_selector(|| "mo-ops-overlay".to_string());
    wrapper
        .interactivity()
        .on_mouse_down_out(move |_ev, _window, cx| {
            out.update(cx, |v, cx| v.close_ops_popover(cx));
        });

    // 面板本体：折叠 300px / 展开 380px，同一张卡片原地长高。
    let mut panel = div()
        .id("mo-ops-panel")
        .w(px(if open { 380.0 } else { 300.0 }))
        .flex()
        .flex_col()
        .rounded(px(8.0))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::separator())
        .shadow_lg()
        .overflow_hidden()
        .debug_selector(|| "mo-ops-panel".to_string());

    if open {
        // ── 展开态：标题行（点击收起）+ 全部任务列表 ─────────────────────
        let running = ops
            .iter()
            .filter(|op| {
                matches!(
                    op.status,
                    OperationStatus::Pending | OperationStatus::Running
                )
            })
            .count();
        let summary = if running > 0 {
            format!("任务（{}）· {} 个进行中", ops.len(), running)
        } else {
            format!("任务（{}）", ops.len())
        };

        // ⚠️ 必须有元素 ID：无 ID 的裸 div 拿不到 element_state，on_click 永远不触发。
        let mut header = div()
            .id("mo-ops-badge")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(7.0))
            .text_size(px(11.0))
            .text_color(theme::muted())
            .hover(|s| s.bg(theme::hover_bg()))
            .debug_selector(|| "mo-ops-badge".to_string())
            .child(div().flex_1().min_w_0().truncate().child(text!(summary)))
            .child(text!("收起 ▾"));
        let toggle = entity.clone();
        header.interactivity().on_click(move |_, _window, cx| {
            toggle.update(cx, |v, cx| v.toggle_ops_popover(cx));
        });
        // `.test_support()`：headless 的 `click("mo-ops-badge")` 只认**被观察**的元素；
        // 非 test 构建（不带 test-support feature）这是恒等包装，不影响产物。
        panel = panel.child(header.test_support()).child(
            div()
                .id("mo-ops-popover")
                .max_h(px(300.0))
                .flex()
                .flex_col()
                .overflow_y_scrollbar()
                .border_t_1()
                .border_color(theme::separator())
                .debug_selector(|| "mo-ops-popover".to_string())
                .children(ops.iter().map(|op| render_op_row(op, app, entity))),
        );
    } else {
        // ── 折叠态：只显示一个任务 + 底部通栏细进度条，整行点击展开 ──────
        // 优先进行中 / 排队中的第一条；全部结束时显示最后一条（刚完成的那条，
        // 用户还来得及看一眼结果）。
        let current = ops
            .iter()
            .find(|op| {
                matches!(
                    op.status,
                    OperationStatus::Pending | OperationStatus::Running
                )
            })
            .or_else(|| ops.last());
        if let Some(op) = current {
            let ratio = ratio_of(op);
            let mut badge = div()
                .id("mo-ops-badge")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px(px(10.0))
                .pt(px(6.0))
                .pb(px(4.0))
                .text_size(px(11.0))
                .text_color(theme::text())
                .hover(|s| s.bg(theme::hover_bg()))
                .debug_selector(|| "mo-ops-badge".to_string())
                // 状态圆点：一眼看出这单是跑着还是完了。
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(7.0))
                        .rounded_full()
                        .bg(status_color(op)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(text!(op.describe.clone())),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme::muted())
                        .child(text!(status_tail(op, ratio))),
                );
            let toggle = entity.clone();
            badge.interactivity().on_click(move |_, _window, cx| {
                toggle.update(cx, |v, cx| v.toggle_ops_popover(cx));
            });
            panel = panel
                .child(badge.test_support())
                // 通栏细进度条：不占行高也能看出「在动」。
                .child(
                    div()
                        .h(px(3.0))
                        .w_full()
                        .bg(theme::hover_bg())
                        .child(div().h(px(3.0)).w(relative(ratio)).bg(theme::selected_bg())),
                );
        }
    }

    wrapper.child(panel)
}

/// 展开列表里的一行：两行式——上行「状态点 + 描述 + 动作」，下行「进度条 + 尾标」。
fn render_op_row(
    op: &OperationHandle,
    app: &AppState,
    entity: &Entity<RootView>,
) -> impl IntoElement {
    let ratio = ratio_of(op);
    let running = matches!(
        op.status,
        OperationStatus::Pending | OperationStatus::Running
    );

    // 行尾动作：进行中 / 排队 → 取消；已结束 → ✕ 移除（句柄不摘会一直堆着）。
    let app_click = app.clone();
    let entity_click = entity.clone();
    let id = op.id;
    let mut action = div()
        .id(("mo-ops-action", op.id))
        .flex_shrink_0()
        .px(px(4.0))
        .rounded(px(4.0))
        .text_size(px(11.0))
        .text_color(theme::muted())
        .hover(|s| s.bg(theme::hover_bg()))
        .child(text!(if running { "取消" } else { "✕" }.to_string()));
    action.interactivity().on_click(move |_, _window, cx| {
        // ⚠️ 行本身没有 on_click，这里不需要 stop_propagation；
        // 若将来给行加了点击语义，记得先拦冒泡。
        let app = app_click.clone();
        let entity = entity_click.clone();
        cx.spawn(async move |cx| {
            if running {
                app.cancel_operation(id).await;
            } else {
                app.dismiss_operation(id).await;
            }
            entity.update(cx, |_, cx| cx.notify());
        })
        .detach();
    });

    div()
        .id(("mo-ops-row", op.id))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .px(px(12.0))
        .py(px(7.0))
        .hover(|s| s.bg(theme::hover_bg()))
        // 上行：状态点 + 描述（截断）+ 动作。
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(7.0))
                        .rounded_full()
                        .bg(status_color(op)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme::text())
                        .child(text!(op.describe.clone())),
                )
                .child(action),
        )
        // 下行：进度条（占满）+ 尾标（进行中给百分比，结束态给中文状态）。
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(theme::hover_bg())
                        .child(
                            div()
                                .h(px(4.0))
                                .w(relative(ratio))
                                .rounded(px(2.0))
                                .bg(theme::selected_bg()),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .min_w(px(40.0))
                        .text_size(px(11.0))
                        .text_color(if running {
                            theme::muted()
                        } else {
                            status_color(op)
                        })
                        .child(text!(status_tail(op, ratio))),
                ),
        )
}

/// 进度比值（total == 0 时给 0，别除零）；已结束的成功任务视为 100%。
fn ratio_of(op: &OperationHandle) -> f32 {
    if op.status == OperationStatus::Completed {
        return 1.0;
    }
    let (done, total) = op.progress;
    if total > 0 {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// 行尾的小字：进行中给百分比；结束态给中文状态（不再出现「完成 0%」这种
/// 自相矛盾的组合）。
fn status_tail(op: &OperationHandle, ratio: f32) -> String {
    match op.status {
        OperationStatus::Pending | OperationStatus::Running => format!("{:.0}%", ratio * 100.0),
        _ => status_label(op).to_string(),
    }
}

/// 状态的中文短标签。
fn status_label(op: &OperationHandle) -> &'static str {
    match op.status {
        OperationStatus::Pending => "排队",
        OperationStatus::Running => "进行中",
        OperationStatus::Paused => "已暂停",
        OperationStatus::Completed => "完成",
        OperationStatus::Failed => "失败",
        OperationStatus::Cancelled => "已取消",
    }
}

/// 状态点 / 结束态文字的颜色。调色板没有 success / danger 角色，用固定的
/// 系统语义色（Apple system green / red 的深档，浅深底都可读）。
fn status_color(op: &OperationHandle) -> Rgba {
    match op.status {
        OperationStatus::Pending | OperationStatus::Running => theme::selected_bg(),
        OperationStatus::Completed => rgba(0x248a3d),
        OperationStatus::Failed => rgba(0xd70015),
        OperationStatus::Paused | OperationStatus::Cancelled => theme::muted(),
    }
}
