use gpui_kit::*;
use mo_app::AppState;
use mo_operations::{OperationHandle, OperationStatus};

/// 后台操作进度面板。
///
/// 文件操作全部在后台执行，UI 不等待：这里显示每个操作的描述、进度与取消入口。
/// 数据来自 `OperationManager::snapshot()`，由事件总线驱动刷新。
pub fn render(ops: &[OperationHandle], app: &AppState) -> impl IntoElement {
    let mut panel = div().flex().flex_col().w_full();

    if ops.is_empty() {
        return panel;
    }

    for op in ops {
        let (done, total) = op.progress;
        let ratio = if total > 0 {
            (done as f32 / total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let status = match op.status {
            OperationStatus::Pending => "排队",
            OperationStatus::Running => "进行中",
            OperationStatus::Paused => "已暂停",
            OperationStatus::Completed => "完成",
            OperationStatus::Failed => "失败",
            OperationStatus::Cancelled => "已取消",
        };

        let mut row = div()
            // ⚠️ 必须有元素 ID：无 ID 的裸 div 拿不到 element_state，on_click 永远不触发。
            .id(("op-row", op.id))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .w_full()
            .p(px(6.0))
            .bg(gpui_kit::white());

        // 进行中或排队中的操作可以取消；已结束的只有状态。
        if matches!(
            op.status,
            OperationStatus::Pending | OperationStatus::Running
        ) {
            let id = op.id;
            let app_cancel = app.clone();
            row.interactivity().on_click(move |_, _window, cx| {
                let app = app_cancel.clone();
                cx.spawn(async move |_cx| {
                    app.cancel_operation(id).await;
                })
                .detach();
            });
        }

        panel = panel.child(
            row.child(div().w(px(320.0)).child(text!(op.describe.clone())))
                .child(div().w(px(64.0)).child(text!(status.to_string())))
                .child(
                    div()
                        .flex_1()
                        .h(px(6.0))
                        .bg(gpui_kit::rgb(0xdddddd))
                        .child(div().h(px(6.0)).w(relative(ratio)).bg(gpui_kit::blue())),
                )
                .child(text!(format!("{:.0}%", ratio * 100.0))),
        );
    }

    panel
}
