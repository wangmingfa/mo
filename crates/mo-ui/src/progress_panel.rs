use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_app::AppState;
use mo_operations::{OperationHandle, OperationStatus};
use std::collections::HashMap;

use crate::{theme, RootView};

/// 常显卡片尺寸：侧栏宽 188，减去**左右各 10**（与卡片自己的 `left(10)`、状态栏的
/// `px(10)` 同一条竖线）。两边留白必须相等——写成 176 时右边只剩 2px，用户一眼看出
/// 「左右间距不一样」（§47，`tests/layout.rs::badge_is_centered_in_the_sidebar_column`）。
const CARD_W: f32 = 168.0;
/// 卡片高：环（[`RING_D`]）+ 上下各 3px。旧值 30 装不下环，是 §48 把聚合条换成环时
/// 一起长的——卡片是绝对定位的浮层，长高不占布局、不动文件区高度。
const CARD_H: f32 = 42.0;
/// 环形进度条的直径，卡片与浮层行**共用这一个数**。36 是环内文字反推出来的，不是随手
/// 定的：笔画宽由组件按 `min(0.15×直径, 5px)` 算（36 时封顶 5），环内净空 = 直径 −
/// 2×笔画 = 26，[`RING_TEXT`] 字号的「100%」实测宽 22，留 4px 余量——32 的环净空只有
/// 22.4，同样的字只剩 0.4px，换个字体度量就骑到环壁上。这条由
/// `tests/layout.rs::aggregate_ring_keeps_its_diameter_label_and_corners` 拿真实
/// bounds 钉住（§48）。
const RING_D: f32 = 36.0;
/// 环内百分比的字号。
const RING_TEXT: f32 = 9.0;
/// 任务浮层宽度：比卡片宽（任务描述 + 速度 + 剩余时间需要横向空间），
/// 左缘与卡片对齐、向上展开，超出侧栏盖在内容区上是浮层的本分。
const POPOVER_W: f32 = 440.0;
/// 任务行的**保底**高度（一行描述 + 一行环：`DESC_LINE_H + gap 4 + RING_D + 上下
/// py 14`，内容垂直居中）。描述按真实换行撑开（长路径自动加高，见 [`render_op_row`]），
/// 这只是下限。保底值仍要存在：列表滚动区高度是**算出来**的，`Scrollable` 需要定高
/// 上下文，auto 高度链 + `max_h` 撑不出滚动区。
const ROW_H: f32 = 70.0;
/// 描述文本的行高——**显式钉住**：行高估算（[`estimated_row_h`]）与真实布局
/// 必须用同一个值，否则估的行高和实际渲染高度对不上。
const DESC_LINE_H: f32 = 16.0;
/// 任务列表的滚动区上限；行数少时列表按内容自适应（不撑到上限）。
const LIST_MAX_H: f32 = 300.0;

/// 左下角统一**任务管理器**（状态栏上方，仿 GNOME Files / Nautilus）。
///
/// 所有耗时操作（删除 / 复制 / 移动 / 远程传输，以及后续的索引、同步等长期任务）
/// 都汇进 `OperationManager` 的同一份快照，在这里统一呈现。两层结构：
///
/// * **常显卡片**：贴侧栏底部同宽的长条，「N 个任务」加一只**聚合环**（总百分比写在
///   环里）——有任何任务（含刚结束还没清走的）就一直在；
/// * **任务浮层**：点卡片后在卡片**上方**（top-start 对齐卡片左缘）弹出，列出
///   全部任务（行样式见 [`render_op_row`]，每行一只同款环、环色即状态色），右上角
///   扫帚一键清除已完成的任务。
///   点空白处（`on_mouse_down_out`）收起；任务全部移除后由渲染层自动收起
///   （见 `RootView::render` 里 `ops_open` 的复位）。
///
/// 进度一律用 [`ProgressCircle`]（gpui-kit 现成组件：真画弧线路径，`canvas` +
/// `paint_path`），不再用细条——环能同时说清「多少」「在不在动」「什么状态」，
/// 百分比也就有了地方放（§48）。
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

    // ── 常显卡片：N 个任务 + 聚合环（百分比写在环里），整卡点击开合浮层 ──
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
        .child(progress_ring(
            "mo-ops-aggregate",
            pct,
            theme::selected_bg(),
            "mo-ops-ring".to_string(),
        ));
    // 点击开合挂**外层卡片**这一层就够了：环里没有可点区，但环要是挂了一次点击，
    // 冒泡到卡片会触发两遍（toggle × 2 = 没开）——嵌套点击只留最外层。
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
        .child(row);
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

/// 环形进度条 + 环内百分比：卡片与浮层行共用这一处构造，于是直径、字号、笔画算式
/// 只有一份（§48——两处各写一份的话，改了一处另一处就悄悄不对了）。
///
/// ⚠️ 直径用 `Styled` 的 `.w/.h` 钉，别换成 `Sizable::with_size`：那条路走
/// `progress_circle.rs` 的 `Size::Size(s) => this.size(s * 0.75)`，实测
/// `with_size(px(48.))` 画出来是 36，而 `.size(px(48.))`（= `.w/.h`）画出来是 48。
/// 测试 `tests/layout.rs::aggregate_ring_keeps_its_diameter_label_and_corners`
/// 量的就是画出来的那条边。
fn progress_ring(id: impl Into<ElementId>, ratio: f32, color: Rgba, selector: String) -> Div {
    let label_selector = format!("{selector}-label");
    div()
        .flex_shrink_0()
        .debug_selector(move || selector.clone())
        .child(
            ProgressCircle::new(id)
                // 组件吃的是 0~100 的百分值，不是 0~1 的比值。
                .value(ratio * 100.0)
                .color(color)
                .w(px(RING_D))
                .h(px(RING_D))
                .child(
                    div()
                        .debug_selector(move || label_selector.clone())
                        .text_size(px(RING_TEXT))
                        .text_color(theme::text())
                        .child(text!(format!("{:.0}%", (ratio * 100.0).round()))),
                ),
        )
}

/// 测试专用：环的 `(直径, 环内字号)`（见 `lib.rs::progress_ring_geometry_for_tests`）。
/// 布局判据要读**同一份来源**，不能把直径数字抄进测试——那正是 §47 收过账的写法。
#[doc(hidden)]
pub(crate) fn ring_geometry_for_tests() -> (f32, f32) {
    (RING_D, RING_TEXT)
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
    // 滚动区，既滚不动也看不见滚动条（用户实测）。行高按描述估行数折算
    // （[`estimated_row_h`]），行数少时高度=内容自然高，不浪费空间。
    let list_h = ops
        .iter()
        .map(|op| estimated_row_h(&op.describe))
        .sum::<f32>()
        .min(LIST_MAX_H);
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

/// 展开列表里的一行：两行式——上行「状态点 + 描述 + 动作」，下行「进度环 + 尾标」。
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
        // 保底高 + 垂直居中：单行描述时与旧定高观感一致；描述按**真实换行**
        // 撑开（长路径不截断，用户要看得见完整路径），行自然加高。列表滚动区
        // 高度按估算折算（[`estimated_row_h`]），估差不裁字、只差几像素滚动余量。
        .min_h(px(ROW_H))
        .justify_center()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .px(px(12.0))
        .py(px(7.0))
        .hover(|s| s.bg(theme::hover_bg()))
        // 上行：状态点 + 描述（自然换行，行高钉 [`DESC_LINE_H`]）+ 动作。
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
                        .text_size(px(12.0))
                        .line_height(px(DESC_LINE_H))
                        .text_color(theme::text())
                        .child(text!(op.describe.clone())),
                )
                .child(action),
        )
        // 下行：环（百分比在里面）+ 尾标（环里装不下的信息：速度 / 剩余时间 / 中文状态）。
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(progress_ring(
                    ("mo-ops-ring", op.id),
                    ratio,
                    status_color(op),
                    format!("mo-ops-ring-{}", op.id),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(if running {
                            theme::muted()
                        } else {
                            status_color(op)
                        })
                        .child(text!(status_tail(op, speed))),
                ),
        )
}

/// 描述文本在浮层行内的可用宽度：浮层宽 − 左右内边距 − 状态点 − 两处 gap −
/// 行尾动作钮（「暂停 / 继续 / 取消」两字 + 内边距的余量）。估算与真实布局
/// 共用这一个推导，改浮层内边距时两处自动同调。
fn desc_usable_w() -> f32 {
    POPOVER_W - 12.0 * 2.0 - 7.0 - 8.0 * 2.0 - 40.0
}

/// 估算一行描述折成的行数（半角记 0.55em、CJK/全角记 1em，按
/// [`DESC_LINE_H`] 的 12px 字号折像素）。启发式只服务**滚动视口高度**：
/// 行本身按真实换行 auto 撑开，估差了最多让滚动条长度差几像素，不裁字。
fn estimate_desc_lines(text: &str) -> usize {
    let em: f32 = text
        .chars()
        .map(|ch| if ch.is_ascii() { 0.55 } else { 1.0 })
        .sum();
    let lines = (em * 12.0 / desc_usable_w().max(1.0)).ceil();
    lines.max(1.0) as usize
}

/// 单行的估算高度：行数 × 钉住的行高 + 下行环 [`RING_D`] + 行间 gap 4 + 上下
/// 内边距 14；不低于保底高 [`ROW_H`]（单行时的观感与旧定高一致）。环与估算绑
/// **同一个常量**——§47 那条教训：两处各写一份尺寸，改一处就漂移，这次直接引用。
fn estimated_row_h(describe: &str) -> f32 {
    let content = estimate_desc_lines(describe) as f32 * DESC_LINE_H + RING_D + 4.0 + 14.0;
    ROW_H.max(content)
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

/// 行尾的小字：百分比画进环里之后，这里只补环装不下的东西——进行中给
/// 「速度 · 剩余时间」（首次观测还没差分出速度就留空，宁可少一句也不写
/// 「· 3 MB/s」这种残句）；其余状态给中文标签（排队 / 已暂停 / 完成 / 失败 /
/// 已取消），于是不再出现「完成 0%」这种自相矛盾的组合。
fn status_tail(op: &OperationHandle, speed: Option<(f32, f64)>) -> String {
    if op.status != OperationStatus::Running {
        return status_label(op).to_string();
    }
    match speed {
        Some((bps, eta)) if bps > 0.0 => {
            let mut s = speed_label(bps);
            if eta > 0.0 {
                s.push_str(&format!(" · {}", eta_label(eta)));
            }
            s
        }
        _ => String::new(),
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
