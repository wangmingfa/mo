use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_app::AppState;
use mo_operations::{OperationHandle, OperationStatus};
use std::collections::HashMap;

use crate::{theme, RootView};

/// 常显卡片尺寸：与侧栏同宽（188）减去左右 6px 留白，一行文字 + 3px 进度条。
const CARD_W: f32 = 176.0;
const CARD_H: f32 = 30.0;
/// 任务浮层宽度：比卡片宽（任务描述 + 速度 + 剩余时间需要横向空间），
/// 左缘与卡片对齐、向上展开，超出侧栏盖在内容区上是浮层的本分。
const POPOVER_W: f32 = 380.0;
/// 任务行定高（两行式内容垂直居中）。定高是为了让列表高度可以**算出来**：
/// `Scrollable` 需要定高上下文，auto 高度链 + `max_h` 撑不出滚动区。
const ROW_H: f32 = 56.0;
/// 任务列表的滚动区上限；行数少时列表按内容自适应（不撑到上限）。
const LIST_MAX_H: f32 = 300.0;

/// 左下角统一**任务管理器**（状态栏上方，仿 GNOME Files / Nautilus）。
///
/// 所有耗时操作（删除 / 复制 / 移动 / 远程传输，以及后续的索引、同步等长期任务）
/// 都汇进 `OperationManager` 的同一份快照，在这里统一呈现。两层结构：
///
/// * **常显卡片**：贴侧栏底部同宽的长条，显示「N 个任务 + 总百分比」和一条聚合
///   进度条——有任何任务（含刚结束还没清走的）就一直在；
/// * **任务浮层**：点卡片后在卡片**上方**（top-start 对齐卡片左缘）弹出，列出
///   全部任务（行样式见 [`render_op_row`]），右上角扫帚一键清除已完成的任务。
///   点空白处（`on_mouse_down_out`）收起；任务全部移除后由渲染层自动收起
///   （见 `RootView::render` 里 `ops_open` 的复位）。
///
/// 数据来自 `OperationManager::snapshot()`（经 `tab.ops` 快照），由事件总线驱动刷新。
/// `speeds` 是 UI 层对相邻快照差分出的估速（`RootView::op_speeds`），
/// 进行中的任务显示「速度 · 剩余时间」，首次观测 / 估不出时不显示。
/// 两层整体绝对定位，**不占布局**：有没有任务，文件区高度都不变。
pub fn render_overlay(
    ops: &[OperationHandle],
    speeds: &HashMap<u64, (f32, f64)>,
    app: &AppState,
    open: bool,
    entity: &Entity<RootView>,
) -> impl IntoElement {
    if ops.is_empty() {
        // 与正常分支同型（Stateful<Div>），`impl IntoElement` 两个分支必须同型。
        return div().id("mo-ops-empty");
    }

    // 外层 wrapper 挂 `on_mouse_down_out`：点浮层外（空白处）收起。
    // ⚠️ 卡片与浮层必须**同包这一个 wrapper**，且浮层走**流内布局**（不用
    // absolute）：wrapper 锚定 bottom、内容向上生长，浮层自然贴在卡片上方；
    // 若浮层 absolute 定位，它不占 wrapper 的 hitbox 矩形，点浮层自己就会被
    // 「点外面」误判收起（实测踩到）。也别把卡片与浮层拆成两个 wrapper。
    let out = entity.clone();
    let mut wrapper = div()
        .id("mo-ops-overlay")
        .absolute()
        // 状态栏 26px，再留 6px 空隙；贴左下角（侧栏底部）。
        .left(px(10.0))
        .bottom(px(32.0))
        .flex()
        .flex_col()
        .items_start()
        .gap(px(6.0))
        .occlude()
        .debug_selector(|| "mo-ops-overlay".to_string());
    wrapper
        .interactivity()
        .on_mouse_down_out(move |_ev, _window, cx| {
            out.update(cx, |v, cx| v.close_ops_popover(cx));
        });

    // ── 任务浮层：流内第一个 child，贴在卡片上方，左缘对齐（top-start）。
    if open {
        wrapper = wrapper.child(render_popover(ops, speeds, app, entity));
    }

    // ── 常显卡片：N 个任务 + 总百分比 + 聚合进度条，整卡点击开合浮层 ──────
    let pct = aggregate_ratio(ops);
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .flex_1()
        .px(px(10.0))
        .text_size(px(11.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme::muted())
                .child(text!(format!("{} 个任务", ops.len()))),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_color(theme::text())
                .child(text!(format!("{:.0}%", pct * 100.0))),
        );
    // 点击开合挂**外层卡片**一层就够了：行里再挂一次会经冒泡触发两遍
    // （toggle × 2 = 没开），嵌套可点击元素必须只有最内层带语义或外层拦冒泡。
    let card = div()
        .id("mo-ops-badge")
        .w(px(CARD_W))
        .h(px(CARD_H))
        .flex()
        .flex_col()
        .rounded(px(8.0))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::separator())
        .shadow_lg()
        .overflow_hidden()
        .hover(|s| s.bg(theme::hover_bg()))
        .debug_selector(|| "mo-ops-badge".to_string())
        .child(row)
        // 通栏聚合进度条：不占行高也能看出「在动」。
        // ⚠️ 这个 fork 的 `div` **不把子元素裁进父级圆角**（`overflow_hidden()`
        // 不被消费），通栏贴边会从卡片 8px 圆角底下戳出去；而 `paint_quad` 又
        // 不 clamp 超尺寸圆角（3px 高装不下 8px 半径），子元素自己 `rounded_b(8)`
        // 也画不对。收进卡片左右 10px 的直边区（与行内边距对齐），条自身用
        // ≤半高的 pill 圆角——任何位置都不与圆角相交。
        .child(
            div()
                .id("mo-ops-bar")
                .h(px(3.0))
                .mx(px(10.0))
                .rounded(px(1.5))
                .bg(theme::hover_bg())
                .debug_selector(|| "mo-ops-bar".to_string())
                .child(
                    div()
                        .h(px(3.0))
                        .w(relative(pct))
                        .rounded(px(1.5))
                        .bg(theme::selected_bg()),
                ),
        );
    // 点击开合挂**外层卡片**这一层：行里再挂一次会经冒泡触发两遍
    // （toggle × 2 = 没开），嵌套可点击区域只有最外层带语义。
    let toggle = entity.clone();
    let mut card = card;
    card.interactivity().on_click(move |_, _window, cx| {
        toggle.update(cx, |v, cx| v.toggle_ops_popover(cx));
    });
    // `.test_support()`：headless 的 `click("mo-ops-badge")` 只认**被观察**的元素；
    // 非 test 构建（不带 test-support feature）这是恒等包装，不影响产物。
    // 必须**最后**包（挂完 children / 事件再包，与 sidebar / trash 行同款，实测踩过）。
    wrapper.child(card.test_support())
}

/// 任务浮层：标题行（任务数 + 扫帚）+ 全部任务列表，绝对定位在卡片上方。
fn render_popover(
    ops: &[OperationHandle],
    speeds: &HashMap<u64, (f32, f64)>,
    app: &AppState,
    entity: &Entity<RootView>,
) -> impl IntoElement {
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

    // 扫帚：一键清除**已完成**的任务（失败的留着——用户要看得见错误）。
    // 没有已完成任务时置灰（点了也是空操作，但不藏着——位置要稳定）。
    let done_ids: Vec<u64> = ops
        .iter()
        .filter(|op| op.status == OperationStatus::Completed)
        .map(|op| op.id)
        .collect();
    let has_done = !done_ids.is_empty();
    let app_clear = app.clone();
    let entity_clear = entity.clone();
    let mut broom = div()
        .id("mo-ops-broom")
        .flex_shrink_0()
        .p(px(3.0))
        .rounded(px(4.0))
        .hover(|s| s.bg(theme::hover_bg()))
        .debug_selector(|| "mo-ops-broom".to_string())
        .child(crate::icons::icon(
            crate::icons::BROOM,
            13.0,
            if has_done {
                theme::text()
            } else {
                theme::muted()
            },
        ));
    broom.interactivity().on_click(move |_, _window, cx| {
        let app = app_clear.clone();
        let entity = entity_clear.clone();
        let ids = done_ids.clone();
        if ids.is_empty() {
            return;
        }
        cx.spawn(async move |cx| {
            for id in &ids {
                app.dismiss_operation(*id).await;
            }
            entity.update(cx, |v, cx| {
                // 乐观更新：不等进度泵的下一拍，行立刻消失（见
                // `remove_ops_from_snapshot` 的文档）。
                v.remove_ops_from_snapshot(&ids);
                cx.notify();
            });
        })
        .detach();
    });

    let header = div()
        .id("mo-ops-popover-header")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(7.0))
        .text_size(px(11.0))
        .text_color(theme::muted())
        .debug_selector(|| "mo-ops-popover-header".to_string())
        .child(div().flex_1().min_w_0().truncate().child(text!(summary)))
        .child(broom.test_support());

    // 列表高度**算出来**而非 max_h 钳：`Scrollable`（overflow_y_scrollbar）的
    // 根节点走 size_full 并从调用方抄 size——处在 auto 高度链里时撑不出有界
    // 滚动区，既滚不动也看不见滚动条（用户实测）。行高是定值（[`ROW_H`]），
    // 行数少时高度=内容自然高，不浪费空间。
    let list_h = (ops.len() as f32 * ROW_H).min(LIST_MAX_H);
    div()
        .id("mo-ops-popover")
        .w(px(POPOVER_W))
        .flex()
        .flex_col()
        .rounded(px(8.0))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::separator())
        .shadow_lg()
        .overflow_hidden()
        .debug_selector(|| "mo-ops-popover".to_string())
        .child(header)
        .child(
            div()
                .h(px(list_h))
                .flex()
                .flex_col()
                .debug_selector(|| "mo-ops-list".to_string())
                .overflow_y_scrollbar()
                .border_t_1()
                .border_color(theme::separator())
                .children(
                    ops.iter()
                        .map(|op| render_op_row(op, speeds.get(&op.id).copied(), app, entity)),
                ),
        )
}

/// 展开列表里的一行：两行式——上行「状态点 + 描述 + 动作」，下行「进度条 + 尾标」。
fn render_op_row(
    op: &OperationHandle,
    speed: Option<(f32, f64)>,
    app: &AppState,
    entity: &Entity<RootView>,
) -> impl IntoElement {
    let ratio = ratio_of(op);
    let running = matches!(
        op.status,
        OperationStatus::Pending | OperationStatus::Running
    );
    let paused = op.status == OperationStatus::Paused;

    // 行尾动作按状态分流：可暂停的传输在跑 → 暂停；已暂停 → 继续；
    // 其余进行中 / 排队 → 取消；已结束 → ✕ 移除（句柄不摘会一直堆着）。
    // 「暂停 / 继续」只给 `pausable` 的操作：单文件快操作按了也没处停。
    let app_click = app.clone();
    let entity_click = entity.clone();
    let id = op.id;
    let label = if paused {
        "继续"
    } else if running {
        if op.pausable {
            "暂停"
        } else {
            "取消"
        }
    } else {
        "✕"
    };
    let mut action = div()
        .id(("mo-ops-action", op.id))
        .flex_shrink_0()
        .px(px(4.0))
        .rounded(px(4.0))
        .text_size(px(11.0))
        .text_color(theme::muted())
        .hover(|s| s.bg(theme::hover_bg()))
        .child(text!(label.to_string()));
    action.interactivity().on_click(move |_, _window, cx| {
        // ⚠️ 行本身没有 on_click，这里不需要 stop_propagation；
        // 若将来给行加了点击语义，记得先拦冒泡。
        let app = app_click.clone();
        let entity = entity_click.clone();
        let label = label;
        cx.spawn(async move |cx| {
            match label {
                "暂停" => app.pause_operation(id).await,
                "继续" => app.resume_operation(id).await,
                "取消" => app.cancel_operation(id).await,
                _ => app.dismiss_operation(id).await,
            }
            entity.update(cx, |v, cx| {
                // 已结束项的 ✕ 与扫帚同一套乐观更新：不等下一个总线事件，行立刻消失。
                if label == "✕" {
                    v.remove_ops_from_snapshot(&[id]);
                }
                cx.notify();
            });
        })
        .detach();
    });

    div()
        .id(("mo-ops-row", op.id))
        .debug_selector(move || format!("mo-ops-row-{}", op.id))
        // 定高 + 垂直居中：列表高度按 [`ROW_H`] 算，行必须守约。
        .h(px(ROW_H))
        .justify_center()
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
                        .child(text!(status_tail(op, ratio, speed))),
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

/// 常显卡片上的**聚合百分比**：每个任务取自身进度比再对任务数取平均——
/// 任务管理器语义下的「整体完成度」（已完成任务由 `ratio_of` 记满格）。
fn aggregate_ratio(ops: &[OperationHandle]) -> f32 {
    if ops.is_empty() {
        return 0.0;
    }
    ops.iter().map(ratio_of).sum::<f32>() / ops.len() as f32
}

/// 行尾的小字：进行中给百分比，估得出速度再补「速度 · 剩余时间」；
/// 结束态给中文状态（不再出现「完成 0%」这种自相矛盾的组合）。
fn status_tail(op: &OperationHandle, ratio: f32, speed: Option<(f32, f64)>) -> String {
    match op.status {
        OperationStatus::Pending | OperationStatus::Running => {
            let mut s = format!("{:.0}%", ratio * 100.0);
            if let Some((bps, eta)) = speed {
                // 首次观测还没差分出速度（或样本太少），只显示百分比。
                if bps > 0.0 {
                    s.push_str(&format!(" · {}", speed_label(bps)));
                    if eta > 0.0 {
                        s.push_str(&format!(" · {}", eta_label(eta)));
                    }
                }
            }
            s
        }
        _ => status_label(op).to_string(),
    }
}

/// 字节速度的人类可读形式（1 MB/s 级别之前保留一位小数，往上取整省宽度）。
fn speed_label(bps: f32) -> String {
    const UNITS: [&str; 5] = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"];
    let mut v = bps.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    // 字节档没有小数；大数值（≥100）取整也够读，省一行宽度。
    if i == 0 || v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// 剩余时间的人类可读形式：秒 → 「剩余 8s」，分钟 → 「剩余 1m20s」，
/// 小时 → 「剩余 2h05m」。速度估不出时调用方就不显示，不给「剩余 0s」。
fn eta_label(secs: f64) -> String {
    let s = secs.ceil() as u64;
    if s >= 3600 {
        format!("剩余 {}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("剩余 {}m{:02}s", s / 60, s % 60)
    } else {
        format!("剩余 {s}s")
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
