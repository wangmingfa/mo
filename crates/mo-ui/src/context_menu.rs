//! 右键上下文菜单（文件 / 文件夹 / 空白处）。
//!
//! 菜单本身**不持有任何业务状态**：它只记录「在哪儿弹的、对着谁」，条目与可用性
//! 由 [`items`] 从菜单状态 + 当前选中数推导出来；真正干活的是
//! [`crate::RootView::run_menu_action`]——那里把所有动作翻译成既有的命令
//! （`mo-app` 的 `AppState` 调用或既有模态），菜单层不直接碰文件系统。
//!
//! 定位用**窗口坐标**：菜单作为根容器的绝对定位子节点，根容器从 `(0, 0)` 铺满窗口，
//! 所以鼠标事件的坐标可以直接当偏移量用（与表头拖拽落点判定同一套坐标）。
//! 渲染时会按视口尺寸做一次钳制，避免贴近右 / 下边缘时菜单被切掉。

use std::path::PathBuf;

use gpui_kit::*;

use crate::theme;

/// 菜单面板宽度。
pub(crate) const MENU_W: f32 = 232.0;

/// 单个菜单项的高度。
const ITEM_H: f32 = 26.0;

/// 菜单面板的上下内边距。
const PAD: f32 = 4.0;

/// 分隔线占用的高度（1px 线 + 上下各 3px 呼吸）。
const SEP_H: f32 = 7.0;

/// 菜单与视口边缘之间保留的最小距离。
const EDGE: f32 = 4.0;

/// 一次右键菜单的上下文。
#[derive(Clone, Debug)]
pub(crate) struct ContextMenu {
    /// 菜单左上角落点（窗口坐标，尚未钳制）。
    pub x: f32,
    pub y: f32,
    /// 右键命中的条目；`None` = 点在空白处，操作对象是**当前目录**。
    pub target: Option<PathBuf>,
    /// `target` 是否为目录（决定「打开」语义与哪些条目可用）。
    pub is_dir: bool,
    /// 触发时选中的条目数（用于「移到废纸篓（3 项）」这类文案与可用性）。
    pub selected: usize,
    /// 打开菜单那一刻的选中路径快照（按可见顺序）。
    ///
    /// 右键时会给 app 发一个异步的「选中这一项」。开模态的动作（重命名 / 压缩 /
    /// 创建副本 / 拷贝路径）如果去读 app 的选择，就要赌那个任务已经跑完——
    /// 直接带这份快照就没有竞态。
    pub paths: Vec<PathBuf>,
}

/// 菜单里的一个动作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuAction {
    /// 打开：目录进入，文件快速预览。
    Open,
    /// 在**新标签页**里打开该目录。
    OpenInNewTab,
    /// 在**第二个窗格**里打开该目录（分栏）。
    OpenInSplit,
    /// 快速预览（与空格键同语义）。
    QuickLook,
    /// 重命名（单个走批量重命名对话框，规则留空即是改名）。
    Rename,
    /// 同目录就地复制（`a.txt` → `a 2.txt`）。
    Duplicate,
    Copy,
    Cut,
    Paste,
    /// 把选中项的路径（每行一个）写进剪贴板。
    CopyPath,
    /// 移到废纸篓。
    Trash,
    /// 在当前目录新建文件夹。
    NewFolder,
    /// 在当前目录新建空文本文件。
    NewFile,
    Compress,
    Extract,
    Hash,
    Compare,
    Tags,
    Properties,
    DiskUsage,
    OpenTerminal,
    Refresh,
    SelectAll,
}

/// 一行菜单项。`separator_before` 为真时它上方还有一条分隔线。
pub(crate) struct MenuItem {
    pub action: MenuAction,
    pub label: String,
    /// 右侧快捷键提示（纯展示，不参与命中；这些键本来就由全局快捷键接管）。
    pub hint: &'static str,
    pub enabled: bool,
    pub separator_before: bool,
}

impl MenuItem {
    fn new(
        action: MenuAction,
        label: impl Into<String>,
        hint: &'static str,
        enabled: bool,
    ) -> Self {
        Self {
            action,
            label: label.into(),
            hint,
            enabled,
            separator_before: false,
        }
    }

    fn separated(mut self) -> Self {
        self.separator_before = true;
        self
    }
}

/// 常见归档后缀（决定是否显示「解压」）。
fn looks_like_archive(path: &std::path::Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    const SUFFIXES: [&str; 8] = [
        ".zip", ".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz",
    ];
    SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// 按当前上下文推导菜单条目。
///
/// 空白处（`target == None`）给的是「目录级」动作（新建 / 粘贴 / 刷新 / 全选），
/// 对着条目给的是「条目级」动作，并按类型 / 选中数裁剪掉不合理的项。
pub(crate) fn items(menu: &ContextMenu) -> Vec<MenuItem> {
    let Some(target) = menu.target.as_ref() else {
        // ------------------------------------------------ 空白处：目录级动作
        // 空白处：目录级动作。两个「新建」同组（中间不隔线）。
        return vec![
            MenuItem::new(MenuAction::NewFolder, "新建文件夹", "", true),
            MenuItem::new(MenuAction::NewFile, "新建文本文件", "", true),
            MenuItem::new(MenuAction::Paste, "粘贴", "⌘V", true).separated(),
            MenuItem::new(MenuAction::SelectAll, "全选", "⌘A", true).separated(),
            MenuItem::new(MenuAction::Refresh, "刷新", "", true),
            MenuItem::new(MenuAction::OpenTerminal, "在终端中打开", "", true).separated(),
            MenuItem::new(MenuAction::Properties, "显示简介", "", true),
        ];
    };

    let is_dir = menu.is_dir;
    let multi = menu.selected > 1;
    let mut out = Vec::new();

    // 打开
    let open_label = if is_dir { "打开" } else { "快速查看" };
    out.push(MenuItem::new(
        if is_dir {
            MenuAction::Open
        } else {
            MenuAction::QuickLook
        },
        open_label,
        if is_dir { "↩" } else { "␣" },
        true,
    ));
    if is_dir {
        out.push(MenuItem::new(
            MenuAction::OpenInNewTab,
            "在新标签页中打开",
            "",
            true,
        ));
        out.push(MenuItem::new(
            MenuAction::OpenInSplit,
            "在分栏中打开",
            "",
            true,
        ));
    }

    // 编辑（重命名走批量重命名对话框，多选时正好用得上，所以始终可用）
    out.push(
        MenuItem::new(
            MenuAction::Rename,
            if multi {
                "批量重命名…"
            } else {
                "重命名…"
            },
            "F2",
            true,
        )
        .separated(),
    );
    out.push(MenuItem::new(
        MenuAction::Duplicate,
        if multi {
            "创建副本（多项）"
        } else {
            "创建副本"
        },
        "⌘D",
        true,
    ));
    out.push(MenuItem::new(MenuAction::Copy, "复制", "⌘C", true));
    out.push(MenuItem::new(MenuAction::Cut, "剪切", "⌘X", true));
    out.push(MenuItem::new(MenuAction::CopyPath, "拷贝路径", "⌥⌘C", true));

    // 删除
    out.push(
        MenuItem::new(
            MenuAction::Trash,
            if multi {
                format!("移到废纸篓（{} 项）", menu.selected)
            } else {
                "移到废纸篓".to_string()
            },
            "⌘⌫",
            true,
        )
        .separated(),
    );

    // 归档
    out.push(MenuItem::new(MenuAction::Compress, "压缩…", "", true).separated());
    if !is_dir && looks_like_archive(target) {
        out.push(MenuItem::new(
            MenuAction::Extract,
            "解压到当前目录",
            "",
            true,
        ));
    }

    // 工具
    out.push(
        MenuItem::new(
            MenuAction::Hash,
            if multi {
                "计算哈希（多项）…"
            } else {
                "计算哈希…"
            },
            "",
            !is_dir,
        )
        .separated(),
    );
    out.push(MenuItem::new(
        MenuAction::Compare,
        "比较选中的两项…",
        "",
        menu.selected == 2,
    ));

    // 信息
    out.push(MenuItem::new(MenuAction::Tags, "标签…", "", true).separated());
    out.push(MenuItem::new(
        MenuAction::Properties,
        "显示简介",
        "⌘I",
        true,
    ));
    if is_dir {
        out.push(MenuItem::new(
            MenuAction::DiskUsage,
            "分析磁盘用量",
            "",
            true,
        ));
        out.push(MenuItem::new(
            MenuAction::OpenTerminal,
            "在终端中打开",
            "",
            true,
        ));
    }

    out
}

/// 菜单面板的总高度（含内边距与分隔线），用于边缘钳制。
fn panel_height(items: &[MenuItem]) -> f32 {
    let inner: f32 = items
        .iter()
        .map(|it| {
            if it.separator_before {
                ITEM_H + SEP_H
            } else {
                ITEM_H
            }
        })
        .sum();
    inner + PAD * 2.0
}

/// 渲染菜单面板。
///
/// `viewport` 是窗口的逻辑尺寸，用于把菜单钳在可见区内。
pub(crate) fn render(
    menu: &ContextMenu,
    items: &[MenuItem],
    viewport: (f32, f32),
    entity: &Entity<crate::RootView>,
) -> impl IntoElement {
    let h = panel_height(items);
    // 钳制：先按「放在鼠标右下」算，再保证右边 / 下边不越界（越界就贴边）。
    let x = menu.x.min(viewport.0 - MENU_W - EDGE).max(EDGE);
    let y = menu.y.min(viewport.1 - h - EDGE).max(EDGE);

    let mut panel = div()
        .id("mo-context-menu")
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(MENU_W))
        .flex()
        .flex_col()
        .py(px(PAD))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::divider())
        .rounded(px(8.0))
        .shadow_lg()
        // 阻止点击穿透到下面的文件行（gpui 靠它截断命中链）。
        .occlude()
        // 测试用（release no-op）：定位 / 钳制的断言都查这个选择器。
        .debug_selector(|| "mo-context-menu".to_string())
        .text_size(px(13.0))
        .text_color(theme::text());

    // 点到菜单外面就关掉（捕获阶段回调用，不需要额外的全屏遮罩层）。
    let out_entity = entity.clone();
    panel
        .interactivity()
        .on_mouse_down_out(move |_ev, _window, cx| {
            out_entity.update(cx, |v, cx| v.close_context_menu(cx));
        });

    for (i, it) in items.iter().enumerate() {
        if it.separator_before {
            panel = panel.child(
                div()
                    .my(px(3.0))
                    .h(px(1.0))
                    .w_full()
                    .bg(theme::separator())
                    .debug_selector(|| "mo-ctx-sep".to_string()),
            );
        }

        let text_color = if !it.enabled {
            theme::muted()
        } else {
            theme::text()
        };
        let mut row = div()
            .id(("mo-ctx-item", i))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .h(px(ITEM_H))
            .px(px(10.0))
            .text_color(text_color)
            .child(
                div()
                    .flex_1()
                    .truncate()
                    // 测试用（release no-op）：按序数定位每一行菜单项。
                    .debug_selector(move || format!("mo-ctx-label-{i}"))
                    .child(text!(it.label.clone())),
            );
        if !it.hint.is_empty() {
            row = row.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .child(text!(it.hint.to_string())),
            );
        }

        if it.enabled {
            let action = it.action;
            let item_entity = entity.clone();
            row = row.hover(|s| s.bg(theme::hover_bg()));
            row.interactivity().on_click(move |_ev, _window, cx| {
                item_entity.update(cx, |v, cx| v.run_menu_action(action, cx));
            });
        }

        panel = panel.child(row);
    }

    panel
}

#[cfg(test)]
mod tests {
    // ⚠️ 不要 `use super::*`：本模块顶层有 `use gpui_kit::*`，它会把 gpui 的
    // `test` 属性宏一起带进来，把内置的 `#[test]` 顶掉，展开时直接撞递归上限。
    use super::{
        items, looks_like_archive, panel_height, ContextMenu, MenuAction, MenuItem, ITEM_H, PAD,
        SEP_H,
    };
    use std::path::{Path, PathBuf};

    fn menu(target: Option<&str>, is_dir: bool, selected: usize) -> ContextMenu {
        ContextMenu {
            x: 10.0,
            y: 10.0,
            target: target.map(PathBuf::from),
            is_dir,
            selected,
            paths: Vec::new(),
        }
    }

    fn actions(m: &ContextMenu) -> Vec<MenuAction> {
        items(m).into_iter().map(|i| i.action).collect()
    }

    fn find(list: &[MenuItem], a: MenuAction) -> &MenuItem {
        list.iter().find(|i| i.action == a).expect("条目不存在")
    }

    /// 空白处：只有目录级动作，绝不能出现「重命名 / 移到废纸篓」这类要对象的项。
    #[test]
    fn blank_area_shows_directory_level_actions_only() {
        let a = actions(&menu(None, true, 0));
        assert_eq!(
            a,
            vec![
                MenuAction::NewFolder,
                MenuAction::NewFile,
                MenuAction::Paste,
                MenuAction::SelectAll,
                MenuAction::Refresh,
                MenuAction::OpenTerminal,
                MenuAction::Properties,
            ]
        );
    }

    /// 两个「新建」必须挨在一起、中间没有分隔线（同属「新建」这一组）。
    #[test]
    fn new_items_share_one_group() {
        let list = items(&menu(None, true, 0));
        let folder = find(&list, MenuAction::NewFolder);
        let file = find(&list, MenuAction::NewFile);
        assert_eq!(folder.label, "新建文件夹");
        assert_eq!(file.label, "新建文本文件");
        assert!(folder.enabled && file.enabled);
        assert!(!file.separator_before, "「新建文本文件」不该自带分隔线");
        assert!(
            find(&list, MenuAction::Paste).separator_before,
            "「粘贴」上方应当有分隔线，把「新建」这组隔开"
        );
    }

    /// 目录：有「在新标签页 / 分栏中打开」，没有「解压」；「打开」而不是「快速查看」。
    #[test]
    fn directory_menu_has_open_variants_and_no_extract() {
        let a = actions(&menu(Some("/tmp/dir"), true, 1));
        assert!(a.contains(&MenuAction::Open), "目录应当用「打开」");
        assert!(!a.contains(&MenuAction::QuickLook));
        assert!(a.contains(&MenuAction::OpenInNewTab));
        assert!(a.contains(&MenuAction::OpenInSplit));
        assert!(a.contains(&MenuAction::DiskUsage));
        assert!(!a.contains(&MenuAction::Extract), "目录不该有解压");
    }

    /// 归档文件：多出「解压」；普通文件没有。
    #[test]
    fn extract_item_appears_only_for_archives() {
        let zip = actions(&menu(Some("/tmp/a.zip"), false, 1));
        assert!(zip.contains(&MenuAction::Extract));
        let txt = actions(&menu(Some("/tmp/a.txt"), false, 1));
        assert!(!txt.contains(&MenuAction::Extract));
        // 常见后缀都要认。
        for name in ["x.tar", "x.tar.gz", "x.tgz", "x.tar.bz2"] {
            let p = format!("/tmp/{name}");
            assert!(looks_like_archive(Path::new(&p)), "{name} 应当被识别为归档");
        }
    }

    /// 「比较」只在恰好 2 项时可用；重命名 / 哈希在多选时改文案但保持可用
    /// （批量重命名对话框与哈希工具本身就支持多项）。
    #[test]
    fn items_track_the_selection_size() {
        let three = items(&menu(Some("/tmp/a.txt"), false, 3));
        assert!(
            find(&three, MenuAction::Rename).enabled,
            "多项应当能批量重命名"
        );
        assert_eq!(find(&three, MenuAction::Rename).label, "批量重命名…");
        assert!(find(&three, MenuAction::Hash).enabled, "多项应当能算哈希");
        assert!(!find(&three, MenuAction::Compare).enabled, "3 项不能比较");

        let two = items(&menu(Some("/tmp/a.txt"), false, 2));
        assert!(
            find(&two, MenuAction::Compare).enabled,
            "恰好 2 项时应当可以比较"
        );

        let one = items(&menu(Some("/tmp/a.txt"), false, 1));
        assert_eq!(find(&one, MenuAction::Rename).label, "重命名…");
    }

    /// 高度要跟着分隔线一起算——钳制用的是它，算错菜单会被切掉一截。
    #[test]
    fn panel_height_accounts_for_separators() {
        let blank = items(&menu(None, true, 0));
        assert_eq!(blank.len(), 7);
        // 3 条分隔线：paste / select_all / open_terminal 各自上方一条。
        let seps = blank.iter().filter(|i| i.separator_before).count();
        assert_eq!(seps, 3);
        assert_eq!(panel_height(&blank), PAD * 2.0 + ITEM_H * 7.0 + SEP_H * 3.0);
    }
}
