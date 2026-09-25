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
pub(crate) const ITEM_H: f32 = 26.0;

/// 菜单面板的上下内边距。
pub(crate) const PAD: f32 = 4.0;

/// 菜单面板的圆角半径。
pub(crate) const PANEL_RADIUS: f32 = 8.0;

/// 菜单项 hover 底色的圆角半径。
///
/// 面板只有上下内边距，所以首 / 末项的底色矩形会一路顶到面板边缘、压住面板圆角
/// ——矩形的底色把圆角「切方」（gpui 不把子元素裁进父级圆角，与对话框标题栏
/// 那处是同一类问题）。取「面板半径 − 上下内边距」＝与面板同心，正好嵌在圆角内侧。
pub(crate) const ITEM_RADIUS: f32 = PANEL_RADIUS - PAD;

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
    /// 当前页是不是在**远程**（FTP / SFTP / WebDAV）。
    ///
    /// 只有它决定「在访达中显示」这类**本机**动作出不出菜单：远程条目在本机
    /// 磁盘上不存在，给了也是点了没反应（与「目录判据只问列表模型」同一条
    /// 纪律——别拿路径长相去猜它在哪个后端）。
    pub remote: bool,
}

/// 菜单里的一个动作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuAction {
    /// 打开：目录进入，文件用**系统默认应用**打开（与双击同语义）。
    Open,
    /// 用「打开方式」候选列表里的第 `usize` 个应用打开。
    OpenWith(usize),
    /// 弹出系统「打开方式」选择对话框。
    OpenWithOther,
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
    /// 移到废纸篓（可撤销、回收站面板里看得见）。macOS 生产模式下文件由系统
    /// 送进废纸篓、Mo 记账还原——对用户来说只有这一个「废纸篓」。
    Trash,
    /// 在系统的文件管理器里定位（macOS = 在访达中显示）。
    RevealInFileManager,
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
    /// 反选：把可见条目里没选中的换上来（与「全选」同一条「只动可见集」的边界）。
    InvertSelection,
    /// 收集到暂存区：与「复制」同组但语义是**追加**（见 `mo-app::staging`）。
    Stage,
}

/// 一行菜单项。`separator_before` 为真时它上方还有一条分隔线。
pub(crate) struct MenuItem {
    pub action: MenuAction,
    pub label: String,
    /// 右侧快捷键提示（纯展示，不参与命中；由键表推导，见 [`crate::keys::hint`]）。
    pub hint: String,
    pub enabled: bool,
    pub separator_before: bool,
    /// 二级菜单（「打开方式」的应用列表）。非空时本行 hover 展开，点击动作由
    /// 子项的 action 承担；本行自身的 action 不执行。
    pub submenu: Vec<(String, MenuAction)>,
}

impl MenuItem {
    fn new(
        action: MenuAction,
        label: impl Into<String>,
        hint: impl Into<String>,
        enabled: bool,
    ) -> Self {
        Self {
            action,
            label: label.into(),
            hint: hint.into(),
            enabled,
            separator_before: false,
            submenu: Vec::new(),
        }
    }

    fn with_submenu(mut self, submenu: Vec<(String, MenuAction)>) -> Self {
        self.submenu = submenu;
        self
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
/// `open_with` 是「打开方式」的候选应用列表（文件才有；在菜单打开时异步查询）。
pub(crate) fn items(menu: &ContextMenu, open_with: &[mo_app::shell::OpenWithApp]) -> Vec<MenuItem> {
    let Some(target) = menu.target.as_ref() else {
        // ------------------------------------------------ 空白处：目录级动作
        // 空白处：目录级动作。两个「新建」同组（中间不隔线）。
        return vec![
            MenuItem::new(MenuAction::NewFolder, "新建文件夹", "", true),
            MenuItem::new(MenuAction::NewFile, "新建文本文件", "", true),
            MenuItem::new(
                MenuAction::Paste,
                "粘贴",
                crate::keys::hint("clipboard.paste"),
                true,
            )
            .separated(),
            MenuItem::new(
                MenuAction::SelectAll,
                "全选",
                crate::keys::hint("select.all"),
                true,
            )
            .separated(),
            MenuItem::new(
                MenuAction::InvertSelection,
                "反选",
                crate::keys::hint("select.invert"),
                true,
            ),
            MenuItem::new(MenuAction::Refresh, "刷新", "", true),
            MenuItem::new(MenuAction::OpenTerminal, "在终端中打开", "", true).separated(),
            MenuItem::new(MenuAction::Properties, "显示简介", "", true),
        ];
    };

    let is_dir = menu.is_dir;
    let multi = menu.selected > 1;
    let mut out = Vec::new();

    // 打开（目录进入；文件用系统默认应用，与双击一致）
    // 提示由键表推导：macOS 是 ⌘↓，Windows / Linux 是 Enter。
    out.push(MenuItem::new(
        MenuAction::Open,
        "打开",
        crate::keys::hint("list.open"),
        true,
    ));
    // 打开方式：仅文件；hover 展开二级菜单（应用列表 + 系统选择对话框）。
    // 候选未就绪（异步查询中 / 无候选）时只剩「选择其他应用…」。
    if !is_dir {
        let mut submenu: Vec<(String, MenuAction)> = open_with
            .iter()
            .enumerate()
            .map(|(i, app)| (app.name.clone(), MenuAction::OpenWith(i)))
            .collect();
        submenu.push(("选择其他应用…".to_string(), MenuAction::OpenWithOther));
        out.push(
            MenuItem::new(MenuAction::OpenWithOther, "打开方式", "▸", true).with_submenu(submenu),
        );
        out.push(MenuItem::new(
            MenuAction::QuickLook,
            "快速查看",
            crate::keys::hint("list.preview"),
            true,
        ));
    }
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
    // 在系统文件管理器里定位：只有**本机**路径有意义（远程条目本机磁盘上不存在，
    // 给了就是点了没反应），且平台要有实现。标签随平台（macOS 叫访达）。
    if !menu.remote && mo_platform::supports_reveal() {
        out.push(MenuItem::new(
            MenuAction::RevealInFileManager,
            mo_platform::reveal_label(),
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
            crate::keys::hint("list.rename"),
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
        crate::keys::hint("file.duplicate"),
        true,
    ));
    out.push(MenuItem::new(
        MenuAction::Copy,
        "复制",
        crate::keys::hint("clipboard.copy"),
        true,
    ));
    out.push(MenuItem::new(
        MenuAction::Cut,
        "剪切",
        crate::keys::hint("clipboard.cut"),
        true,
    ));
    out.push(MenuItem::new(
        MenuAction::CopyPath,
        "拷贝路径",
        crate::keys::hint("clipboard.copy_path"),
        true,
    ));
    // 收集到暂存区：与上面同组（不隔线）——它也是「把这几个文件收起来待用」，
    // 只是收的地方不同（累积清单 vs 一次性剪贴板）。
    out.push(MenuItem::new(
        MenuAction::Stage,
        if multi {
            format!("收集到暂存区（{} 项）", menu.selected)
        } else {
            "收集到暂存区".to_string()
        },
        crate::keys::hint("staging.collect"),
        true,
    ));

    // 删除。只有一个「废纸篓」入口——macOS 生产模式下它就是「系统废纸篓 +
    // Mo 账本」，不再有第二条「移到系统废纸篓」（见 devlog/trash-unify.md）。
    out.push(
        MenuItem::new(
            MenuAction::Trash,
            if multi {
                format!("移到废纸篓（{} 项）", menu.selected)
            } else {
                "移到废纸篓".to_string()
            },
            crate::keys::hint("file.trash"),
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
        crate::keys::hint("file.properties"),
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
/// `submenu_open` 为真时展开「打开方式」的二级菜单（应用列表）。
pub(crate) fn render(
    menu: &ContextMenu,
    items: &[MenuItem],
    viewport: (f32, f32),
    entity: &Entity<crate::RootView>,
    submenu_open: bool,
) -> impl IntoElement {
    let h = panel_height(items);
    // 钳制：先按「放在鼠标右下」算，再保证右边 / 下边不越界（越界就贴边）。
    let x = menu.x.min(viewport.0 - MENU_W - EDGE).max(EDGE);
    let y = menu.y.min(viewport.1 - h - EDGE).max(EDGE);

    // 展开行（「打开方式」）在面板内的纵向偏移，与二级菜单面板的高度。
    let empty_submenu: Vec<(String, MenuAction)> = Vec::new();
    let (sub_row, sub_items) = items
        .iter()
        .enumerate()
        .find(|(_, it)| !it.submenu.is_empty())
        .map(|(i, it)| (row_top(items, i), &it.submenu))
        .unwrap_or((0.0, &empty_submenu));
    let sub_h = sub_items.len() as f32 * ITEM_H + PAD * 2.0;
    // 二级菜单贴主菜单右缘；底边越界时向上收。
    let sub_rel_y = if y + sub_row + sub_h + EDGE > viewport.1 {
        (viewport.1 - EDGE - sub_h - y).max(0.0)
    } else {
        sub_row
    };

    // ⚠️ 主菜单与二级菜单必须包在同一个 wrapper 里：`on_mouse_down_out`
    // 按「鼠标是否在本元素 bounds 内」判定，若各自为政，点二级菜单
    // （在主面板 bounds 外）会把整个菜单关掉。wrapper 的命中区覆盖
    // 两者并集，点空白处（wrapper 外）才关菜单。
    let sub_w = if submenu_open { MENU_W } else { 0.0 };
    let wrap_w = MENU_W + sub_w;
    let wrap_h = if submenu_open {
        h.max(sub_rel_y + sub_h)
    } else {
        h
    };
    let mut wrapper = div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(wrap_w))
        .h(px(wrap_h))
        // 阻止点击穿透到底下的文件行（空隙区域也会被挡住，可接受）。
        .occlude()
        .debug_selector(|| "mo-context-menu-wrap".to_string());
    let out_entity = entity.clone();
    wrapper
        .interactivity()
        .on_mouse_down_out(move |_ev, _window, cx| {
            out_entity.update(cx, |v, cx| v.close_context_menu(cx));
        });

    let mut panel = div()
        .id("mo-context-menu")
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .w(px(MENU_W))
        .flex()
        .flex_col()
        .py(px(PAD))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::divider())
        .rounded(px(PANEL_RADIUS))
        .shadow_lg()
        // 测试用（release no-op）：定位 / 钳制的断言都查这个选择器。
        .debug_selector(|| "mo-context-menu".to_string())
        .text_size(px(13.0))
        .text_color(theme::text());

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
            let has_submenu = !it.submenu.is_empty();
            // 只有贴住面板边缘的那一项（第一项 / 最后一项）需要跟随圆角。
            // 第一项若有前导分隔线，则压在圆角上的其实是那条线，不是这一行。
            let is_first = i == 0 && !it.separator_before;
            let is_last = i + 1 == items.len();
            row = row.hover(move |s| item_hover_bg(s, is_first, is_last));
            // 二级菜单交互（⚠️ 一行只能挂一次 on_hover，两种情况合并处理）：
            // hover 到带子菜单的行展开；hover 到其它行收起。离开主菜单
            // （鼠标进二级菜单）不收起——否则跨面板的间隙会把菜单闪掉；
            // 菜单关闭 / 执行动作时会一并复位。
            let hover_entity = entity.clone();
            row.interactivity().on_hover(move |hovered, _window, cx| {
                if !*hovered {
                    return;
                }
                hover_entity.update(cx, |v, cx| {
                    if has_submenu {
                        v.open_ctx_submenu(cx);
                    } else {
                        v.close_ctx_submenu(cx);
                    }
                });
            });
            // 有子菜单的行只负责展开，点击动作由子项承担。
            if !has_submenu {
                row.interactivity().on_click(move |_ev, _window, cx| {
                    item_entity.update(cx, |v, cx| v.run_menu_action(action, cx));
                });
            }
        }

        panel = panel.child(row);
    }
    wrapper = wrapper.child(panel);

    // 二级菜单面板（「打开方式」应用列表）。
    if submenu_open && !sub_items.is_empty() {
        let mut sub_panel = div()
            .id("mo-context-submenu")
            .absolute()
            .left(px(MENU_W))
            .top(px(sub_rel_y))
            .w(px(MENU_W))
            .flex()
            .flex_col()
            .py(px(PAD))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::divider())
            .rounded(px(PANEL_RADIUS))
            .shadow_lg()
            .occlude()
            .debug_selector(|| "mo-context-submenu".to_string())
            .text_size(px(13.0))
            .text_color(theme::text());

        for (j, (label, action)) in sub_items.iter().enumerate() {
            let is_first = j == 0;
            let is_last = j + 1 == sub_items.len();
            let mut item = div()
                .id(("mo-ctx-subitem", j))
                .flex()
                .flex_row()
                .items_center()
                .h(px(ITEM_H))
                .px(px(10.0))
                .truncate()
                .hover(move |s| item_hover_bg(s, is_first, is_last))
                .child(text!(label.clone()));
            let entity_click = entity.clone();
            let action = *action;
            item.interactivity().on_click(move |ev, _window, cx| {
                // 阻断冒泡：别让 wrapper / 根容器把这次点击当成「关菜单」。
                cx.stop_propagation();
                let _ = ev;
                entity_click.update(cx, |v, cx| v.run_menu_action(action, cx));
            });
            sub_panel = sub_panel.child(item);
        }
        wrapper = wrapper.child(sub_panel);
    }

    wrapper
}

/// 菜单项 hover 时的底色。
///
/// 贴住面板上下边缘的那一项（第一项 / 最后一项）要顺着面板圆角收一下，否则
/// 矩形底色会把圆角「切方」——见 `ITEM_RADIUS` 的说明。
fn item_hover_bg(s: StyleRefinement, is_first: bool, is_last: bool) -> StyleRefinement {
    let s = s.bg(theme::hover_bg());
    match (is_first, is_last) {
        // 只有一项的短菜单：上下都要收。
        (true, true) => s.rounded(px(ITEM_RADIUS)),
        (true, false) => s.rounded_t(px(ITEM_RADIUS)),
        (false, true) => s.rounded_b(px(ITEM_RADIUS)),
        (false, false) => s,
    }
}

/// 第 `index` 个条目在面板内的纵向偏移（含上方分隔线），二级菜单定位用。
fn row_top(items: &[MenuItem], index: usize) -> f32 {
    PAD + items[..index]
        .iter()
        .map(|it| {
            if it.separator_before {
                ITEM_H + SEP_H
            } else {
                ITEM_H
            }
        })
        .sum::<f32>()
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
            remote: false,
        }
    }

    /// 同上，但当前页在**远程**（用来验证本机专属动作被裁掉）。
    #[cfg(target_os = "macos")]
    fn remote_menu(target: Option<&str>, is_dir: bool, selected: usize) -> ContextMenu {
        ContextMenu {
            remote: true,
            ..menu(target, is_dir, selected)
        }
    }

    fn actions(m: &ContextMenu) -> Vec<MenuAction> {
        items(m, &[]).into_iter().map(|i| i.action).collect()
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
                MenuAction::InvertSelection,
                MenuAction::Refresh,
                MenuAction::OpenTerminal,
                MenuAction::Properties,
            ]
        );
    }

    /// 两个「新建」必须挨在一起、中间没有分隔线（同属「新建」这一组）。
    #[test]
    fn new_items_share_one_group() {
        let list = items(&menu(None, true, 0), &[]);
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

    /// 远端页（FTP / SFTP / WebDAV）上**不**出现「在访达中显示」：
    /// 远程条目在本机磁盘上根本不存在，给了就是点了没反应——与「目录判据只问列表
    /// 模型」同一条纪律。
    #[cfg(target_os = "macos")]
    #[test]
    fn remote_page_hides_host_only_actions() {
        let host = actions(&menu(Some("/tmp/a.txt"), false, 1));
        assert!(
            host.contains(&MenuAction::RevealInFileManager),
            "本机页应当能「在访达中显示」"
        );

        let remote = actions(&remote_menu(Some("/pub/a.txt"), false, 1));
        assert!(
            !remote.contains(&MenuAction::RevealInFileManager),
            "远程条目没法在访达里显示"
        );
        // 裁剪只针对这一条：常规动作（复制到剪贴板 / 移到废纸篓）照旧。
        assert!(remote.contains(&MenuAction::Copy));
        assert!(remote.contains(&MenuAction::Trash));
    }

    /// 平台没实现时（这里是不支持的那几个）「在访达中显示」压根不该进菜单——
    /// 列出来点了只会报「不支持」。
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platform_hides_host_only_actions() {
        let a = actions(&menu(Some("/tmp/a.txt"), false, 1));
        assert!(!a.contains(&MenuAction::RevealInFileManager));
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
        let three = items(&menu(Some("/tmp/a.txt"), false, 3), &[]);
        assert!(
            find(&three, MenuAction::Rename).enabled,
            "多项应当能批量重命名"
        );
        assert_eq!(find(&three, MenuAction::Rename).label, "批量重命名…");
        assert!(find(&three, MenuAction::Hash).enabled, "多项应当能算哈希");
        assert!(!find(&three, MenuAction::Compare).enabled, "3 项不能比较");

        let two = items(&menu(Some("/tmp/a.txt"), false, 2), &[]);
        assert!(
            find(&two, MenuAction::Compare).enabled,
            "恰好 2 项时应当可以比较"
        );

        let one = items(&menu(Some("/tmp/a.txt"), false, 1), &[]);
        assert_eq!(find(&one, MenuAction::Rename).label, "重命名…");
    }

    /// 高度要跟着分隔线一起算——钳制用的是它，算错菜单会被切掉一截。
    #[test]
    fn panel_height_accounts_for_separators() {
        let blank = items(&menu(None, true, 0), &[]);
        assert_eq!(blank.len(), 8);
        // 3 条分隔线：paste / select_all / open_terminal 各自上方一条。
        let seps = blank.iter().filter(|i| i.separator_before).count();
        assert_eq!(seps, 3);
        assert_eq!(panel_height(&blank), PAD * 2.0 + ITEM_H * 8.0 + SEP_H * 3.0);
    }

    /// 文件菜单：第一项是「打开」（系统默认应用，同双击），带「打开方式」
    /// 二级菜单（候选应用 + 系统选择对话框），「快速查看」仍在但不再占首位。
    #[test]
    fn file_menu_has_open_and_open_with_submenu() {
        let list = items(&menu(Some("/tmp/a.txt"), false, 1), &[]);
        let open = find(&list, MenuAction::Open);
        assert_eq!(open.label, "打开");

        let ow = find(&list, MenuAction::OpenWithOther);
        assert_eq!(ow.label, "打开方式");
        assert_eq!(ow.hint, "▸");
        // 无候选时二级菜单也必须有「选择其他应用…」兜底。
        assert_eq!(
            ow.submenu.last().map(|(l, _)| l.as_str()),
            Some("选择其他应用…")
        );

        // 候选应用按序号进入二级菜单。
        let apps = vec![
            mo_app::shell::OpenWithApp {
                name: "记事本".to_string(),
                progid: "txtfile".to_string(),
            },
            mo_app::shell::OpenWithApp {
                name: "写字板".to_string(),
                progid: "AppXxyz".to_string(),
            },
        ];
        let list = items(&menu(Some("/tmp/a.txt"), false, 1), &apps);
        let ow = find(&list, MenuAction::OpenWithOther);
        assert_eq!(ow.submenu.len(), 3);
        assert_eq!(
            ow.submenu[0],
            ("记事本".to_string(), MenuAction::OpenWith(0))
        );
        assert_eq!(
            ow.submenu[1],
            ("写字板".to_string(), MenuAction::OpenWith(1))
        );
        assert!(list.iter().any(|i| i.action == MenuAction::QuickLook));
        assert_ne!(
            list.first().map(|i| i.action),
            Some(MenuAction::QuickLook),
            "「快速查看」不该再是文件菜单第一项"
        );
    }

    /// 目录菜单不出现「打开方式 / 快速查看」。
    #[test]
    fn directory_menu_has_no_open_with() {
        let a = actions(&menu(Some("/tmp/dir"), true, 1));
        assert!(!a.contains(&MenuAction::OpenWithOther));
        assert!(!a.contains(&MenuAction::QuickLook));
        assert!(!a.contains(&MenuAction::OpenWith(0)));
    }
}
