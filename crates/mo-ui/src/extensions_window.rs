//! 扩展管理器的**独立窗口**。
//!
//! 扩展页原先顶掉主窗口的中央区（`Modal::Extensions`）——看一眼「这个扩展会往
//! 界面里放什么」，正在浏览的文件就没了。现在迁到独立窗口：主窗口照常浏览，
//! 扩展管理器并排开着（与快速预览窗口同一套懒开 + 复用置前的习惯，见
//! [`crate::RootView::open_extensions_picker`]）。
//!
//! 状态**全部**住在主窗口根视图（[`RootView`]：清单、键表、选中行、确认卡都
//! 是它的字段），本窗口只是**借渲染**——页面主体由
//! [`RootView::render_extensions`] 产出，监听器直接改主窗口根视图的状态。
//! 因此状态一变要手动给本窗口递通知（两个窗口各画各的帧），见
//! [`RootView::sync_extensions_window`]。
//!
//! 键盘（Esc / ↑↓ / Enter 的语义与原 `handle_modal_key` 的扩展分支一致）：
//! * 确认卡开着：Esc = 取消、Enter = 确认（启用 / 卸载）；
//! * 否则：↑↓ 换选中行、Enter 翻启停（启用先过确认卡）、Esc 关本窗口。

use gpui_kit::*;

use crate::app::{
    confirm_enable_ext, confirm_remove_broken_ext, confirm_uninstall_ext, dismiss_enable_confirm,
    dismiss_remove_broken_confirm, dismiss_uninstall_confirm, render_enable_confirm,
    render_remove_broken_confirm, render_uninstall_confirm,
};
use crate::theme;
use crate::RootView;

/// 扩展管理器窗口的根视图。
pub struct ExtensionsWindow {
    focus: FocusHandle,
    /// 主窗口根视图：扩展页的状态与监听器都在那边。
    root: Entity<RootView>,
    /// 首帧占位开关。`open_window` 是**同步**渲染首帧的，而开窗发生在
    /// `RootView::open_extensions_picker` 里——那一刻主视图还锁在
    /// `root.update` 中，此刻去 `read` 它会 panic（「already being updated」，
    /// headless 实测）。所以第一帧只画空壳；开窗一返回，主视图立刻
    /// `set_ready` + 通知，第二帧起才真正读主视图渲染扩展页。
    ready: bool,
}

impl Focusable for ExtensionsWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ExtensionsWindow {
    pub fn new(root: Entity<RootView>, cx: &mut Context<Self>) -> Self {
        Self {
            root,
            focus: cx.focus_handle(),
            ready: false,
        }
    }

    /// 首帧占位结束：主视图开完窗（脱离 `root.update`）后调用，下一帧起读真页。
    pub fn set_ready(&mut self, cx: &mut Context<Self>) {
        self.ready = true;
        cx.notify();
    }
}

impl Render for ExtensionsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = self.root.clone();
        let mut root = div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::surface())
            .text_color(theme::text())
            .text_size(px(13.0));

        // 首帧只画空壳（见 `ready` 字段注释）：此刻主视图还锁在 `root.update` 里。
        if self.ready {
            // 页面主体借主窗口根视图渲染（外壳与原 `Modal::Extensions` 时代同款：
            // 标题「扩展」+ 底部键位提示）。
            root = root.child(self.root.read(cx).render_extensions(&entity));

            // 确认卡浮层：状态在 `RootView.modal`（确认卡开着期间扩展页仍画在
            // 下面，与遮罩时代同形）。卡开着期间清单被别处改掉了（目录被删）
            // 就没有这张卡可画，什么都不挂。
            match self.root.read(cx).modal.clone() {
                crate::app::Modal::ConfirmEnableExt(id) => {
                    if let Some(m) = self
                        .root
                        .read(cx)
                        .extensions
                        .iter()
                        .find(|e| e.manifest.id == id)
                        .map(|e| e.manifest.clone())
                    {
                        let lines = self.root.read(cx).contribution_lines(&m.id);
                        root = root.child(render_enable_confirm(&m, &lines, &entity));
                    }
                }
                crate::app::Modal::ConfirmUninstallExt(id) => {
                    if let Some(m) = self
                        .root
                        .read(cx)
                        .extensions
                        .iter()
                        .find(|e| e.manifest.id == id)
                        .map(|e| e.manifest.clone())
                    {
                        root = root.child(render_uninstall_confirm(&m, &entity));
                    }
                }
                crate::app::Modal::ConfirmRemoveBrokenExt(dir) => {
                    let reason = self
                        .root
                        .read(cx)
                        .broken_exts
                        .iter()
                        .find(|b| b.path.parent() == Some(std::path::Path::new(&dir)))
                        .map(|b| b.reason.clone())
                        .unwrap_or_default();
                    root = root.child(render_remove_broken_confirm(&dir, &reason, &entity));
                }
                _ => {}
            }
        }

        // Esc / ↑↓ / Enter 的语义与原扩展页键位一致（见模块注释）。
        let nav = entity.clone();
        root.interactivity().on_key_down(move |ev, window, cx| {
            let confirm_open = matches!(
                nav.read(cx).modal,
                crate::app::Modal::ConfirmEnableExt(_)
                    | crate::app::Modal::ConfirmUninstallExt(_)
                    | crate::app::Modal::ConfirmRemoveBrokenExt(_)
            );
            match ev.keystroke.key.as_str() {
                "escape" => match nav.read(cx).modal.clone() {
                    crate::app::Modal::ConfirmEnableExt(_) => dismiss_enable_confirm(&nav, cx),
                    crate::app::Modal::ConfirmUninstallExt(_) => {
                        dismiss_uninstall_confirm(&nav, cx)
                    }
                    crate::app::Modal::ConfirmRemoveBrokenExt(_) => {
                        dismiss_remove_broken_confirm(&nav, cx)
                    }
                    // 没有确认卡：Esc 关掉整个扩展管理器窗口。
                    _ => window.remove_window(),
                },
                "enter" => match nav.read(cx).modal.clone() {
                    crate::app::Modal::ConfirmEnableExt(_) => confirm_enable_ext(&nav, cx),
                    crate::app::Modal::ConfirmUninstallExt(_) => confirm_uninstall_ext(&nav, cx),
                    crate::app::Modal::ConfirmRemoveBrokenExt(_) => {
                        confirm_remove_broken_ext(&nav, cx)
                    }
                    _ => nav.update(cx, |v, cx| v.extension_toggle_selected(cx)),
                },
                // 确认卡开着**不**吃上下键——卡还开着就换选中行，等于确认的是别条扩展。
                "up" | "arrowup" if !confirm_open => {
                    nav.update(cx, |v, cx| v.extension_step(-1, cx));
                }
                "down" | "arrowdown" if !confirm_open => {
                    nav.update(cx, |v, cx| v.extension_step(1, cx));
                }
                _ => {}
            }
        });

        // 让按键落到本窗口视图上。
        if !self.focus.is_focused(window) {
            cx.focus_self(window);
        }

        root
    }
}
