use std::path::PathBuf;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_app::AppState;
use mo_core::{RenameSpec, SortKey};
use mo_operations::{HashAlgo, OperationHandle, TrashEntry};
use mo_preview::Preview;
use mo_search::SearchHit;

use crate::dialogs;
use crate::listing;
use crate::panel::{ColumnData, Pane, Panel, ViewMode};
use crate::{columns, file_list, grid, progress_panel, sidebar, status_bar, theme, toolbar};

/// 首次同步时抓取的窗口大小。
pub(crate) const INITIAL_WINDOW: usize = 200;

/// 侧边栏宽度：网格视图用它推算每个窗格的可用宽度。
const SIDEBAR_WIDTH: f32 = 188.0;

/// 地址栏编辑态的占位文字（空输入时显示）。
pub(crate) const ADDRESS_PLACEHOLDER: &str = "输入路径，回车跳转";

/// 当前打开的模态层（占用中央区；Esc 关闭）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Modal {
    None,
    /// 命令面板。
    CommandPalette,
    /// 全局搜索。
    GlobalSearch,
    /// 快速预览（空格 Quick Look）。
    QuickLook,
    /// 回收站面板。
    Trash,
    /// 文件 / 文件夹比较结果。
    Diff,
    /// 属性与权限。
    Properties,
    /// 批量重命名。
    BatchRename,
    /// 压缩（输入目标文件名）。
    Archive,
    /// 磁盘空间分析。
    DiskUsage,
    /// 文件标签。
    Tags,
    /// 纯文本信息（哈希结果 / 提示）。
    Info(String),
}

/// 命令面板中的可执行命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandId {
    Refresh,
    Back,
    Forward,
    Parent,
    SelectAll,
    ClearSelection,
    DeleteSelection,
    SortName,
    SortSize,
    SortModified,
    SortKind,
    IndexCurrent,
    StopIndexing,
    OpenGlobalSearch,
    QuickLook,
    HashSelection,
    CompareSelection,
    Undo,
    Redo,
    OpenTrash,
    OpenTerminal,
    AddBookmark,
    RemoveBookmark,
    Properties,
    BatchRename,
    CreateArchive,
    ExtractArchive,
    DiskUsage,
    TagSelection,
    CopyClipboard,
    CutClipboard,
    PasteClipboard,
    NewTab,
    CloseTab,
    ToggleSplit,
    CreateSymlink,
    CreateHardlink,
}

struct CmdDef {
    id: CommandId,
    title: &'static str,
    category: &'static str,
}

/// 命令目录（命令面板的数据源）。
fn commands() -> Vec<CmdDef> {
    vec![
        CmdDef {
            id: CommandId::OpenGlobalSearch,
            title: "全局搜索…",
            category: "搜索",
        },
        CmdDef {
            id: CommandId::QuickLook,
            title: "快速预览（Quick Look）",
            category: "预览",
        },
        CmdDef {
            id: CommandId::HashSelection,
            title: "计算选中文件哈希（MD5/SHA-1/SHA-256）",
            category: "工具",
        },
        CmdDef {
            id: CommandId::CompareSelection,
            title: "比较选中的两项（文件 / 文件夹，含 diff）",
            category: "工具",
        },
        CmdDef {
            id: CommandId::IndexCurrent,
            title: "索引当前目录（建立全局搜索索引）",
            category: "搜索",
        },
        CmdDef {
            id: CommandId::StopIndexing,
            title: "停止索引",
            category: "搜索",
        },
        CmdDef {
            id: CommandId::Refresh,
            title: "刷新",
            category: "导航",
        },
        CmdDef {
            id: CommandId::Back,
            title: "后退",
            category: "导航",
        },
        CmdDef {
            id: CommandId::Forward,
            title: "前进",
            category: "导航",
        },
        CmdDef {
            id: CommandId::Parent,
            title: "上级目录",
            category: "导航",
        },
        CmdDef {
            id: CommandId::SelectAll,
            title: "全选",
            category: "选择",
        },
        CmdDef {
            id: CommandId::ClearSelection,
            title: "清除选择",
            category: "选择",
        },
        CmdDef {
            id: CommandId::DeleteSelection,
            title: "删除选中",
            category: "操作",
        },
        CmdDef {
            id: CommandId::Undo,
            title: "撤销",
            category: "操作",
        },
        CmdDef {
            id: CommandId::Redo,
            title: "重做",
            category: "操作",
        },
        CmdDef {
            id: CommandId::OpenTrash,
            title: "回收站…",
            category: "操作",
        },
        CmdDef {
            id: CommandId::OpenTerminal,
            title: "在当前目录打开终端",
            category: "工具",
        },
        CmdDef {
            id: CommandId::AddBookmark,
            title: "把当前目录加入书签",
            category: "导航",
        },
        CmdDef {
            id: CommandId::RemoveBookmark,
            title: "把当前目录从书签移除",
            category: "导航",
        },
        CmdDef {
            id: CommandId::Properties,
            title: "属性与权限…（⌘I）",
            category: "工具",
        },
        CmdDef {
            id: CommandId::BatchRename,
            title: "批量重命名…",
            category: "工具",
        },
        CmdDef {
            id: CommandId::CreateArchive,
            title: "压缩选中项…",
            category: "工具",
        },
        CmdDef {
            id: CommandId::ExtractArchive,
            title: "解压到当前目录",
            category: "工具",
        },
        CmdDef {
            id: CommandId::DiskUsage,
            title: "磁盘空间分析…",
            category: "工具",
        },
        CmdDef {
            id: CommandId::TagSelection,
            title: "给选中项设置标签…",
            category: "工具",
        },
        CmdDef {
            id: CommandId::CopyClipboard,
            title: "复制选中（⌘C）",
            category: "操作",
        },
        CmdDef {
            id: CommandId::CutClipboard,
            title: "剪切选中（⌘X）",
            category: "操作",
        },
        CmdDef {
            id: CommandId::PasteClipboard,
            title: "粘贴到当前目录（⌘V）",
            category: "操作",
        },
        CmdDef {
            id: CommandId::NewTab,
            title: "新建标签页（⌘T）",
            category: "窗口",
        },
        CmdDef {
            id: CommandId::CloseTab,
            title: "关闭标签页（⌘W）",
            category: "窗口",
        },
        CmdDef {
            id: CommandId::ToggleSplit,
            title: "双栏分栏开 / 关（⌘⇧D）",
            category: "窗口",
        },
        CmdDef {
            id: CommandId::CreateSymlink,
            title: "创建符号链接（同目录）",
            category: "操作",
        },
        CmdDef {
            id: CommandId::CreateHardlink,
            title: "创建硬链接（同目录，仅文件）",
            category: "操作",
        },
        CmdDef {
            id: CommandId::SortName,
            title: "按名称排序",
            category: "排序",
        },
        CmdDef {
            id: CommandId::SortSize,
            title: "按大小排序",
            category: "排序",
        },
        CmdDef {
            id: CommandId::SortModified,
            title: "按修改时间排序",
            category: "排序",
        },
        CmdDef {
            id: CommandId::SortKind,
            title: "按类型排序",
            category: "排序",
        },
    ]
}

/// 按查询串过滤命令（大小写不敏感子串匹配）。
fn filtered_commands(q: &str) -> Vec<CommandId> {
    let q = q.trim().to_lowercase();
    commands()
        .into_iter()
        .filter(|c| {
            q.is_empty()
                || c.title.to_lowercase().contains(&q)
                || c.category.to_lowercase().contains(&q)
        })
        .map(|c| c.id)
        .collect()
}

/// 根视图：组合 toolbar / (sidebar + 窗格) / 进度面板 / status bar。
///
/// **每个窗格里的每个标签页都是一份独立的浏览状态**（自己的 `AppState`
/// 与窗口快照）——见 [`crate::panel`]。多标签页 / 分栏因此不需要让
/// `mo-app` 支持「多会话」：独立性由构造保证，读写路径与单目录完全一致。
///
/// UI **不持有整份目录**：每个面板只持有可见区窗口，滚动到哪取哪。
pub struct RootView {
    /// 窗格列表：默认 1 个；分栏时 2 个。每个窗格含 1..N 个标签页。
    pub(crate) panes: Vec<Pane>,
    /// 当前焦点窗格下标。
    pub(crate) active_pane: usize,
    /// 是否显示第二个窗格（双栏分栏视图）。
    pub(crate) split: bool,
    /// 键盘焦点：没有它收不到按键事件。
    focus: FocusHandle,
    /// 当前模态层（全局：一次只显示一个）。
    modal: Modal,
    /// 命令面板过滤词。
    cmd_query: String,
    /// 命令面板 / 搜索结果的高亮下标。
    palette_index: usize,
    /// 全局搜索过滤词。
    search_query: String,
    /// 全局搜索结果。
    search_results: Vec<SearchHit>,
    /// 快速预览缓存。
    preview_cache: Option<Preview>,
    /// 已索引文件数（状态栏展示，取最近一次同步的值）。
    indexed: usize,
    /// 回收站条目快照（回收站面板数据源）。
    trash_entries: Vec<TrashEntry>,
    /// 比较 / diff 结果缓存（比较模态数据源）。
    diff_cache: Option<mo_diff::Comparison>,
    /// 模态内表单的当前字段下标。
    pub(crate) form_index: usize,
    /// 属性面板的可编辑状态。
    pub(crate) prop: Option<crate::dialogs::PropEdit>,
    /// 批量重命名规则（与选中项一起构成预览）。
    pub(crate) rename_spec: RenameSpec,
    /// 批量重命名 / 压缩的选中项快照。
    pub(crate) rename_paths: Vec<PathBuf>,
    /// 压缩对话框里的目标文件名。
    pub(crate) archive_name: String,
    /// 磁盘空间分析结果。
    pub(crate) usage: Vec<mo_app::DirUsage>,
    /// 进行中的拖拽（鼠标按下时记录、抬起时结算）。
    pub(crate) drag: Option<DragState>,
    /// 列表视图的列布局（顺序 + 宽度），表头与数据行共用。
    pub(crate) cols: crate::list_columns::ColumnLayout,
    /// 进行中的表头操作（调宽 / 调序）。
    pub(crate) header_drag: Option<HeaderDrag>,
    /// 表头各列的**真实** bounds（prepaint 回写），拖动落点判定用。
    pub(crate) header_cells: Vec<(crate::list_columns::ColId, Bounds<Pixels>)>,
    /// `header_cells` 属于哪个 (窗格, 标签页)——分栏时避免用错窗格的 bounds。
    pub(crate) header_cells_owner: (usize, usize),
    /// 右键上下文菜单（一次只开一个；`None` = 关闭）。
    pub(crate) context_menu: Option<crate::context_menu::ContextMenu>,
}

/// 一次表头拖动：要么在调列宽，要么在调列序。
pub(crate) enum HeaderDrag {
    /// 拖列**分隔线**（画在 `col` 的左缘）。
    ///
    /// `anchor` 是按下那一刻两侧列的宽度快照：拖动中按「相对按下点的总位移」
    /// 一次性重算两侧宽度，而不是逐帧增量——增量叠加钳制会把分隔线拖偏
    /// （触到宽度上下限后继续拖，再松回来时线回不到鼠标下）。
    Resizing {
        col: crate::list_columns::ColId,
        start_x: f32,
        anchor: crate::list_columns::DividerAnchor,
    },
    /// 拖列头本身：`moved` 为真才算拖列，否则抬起时按点击（切换排序）处理。
    Reordering {
        pane: usize,
        tab: usize,
        col: crate::list_columns::ColId,
        start_x: f32,
        cur_x: f32,
        moved: bool,
    },
}

/// 一次拖拽：从哪个窗格的哪个标签页拖出了哪些路径。
#[derive(Clone)]
pub(crate) struct DragState {
    pub(crate) pane: usize,
    pub(crate) tab: usize,
    pub(crate) paths: Vec<PathBuf>,
}

/// 「新建」的种类（两种新建流程一致，只有名字与调用的 app 方法不同）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NewEntry {
    Folder,
    File,
}

impl RootView {
    pub fn new(app: AppState, cx: &mut Context<Self>) -> Self {
        let view = Self {
            panes: vec![Pane::new(Panel::new(app.clone()))],
            active_pane: 0,
            split: false,
            focus: cx.focus_handle(),
            modal: Modal::None,
            cmd_query: String::new(),
            palette_index: 0,
            search_query: String::new(),
            search_results: Vec::new(),
            preview_cache: None,
            indexed: 0,
            trash_entries: Vec::new(),
            diff_cache: None,
            form_index: 0,
            prop: None,
            rename_spec: RenameSpec::default(),
            rename_paths: Vec::new(),
            archive_name: String::new(),
            usage: Vec::new(),
            drag: None,
            cols: crate::list_columns::ColumnLayout::new(),
            header_drag: None,
            header_cells: Vec::new(),
            header_cells_owner: (0, 0),
            context_menu: None,
        };

        let weak = cx.entity().downgrade();
        cx.spawn(async move |_weak, cx| {
            tab_loop(app, weak, cx, 0, 0, true).await;
        })
        .detach();

        view
    }

    // ------------------------------------------------------------ 面板访问

    /// 当前焦点窗格。
    pub(crate) fn pane(&self) -> &Pane {
        &self.panes[self.active_pane.min(self.panes.len() - 1)]
    }

    /// 当前焦点窗格的可变引用。
    pub(crate) fn pane_mut(&mut self) -> &mut Pane {
        let i = self.active_pane.min(self.panes.len() - 1);
        &mut self.panes[i]
    }

    /// 当前焦点标签页（空窗格不会发生，这里是安全的）。
    pub(crate) fn panel(&self) -> &Panel {
        self.pane().panel()
    }

    /// 当前焦点标签页的可变引用。
    pub(crate) fn panel_mut(&mut self) -> &mut Panel {
        self.pane_mut().panel_mut()
    }

    /// 指定 `(窗格, 标签页)` 的面板；下标失效（标签页已关闭）返回 `None`。
    ///
    /// 后台同步任务活得比标签页久，必须靠这个返回值判断「目标还在不在」，
    /// 否则关闭标签页后任务继续执行会写坏数据。
    pub(crate) fn panel_at(&self, pane: usize, tab: usize) -> Option<&Panel> {
        let p = self.panes.get(pane)?;
        p.tabs.get(tab)
    }

    /// 同 [`Self::panel_at`] 的可变版本。
    pub(crate) fn panel_at_mut(&mut self, pane: usize, tab: usize) -> Option<&mut Panel> {
        let p = self.panes.get_mut(pane)?;
        p.tabs.get_mut(tab)
    }

    /// 当前焦点标签页的 `AppState`（所有面板级操作的入口）。
    pub(crate) fn app(&self) -> AppState {
        self.panel().app.clone()
    }

    /// 是否存在指定面板（后台任务判断目标是否还存活用）。
    pub(crate) fn has_panel(&self, pane: usize, tab: usize) -> bool {
        self.panel_at(pane, tab).is_some()
    }

    /// 全部窗格的操作快照汇总（后台操作可能由任意窗格发起）。
    pub(crate) fn all_ops(&self) -> Vec<OperationHandle> {
        let mut out = Vec::new();
        for pane in &self.panes {
            for tab in &pane.tabs {
                out.extend(tab.ops.iter().cloned());
            }
        }
        out
    }

    // ------------------------------------------------------------ 标签页 / 分栏

    /// 在 `pane_idx` 新建一个标签页并切到它。
    ///
    /// 每个标签页持有独立的 `AppState`：导航栈、选择、滚动位置天然隔离。
    pub(crate) fn new_tab(&mut self, cx: &mut Context<Self>, pane_idx: usize) {
        let app = AppState::new();
        // 每个 AppState 需要自己的监听泵 / 刷新泵（ watcher 进程级、runtime 共享）。
        app.spawn_watcher_pump();
        app.spawn_refresh_pump();
        let Some(pane) = self.panes.get_mut(pane_idx) else {
            return;
        };
        let tab_idx = pane.tabs.len();
        pane.tabs.push(Panel::new(app.clone()));
        pane.active = tab_idx;

        let weak = cx.entity().downgrade();
        cx.spawn(async move |_weak, cx| {
            tab_loop(app, weak, cx, pane_idx, tab_idx, true).await;
        })
        .detach();
    }

    /// 关闭标签页。
    ///
    /// 返回 `true` 表示已触发退出应用（关掉的是整个应用最后一个标签页）。
    /// 单窗格下关掉唯一标签页 → 退出；分栏下某窗格还有别的标签页时，不允许把该窗格清空。
    pub(crate) fn close_tab(&mut self, pane_idx: usize, tab_idx: usize, cx: &mut App) -> bool {
        let Some(pane) = self.panes.get_mut(pane_idx) else {
            return false;
        };
        if pane.tabs.len() <= 1 {
            // 这是该窗格最后一个标签页：仅在「整个应用再无其它标签页」时退出应用。
            let total: usize = self.panes.iter().map(|p| p.tabs.len()).sum();
            if total <= 1 {
                cx.quit();
                return true;
            }
            return false;
        }
        pane.tabs.remove(tab_idx);
        pane.active = pane.active.min(pane.tabs.len() - 1);
        false
    }

    /// 切入指定窗格的指定标签页（顺带把该窗格设为焦点）。
    pub(crate) fn switch_tab(&mut self, pane_idx: usize, tab_idx: usize) {
        let Some(pane) = self.panes.get_mut(pane_idx) else {
            return;
        };
        if tab_idx < pane.tabs.len() {
            pane.active = tab_idx;
            self.active_pane = pane_idx;
        }
    }

    /// 当前窗格内的上一个 / 下一个标签页（环绕）。
    pub(crate) fn cycle_tab(&mut self, step: isize) {
        let idx = self.active_pane.min(self.panes.len() - 1);
        let len = self.panes.get(idx).map(|p| p.tabs.len()).unwrap_or(1);
        let cur = self.panes.get(idx).map(|p| p.active).unwrap_or(0);
        let next = if step < 0 {
            (cur + len - 1) % len.max(1)
        } else {
            (cur + 1) % len.max(1)
        };
        self.switch_tab(idx, next);
    }

    /// 开启 / 关闭双栏分栏：第二窗格按需创建或销毁。
    ///
    /// `start` 为 `Some(path)` 时第二窗格直接从该目录起步（右键「在分栏中打开」），
    /// 为 `None` 时仍从 Home 起步。
    pub(crate) fn toggle_split(&mut self, cx: Option<&mut Context<Self>>, start: Option<PathBuf>) {
        self.split = !self.split;
        if self.split && self.panes.len() < 2 {
            let Some(cx) = cx else {
                self.split = false;
                return;
            };
            let app = AppState::new();
            app.spawn_watcher_pump();
            app.spawn_refresh_pump();
            let idx = self.panes.len();
            self.panes.push(Pane::new(Panel::new(app.clone())));
            let weak = cx.entity().downgrade();
            cx.spawn(async move |_weak, cx| {
                match start {
                    // 先打开目标目录再起同步循环，否则首帧会先闪一次 Home。
                    Some(path) => {
                        let _ = mo_app::DirectoryController::new(app.clone())
                            .open(&path)
                            .await;
                        tab_loop(app, weak, cx, idx, 0, false).await;
                    }
                    // 第二窗格默认从 Home 起步。
                    None => tab_loop(app, weak, cx, idx, 0, true).await,
                }
            })
            .detach();
        } else if !self.split && self.panes.len() > 1 {
            self.panes.truncate(1);
            self.active_pane = 0;
        }
    }

    /// 在 `pane_idx` 新建一个标签页，并**直接打开 `path`**（而不是 Home）。
    ///
    /// 与 [`Self::new_tab`] 的唯一区别是初始目录：多标签页的「在新标签页中打开」。
    pub(crate) fn open_in_new_tab(
        &mut self,
        pane_idx: usize,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let app = AppState::new();
        app.spawn_watcher_pump();
        app.spawn_refresh_pump();
        let Some(pane) = self.panes.get_mut(pane_idx) else {
            return;
        };
        let tab_idx = pane.tabs.len();
        pane.tabs.push(Panel::new(app.clone()));
        pane.active = tab_idx;

        let weak = cx.entity().downgrade();
        cx.spawn(async move |_weak, cx| {
            let _ = mo_app::DirectoryController::new(app.clone())
                .open(&path)
                .await;
            tab_loop(app, weak, cx, pane_idx, tab_idx, false).await;
        })
        .detach();
    }

    /// 切焦点窗格（分栏时才有意义）。
    pub(crate) fn switch_pane(&mut self, step: isize) {
        if self.panes.len() < 2 {
            return;
        }
        let len = self.panes.len();
        let cur = self.active_pane.min(len - 1);
        self.active_pane = if step < 0 {
            (cur + len - 1) % len
        } else {
            (cur + 1) % len
        };
    }

    // ------------------------------------------------------------ 面板动作

    /// 进入地址栏编辑态：预填当前完整路径，聚焦并**全选**。
    ///
    /// 全选是 Finder / Win11 的行为：点一下地址栏就能直接敲新路径覆盖旧的，
    /// 不必先自己选中（也顺手覆盖了「编辑时想选中文本」这个诉求）。
    ///
    /// 输入框用框架的 [`InputState`]（选区 / 光标 / 剪贴板 / 输入法都由它提供），
    /// 且**一个面板一个**：分栏时两个窗格的地址栏互不串味。
    pub fn begin_address_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .panel()
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // 懒创建：`Panel` 构造时拿不到 `window`，第一次进入编辑态才建。
        if self.panel().address.is_none() {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(ADDRESS_PLACEHOLDER));
            let sub = cx.subscribe_in(
                &state,
                window,
                |this: &mut Self, state, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { .. } => this.submit_address(state.clone(), window, cx),
                    // 点到别处（文件行 / 侧边栏）就当放弃这次编辑。
                    InputEvent::Blur => this.end_address_edit(cx),
                    InputEvent::Change | InputEvent::Focus => {}
                },
            );
            let p = self.panel_mut();
            p.address = Some(state);
            p.address_sub = Some(sub);
        }

        let state = self.panel().address.clone();
        self.panel_mut().address_editing = true;
        if let Some(state) = state {
            state.update(cx, |s, cx| {
                s.set_value(text, window, cx);
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        }
        cx.notify();
    }

    /// 退出地址栏编辑态。输入状态实体留着，下次进入复用（不重建、不丢历史）。
    pub(crate) fn end_address_edit(&mut self, cx: &mut Context<Self>) {
        if self.panel().address_editing {
            self.panel_mut().address_editing = false;
            cx.notify();
        }
    }

    /// 地址栏回车：按输入的路径跳转，并把键盘焦点还给根视图。
    ///
    /// 焦点交还很重要：输入框一直握着焦点的话，上下键 / 输入即过滤这些
    /// 列表快捷键就一直收不到。
    fn submit_address(
        &mut self,
        state: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = state.read(cx).value().trim().to_string();
        let app = self.panel().app.clone();
        self.panel_mut().address_editing = false;
        window.focus(&self.focus, cx);
        cx.notify();
        if text.is_empty() {
            return;
        }
        let target = PathBuf::from(text);
        cx.spawn(async move |_weak, _cx| {
            if let Err(e) = app.open_directory(&target).await {
                eprintln!("打开失败: {e}");
            }
        })
        .detach();
    }

    /// 打开一个条目（双击 / Enter 同语义）：目录进入，文件快速预览。
    pub(crate) fn open_entry(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if path.is_dir() {
            let app = self.app();
            cx.spawn(async move |_weak, _cx| {
                let _ = app.open_directory(&path).await;
            })
            .detach();
        } else {
            match self.app().preview(&path) {
                Ok(pv) => {
                    self.preview_cache = Some(pv);
                    self.modal = Modal::QuickLook;
                }
                Err(e) => {
                    self.modal = Modal::Info(format!("无法预览 {path:?}：{e}"));
                }
            }
            cx.notify();
        }
    }

    // ------------------------------------------------------------ 列视图

    /// 所有处于列视图的标签页：根列缺失或目录已变化时触发加载。
    ///
    /// 每帧调用，但只在「当前目录 ≠ 根列」且没有在途任务时才真正读盘。
    fn ensure_columns(&mut self, cx: &mut Context<Self>) {
        let mut todo: Vec<(usize, usize, PathBuf)> = Vec::new();
        for (pi, pane) in self.panes.iter().enumerate() {
            for (ti, tab) in pane.tabs.iter().enumerate() {
                if tab.view_mode != ViewMode::Columns || tab.column_busy {
                    continue;
                }
                let Some(path) = tab.path.clone() else {
                    continue;
                };
                let stale = tab.columns.first().map(|c| c.path != path).unwrap_or(true);
                if stale {
                    todo.push((pi, ti, path));
                }
            }
        }
        for (pi, ti, path) in todo {
            self.load_column(cx, path, None, pi, ti);
        }
    }

    /// 加载一列并作为当前最深列；`parent` 为 `Some(i)` 时先截掉更深的列。
    pub(crate) fn load_column(
        &mut self,
        cx: &mut Context<Self>,
        path: PathBuf,
        parent: Option<usize>,
        pane: usize,
        tab: usize,
    ) {
        let Some(app) = self.panel_at(pane, tab).map(|p| p.app.clone()) else {
            return;
        };
        if let Some(p) = self.panel_at_mut(pane, tab) {
            if p.column_busy {
                return;
            }
            p.column_busy = true;
        }
        let _ = parent;
        cx.spawn(async move |weak, cx| {
            let entries = app.list_dir(&path).await.unwrap_or_default();
            let _ = weak.update(cx, |v, cx| {
                let Some(p) = v.panel_at_mut(pane, tab) else {
                    return;
                };
                p.column_busy = false;
                if let Some(i) = parent {
                    p.columns.truncate(i + 1);
                }
                p.columns.push(ColumnData {
                    path: path.clone(),
                    entries,
                    cursor: 0,
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// 设置某列的高亮行。
    pub(crate) fn set_column_cursor(&mut self, pane: usize, tab: usize, column: usize, row: usize) {
        if let Some(p) = self.panel_at_mut(pane, tab) {
            if let Some(c) = p.columns.get_mut(column) {
                c.cursor = row;
            }
        }
    }

    /// 打开属性面板：取当前聚焦 / 选中项的名称与权限。
    /// 打开「显示简介」。
    ///
    /// `target` 为 `Some` 时直接用它（右键菜单对着某个条目 / 目录）；
    /// 为 `None` 时沿用旧行为：选中项的第一个，没有选中则当前目录。
    pub(crate) fn open_properties(&mut self, cx: &mut Context<Self>, target: Option<PathBuf>) {
        let app = self.app();
        let Some(path) = self.panel().path.clone() else {
            return;
        };
        let target = target.unwrap_or_else(|| {
            self.panel()
                .selection
                .selected_ids()
                .iter()
                .next()
                .and_then(|id| self.panel().window.iter().find(|e| e.id == *id))
                .map(|e| e.path.clone())
                .unwrap_or(path.clone())
        });
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let (size, mode) = match std::fs::metadata(&target) {
            Ok(m) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    (m.len(), m.mode() & 0o777)
                }
                #[cfg(not(unix))]
                {
                    (m.len(), 0)
                }
            }
            Err(_) => (0, 0o644),
        };
        let info = format!(
            "路径：{}
大小：{}",
            target.display(),
            crate::file_item::format_size(size)
        );
        self.prop = Some(crate::dialogs::PropEdit {
            path: target,
            name,
            mode,
            bit: 0,
            info,
        });
        self.form_index = 0;
        self.modal = Modal::Properties;
        let _ = app;
        cx.notify();
    }

    /// 打开批量重命名：`paths` 为 `None` 时快照当前选中的路径。
    ///
    /// 右键菜单会直接传它那一份路径快照——菜单打开时发出的异步选中未必已经
    /// 落到 `app` 上，而这里要立刻开模态，等不起。
    pub(crate) fn open_batch_rename(
        &mut self,
        cx: &mut Context<Self>,
        paths: Option<Vec<PathBuf>>,
    ) {
        let Some(paths) = paths else {
            let app = self.app();
            let this = cx.entity().clone();
            cx.spawn(async move |_, cx| {
                let paths = app.selection_paths().await;
                this.update(cx, |v, cx| v.begin_batch_rename(paths, cx));
            })
            .detach();
            return;
        };
        self.begin_batch_rename(paths, cx);
    }

    fn begin_batch_rename(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            self.modal = Modal::Info("没有选中文件".to_string());
        } else {
            self.rename_paths = paths;
            self.rename_spec = RenameSpec::default();
            self.form_index = 0;
            self.modal = Modal::BatchRename;
        }
        cx.notify();
    }

    /// 打开压缩对话框（`paths` 语义同 [`Self::open_batch_rename`]）。
    pub(crate) fn open_archive(&mut self, cx: &mut Context<Self>, paths: Option<Vec<PathBuf>>) {
        let Some(paths) = paths else {
            let app = self.app();
            let this = cx.entity().clone();
            cx.spawn(async move |_, cx| {
                let paths = app.selection_paths().await;
                this.update(cx, |v, cx| v.begin_archive(paths, cx));
            })
            .detach();
            return;
        };
        self.begin_archive(paths, cx);
    }

    fn begin_archive(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            self.modal = Modal::Info("没有选中文件".to_string());
        } else {
            let hint = paths
                .first()
                .and_then(|p| p.file_stem())
                .map(|s| format!("{}.zip", s.to_string_lossy()))
                .unwrap_or_else(|| "archive.zip".to_string());
            self.rename_paths = paths;
            self.archive_name = hint;
            self.modal = Modal::Archive;
        }
        cx.notify();
    }

    /// 解压选中的归档到当前目录。
    pub(crate) fn extract_selected(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_, cx| {
            let paths = app.selection_paths().await;
            let dest = app.current_path().await;
            let Some(dest) = dest else { return };
            let mut ok = 0usize;
            let mut failed = 0usize;
            for p in paths {
                match app.extract_archive(p, dest.clone()).await {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        failed += 1;
                        tracing::warn!("解压失败：{e}");
                    }
                }
            }
            this.update(cx, |v, cx| {
                v.modal = Modal::Info(format!("解压完成：成功 {ok} 个，失败 {failed} 个"));
                cx.notify();
            });
        })
        .detach();
    }

    /// 磁盘空间分析：统计 `root`（缺省为当前目录）下每个子项的大小。
    ///
    /// 右键菜单对着某个目录时会传入**那个目录**，而不是当前浏览的目录。
    pub(crate) fn analyze_disk_usage(&mut self, cx: &mut Context<Self>, root: Option<PathBuf>) {
        let app = self.app();
        let Some(root) = root.or_else(|| self.panel().path.clone()) else {
            return;
        };
        self.usage.clear();
        self.form_index = 0;
        self.modal = Modal::DiskUsage;
        let this = cx.entity().clone();
        cx.spawn(async move |_, cx| {
            let usage = app.analyze_usage(root).await.unwrap_or_default();
            this.update(cx, |v, cx| {
                v.usage = usage;
                cx.notify();
            });
        })
        .detach();
    }

    /// 给选中项设置颜色标签（空串表示清除）。
    pub(crate) fn apply_tag(&mut self, color: String) {
        let app = self.app();
        let tags = app.tags();
        let paths: Vec<PathBuf> = self
            .panel()
            .selection
            .selected_ids()
            .iter()
            .filter_map(|id| self.panel().window.iter().find(|e| e.id == *id))
            .map(|e| e.path.clone())
            .collect();
        let paths = if paths.is_empty() {
            self.panel()
                .path
                .clone()
                .map(|p| vec![p])
                .unwrap_or_default()
        } else {
            paths
        };
        for p in paths {
            app.set_tag(p, color.clone());
        }
        let _ = tags;
    }

    /// 打开标签选择面板。
    pub(crate) fn open_tags(&mut self, cx: &mut Context<Self>) {
        self.form_index = 0;
        self.modal = Modal::Tags;
        cx.notify();
    }

    // ------------------------------------------------------------ 拖拽

    /// 鼠标在某个文件行上按下：记录拖拽源。
    ///
    /// 行本身已被选中且是多重选择时拖整个选中集合，否则只拖这一行。
    pub(crate) fn begin_drag(
        &mut self,
        pane: usize,
        tab: usize,
        path: PathBuf,
        id: mo_core::FileId,
    ) {
        let Some(p) = self.panel_at(pane, tab) else {
            return;
        };
        let multiple = p.selection.count() > 1 && p.selection.is_selected(&id);
        let paths = if multiple {
            p.window
                .iter()
                .filter(|e| p.selection.is_selected(&e.id))
                .map(|e| e.path.clone())
                .collect()
        } else {
            vec![path]
        };
        self.drag = Some(DragState { pane, tab, paths });
    }

    // ------------------------------------------------------- 列表表头交互

    /// prepaint 回写：记下这个窗格 / 标签页表头各列的真实 bounds。
    pub(crate) fn set_header_cells(
        &mut self,
        pane: usize,
        tab: usize,
        cells: Vec<(crate::list_columns::ColId, Bounds<Pixels>)>,
    ) {
        // 分栏时两个窗格都会回写：只认最后画的那个，落点判定前先核对归属。
        self.header_cells = cells;
        self.header_cells_owner = (pane, tab);
    }

    /// 在列头上按下：准备拖列（抬起时若没移动，就当成点击 → 切换排序）。
    ///
    /// 已有拖拽态（分隔条先写入了 `Resizing`）时不覆盖——分隔条是列头的子节点，
    /// 事件内层先派发。
    pub(crate) fn header_mouse_down_cell(
        &mut self,
        pane: usize,
        tab: usize,
        col: crate::list_columns::ColId,
        x: f32,
    ) {
        if self.header_drag.is_none() {
            self.header_drag = Some(HeaderDrag::Reordering {
                pane,
                tab,
                col,
                start_x: x,
                cur_x: x,
                moved: false,
            });
        }
    }

    /// 在列**分隔线**上按下：开始调列宽。
    ///
    /// 分隔线画在某一列的左缘，`col` 是它**右侧**的那一列；两侧的起始宽度由
    /// [`crate::list_columns::ColumnLayout::divider_anchor`] 取（弹性列不参与，
    /// 排在第一位的列没有分隔线 → 不会进入这里，因为那样也渲染不出把手）。
    pub(crate) fn header_mouse_down_divider(&mut self, col: crate::list_columns::ColId, x: f32) {
        let Some(anchor) = self.cols.divider_anchor(col) else {
            return;
        };
        self.header_drag = Some(HeaderDrag::Resizing {
            col,
            start_x: x,
            anchor,
        });
    }

    /// 表头内鼠标移动：调列宽即时生效；拖列只更新「是否真的移动了」。
    ///
    /// 返回是否需要重绘（调列宽要重绘，分隔线才会跟着鼠标走）。
    pub(crate) fn header_mouse_move(&mut self, x: f32) -> bool {
        match &mut self.header_drag {
            Some(HeaderDrag::Resizing {
                start_x, anchor, ..
            }) => {
                let (sx, anchor) = (*start_x, *anchor);
                let widths = crate::list_columns::divider_resize(anchor, x - sx);
                self.cols.set_widths(&widths);
                true
            }
            Some(HeaderDrag::Reordering {
                start_x,
                cur_x,
                moved,
                ..
            }) => {
                *cur_x = x;
                if (x - *start_x).abs() > file_list::DRAG_THRESHOLD {
                    *moved = true;
                }
                true
            }
            None => false,
        }
    }

    /// 表头内抬起：结算这次操作——没移动=点击排序，移动了=调整列序。
    pub(crate) fn header_mouse_up(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.header_drag.take() else {
            return;
        };
        let HeaderDrag::Reordering {
            pane,
            tab,
            col,
            moved,
            ..
        } = drag
        else {
            // 调列宽在移动过程中已即时生效；这里只需重绘一次，
            // 让分隔线从「拖动中」的加粗态回到常态。
            cx.notify();
            return;
        };
        if !moved {
            self.toggle_sort(pane, tab, col, cx);
            return;
        }
        let Some(to) = self.header_drop_index(pane, tab, x) else {
            return;
        };
        if let Some(from) = self.cols.index_of(col) {
            if self.cols.move_col(from, to) {
                cx.notify();
            }
        }
    }

    /// 拖列落点：指针落在哪一列的中线之前，就插到那一列前面。
    ///
    /// ⚠️ 用 prepaint 回写的真实 bounds（名称列弹性，宽度算不出来），
    /// 并核对 bounds 的归属窗格——分栏时两个窗格都会回写同一份缓存。
    fn header_drop_index(&self, pane: usize, tab: usize, x: f32) -> Option<usize> {
        if self.header_cells_owner != (pane, tab) || self.header_cells.is_empty() {
            return None;
        }
        let centers: Vec<f32> = self
            .header_cells
            .iter()
            .map(|(_, b)| f32::from(b.origin.x) + f32::from(b.size.width) / 2.0)
            .collect();
        crate::list_columns::drop_index(&centers, x)
    }

    /// 正在被拖动换位的列（渲染时给这一列加高亮反馈）。
    pub(crate) fn dragging_column(&self) -> Option<crate::list_columns::ColId> {
        match &self.header_drag {
            Some(HeaderDrag::Reordering {
                col, moved: true, ..
            }) => Some(*col),
            _ => None,
        }
    }

    /// 正在被拖动的那条列分隔线（它右侧的那一列）——渲染时把线画粗、加深。
    pub(crate) fn resizing_divider(&self) -> Option<crate::list_columns::ColId> {
        match &self.header_drag {
            Some(HeaderDrag::Resizing { col, .. }) => Some(*col),
            _ => None,
        }
    }

    /// 点击表头：切换排序。同一列再点翻转方向，换列则用该列的自然方向。
    pub(crate) fn toggle_sort(
        &mut self,
        pane: usize,
        tab: usize,
        col: crate::list_columns::ColId,
        cx: &mut Context<Self>,
    ) {
        let key = col.sort_key();
        let Some(p) = self.panel_at_mut(pane, tab) else {
            return;
        };
        let next = if p.sort.0 == key {
            (key, p.sort.1.flipped())
        } else {
            (key, mo_core::SortDir::natural_for(key))
        };
        p.sort = next;
        let app = p.app.clone();
        // 排序在 app 侧重建可见索引并广播 DirectoryChanged，UI 由 sync_panel 回灌。
        cx.spawn(async move |_weak, _cx| {
            app.set_sort(next.0, next.1).await;
        })
        .detach();
        cx.notify();
    }

    /// 鼠标在某个文件行上抬起：拖到目录行才算放下，否则继续冒泡给窗格。
    ///
    /// 事件顺序是「行 → 窗格」，所以行处理不了时把拖拽状态放回去，
    /// 让窗格级的 [`Self::drop_on_pane`] 有机会落到目标目录上。
    pub(crate) fn drop_on_entry(
        &mut self,
        pane: usize,
        _tab: usize,
        path: PathBuf,
        is_dir: bool,
        alt: bool,
        cx: &mut Context<Self>,
    ) {
        // 拖拽源记录在 DragState 里，这里只需要知道落在哪个窗格。
        let Some(d) = self.drag.take() else {
            return;
        };
        // 原地按下抬起（普通点击）：不是拖拽。
        if d.pane == pane && d.paths.len() == 1 && d.paths[0] == path {
            return;
        }
        let self_drop = d.paths.iter().any(|p| p == &path);
        if is_dir && !self_drop {
            self.run_transfer(d, path, alt, cx);
            return;
        }
        if d.pane != pane {
            // 跨窗格但落在非目录行上：交回给窗格级处理。
            self.drag = Some(d);
        }
    }

    /// 鼠标在某个窗格内抬起：跨窗格拖拽落到该窗格的当前目录。
    pub(crate) fn drop_on_pane(&mut self, pane_idx: usize, alt: bool, cx: &mut Context<Self>) {
        let Some(d) = self.drag.take() else {
            return;
        };
        if d.pane == pane_idx {
            return;
        }
        let dest = self
            .panes
            .get(pane_idx)
            .and_then(|p| p.tabs.get(p.active))
            .and_then(|p| p.path.clone());
        let Some(dest) = dest else {
            return;
        };
        let refresh_app = self
            .panes
            .get(pane_idx)
            .and_then(|p| p.tabs.get(p.active))
            .map(|p| p.app.clone());
        self.run_transfer(d, dest, alt, cx);
        // 目标窗格可能没开监听：主动刷一次让新文件立刻出现。
        if let Some(app) = refresh_app {
            cx.spawn(async move |_weak, _cx| {
                let _ = app.refresh().await;
            })
            .detach();
        }
    }

    /// 真正提交复制 / 移动：按住 ⌥ 是移动，否则复制。
    fn run_transfer(&mut self, d: DragState, dest: PathBuf, alt: bool, cx: &mut Context<Self>) {
        let Some(app) = self.panel_at(d.pane, d.tab).map(|p| p.app.clone()) else {
            return;
        };
        let paths = d.paths.clone();
        cx.spawn(async move |_weak, _cx| {
            let _ = app.transfer(paths, &dest, alt).await;
        })
        .detach();
        cx.notify();
    }

    // ------------------------------------------------------------ 右键菜单

    /// 打开右键菜单。
    ///
    /// `target` 为 `None` 表示点在空白处（动作对象是**当前目录**）。对着条目右键时，
    /// 若该条目不在当前选区里，先把它单选下来——与 Finder 一致：右键既是「弹菜单」
    /// 也是「选中这一项」。少了这一步，菜单里的「移到废纸篓」会去删旧选区。
    ///
    /// `(pane, tab)` 是事件来自哪个标签页：分栏时右键必须作用在被点的窗格上，
    /// 所以这里会顺带把焦点切过去（重命名 / 属性等都读 `active_pane`）。
    pub(crate) fn open_context_menu(
        &mut self,
        target: Option<(PathBuf, bool)>,
        x: f32,
        y: f32,
        pane: usize,
        tab: usize,
        cx: &mut Context<Self>,
    ) {
        if pane < self.panes.len() {
            self.active_pane = pane;
        }

        if let Some((path, _)) = target.as_ref() {
            let id = self
                .panel_at(pane, tab)
                .and_then(|p| p.window.iter().find(|e| e.path == *path).map(|e| e.id));
            if let Some(id) = id {
                let already = self
                    .panel_at(pane, tab)
                    .is_some_and(|p| p.selection.is_selected(&id));
                if !already {
                    if let Some(p) = self.panel_at_mut(pane, tab) {
                        p.selection.select(id);
                    }
                    let app = self
                        .panel_at(pane, tab)
                        .map(|p| p.app.clone())
                        .unwrap_or_else(|| self.app());
                    cx.spawn(async move |_weak, _cx| {
                        app.select(id).await;
                    })
                    .detach();
                }
            }
        }

        // 目标路径快照：按窗口（可见）顺序取当前选中的那些。
        // 右击的条目已在上面同步写进 panel.selection，所以这里一定包含它。
        let paths: Vec<PathBuf> = self
            .panel_at(pane, tab)
            .map(|p| {
                p.window
                    .iter()
                    .filter(|e| p.selection.is_selected(&e.id))
                    .map(|e| e.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        let (target_path, is_dir) = match target {
            Some((p, d)) => (Some(p), d),
            None => (None, true),
        };
        self.context_menu = Some(crate::context_menu::ContextMenu {
            x,
            y,
            target: target_path,
            is_dir,
            selected: paths.len(),
            paths,
        });
        cx.notify();
    }

    /// 关闭右键菜单（点空白 / Esc / 执行完动作）。
    pub(crate) fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// 在当前窗格的目录里新建一个条目（文件夹 / 空文本文件）。
    ///
    /// 两种新建的流程一模一样（取当前目录 → 异步创建 → 刷新列表），差别只在
    /// 调哪个 `AppState` 方法，所以合成一处——顺手也把错误文案统一了。
    fn create_entry(&mut self, kind: NewEntry, cx: &mut Context<Self>) {
        let Some(dir) = self.panel().path.clone() else {
            return;
        };
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let (created, what) = match kind {
                NewEntry::Folder => (app.create_folder(&dir, "新建文件夹").await, "文件夹"),
                NewEntry::File => (app.create_file(&dir, "新建文本.txt").await, "文本文件"),
            };
            match created {
                Ok(path) => {
                    // 建好了：让列表立刻显示（watcher 可能有延迟）。
                    let _ = app.refresh().await;
                    tracing::debug!("新建{what}：{}", path.display());
                }
                Err(e) => {
                    let msg = format!("新建{what}失败：{e}");
                    this.update(cx, |v, cx| {
                        v.modal = Modal::Info(msg);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// 执行一个菜单动作。菜单在此之前就已关闭（动作可能自己开模态）。
    pub(crate) fn run_menu_action(
        &mut self,
        action: crate::context_menu::MenuAction,
        cx: &mut Context<Self>,
    ) {
        use crate::context_menu::MenuAction as A;

        let Some(menu) = self.context_menu.take() else {
            return;
        };
        let target = menu.target.clone();
        let is_dir = menu.is_dir;

        match action {
            A::Open => {
                if let Some(p) = target {
                    self.open_entry(p, cx);
                }
            }
            A::QuickLook => {
                let app = self.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    open_quick_look(&app, &this, cx).await;
                })
                .detach();
            }
            A::OpenInNewTab => {
                if let Some(p) = target {
                    let pane = self.active_pane;
                    self.open_in_new_tab(pane, p, cx);
                }
            }
            A::OpenInSplit => {
                if let Some(p) = target {
                    // 已经分栏时不再重复开，只把目标目录开在第二窗格。
                    if self.split && self.panes.len() > 1 {
                        let app = self
                            .panes
                            .get(1)
                            .map(|pane| pane.panel().app.clone())
                            .unwrap_or_else(|| self.app());
                        self.active_pane = 1;
                        cx.spawn(async move |_weak, _cx| {
                            let _ = app.open_directory(&p).await;
                        })
                        .detach();
                    } else {
                        self.toggle_split(Some(cx), Some(p));
                    }
                }
            }
            A::Rename => self.open_batch_rename(cx, Some(menu.paths.clone())),
            A::Duplicate => {
                let app = self.app();
                let paths = menu.paths.clone();
                cx.spawn(async move |_weak, _cx| {
                    app.duplicate_paths(paths).await;
                })
                .detach();
            }
            A::Copy => {
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    app.copy_selection_to_clipboard().await;
                })
                .detach();
            }
            A::Cut => {
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    app.cut_selection_to_clipboard().await;
                })
                .detach();
            }
            A::Paste => {
                let dest = self.panel().path.clone();
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    let _ = app.paste_clipboard(dest).await;
                })
                .detach();
            }
            A::CopyPath => {
                // 路径是普通文本，直接进系统剪贴板（不经过内部文件剪贴板）。
                if !menu.paths.is_empty() {
                    let text = menu
                        .paths
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join("\n");
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            A::Trash => {
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    app.delete_selection().await;
                })
                .detach();
            }
            A::NewFolder => self.create_entry(NewEntry::Folder, cx),
            A::NewFile => self.create_entry(NewEntry::File, cx),
            A::Compress => self.open_archive(cx, Some(menu.paths.clone())),
            A::Extract => self.extract_selected(cx),
            A::Hash => {
                let app = self.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    compute_hash(&app, &this, cx).await;
                })
                .detach();
            }
            A::Compare => {
                let app = self.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    run_compare(&app, &this, cx).await;
                })
                .detach();
            }
            A::Tags => {
                self.form_index = 0;
                self.modal = Modal::Tags;
            }
            // 空白处右键 = 简介当前目录；对着条目 = 简介那个条目。
            A::Properties => {
                let t = target.clone().or_else(|| self.panel().path.clone());
                self.open_properties(cx, t);
            }
            // 对着目录右键 = 分析**那个目录**；空白处则分析当前目录。
            A::DiskUsage => self.analyze_disk_usage(cx, target.filter(|_| is_dir)),
            A::OpenTerminal => self.open_terminal_in(cx, target.filter(|_| is_dir)),
            A::Refresh => {
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    let _ = app.refresh().await;
                })
                .detach();
            }
            A::SelectAll => {
                let app = self.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    app.select_all_visible().await;
                    pull_selection(&app, &this, cx).await;
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// 「在终端中打开」：`dir` 为 `None` 时用当前目录。
    fn open_terminal_in(&mut self, cx: &mut Context<Self>, dir: Option<PathBuf>) {
        let app = self.app();
        let dir = dir.or_else(|| self.panel().path.clone());
        let Some(dir) = dir else {
            return;
        };
        if let Err(e) = app.open_terminal(&dir) {
            self.modal = Modal::Info(format!("打开终端失败：{e}"));
            cx.notify();
        }
    }

    /// 应用当前过滤词（输入即过滤），作用于当前焦点标签页。
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.panel().query.trim().to_string();
        let app = self.app();
        cx.spawn(async move |_weak, _cx| {
            app.set_filter(if query.is_empty() { None } else { Some(query) })
                .await;
        })
        .detach();
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// 当前用户的 Home 目录。
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
}

/// 一个标签页的生命周期任务：按需打开 Home → 首同步 → 订阅事件总线。
///
/// 键入 WeakEntity：视图销毁后这里静默退出，不会把它钉在内存里。
async fn tab_loop(
    app: AppState,
    this: WeakEntity<RootView>,
    cx: &mut AsyncApp,
    pane: usize,
    tab: usize,
    open_home: bool,
) {
    if open_home {
        if let Some(home) = home_dir() {
            let _ = mo_app::DirectoryController::new(app.clone())
                .open(&home)
                .await;
        }
    }
    sync_panel(&app, &this, cx, pane, tab).await;

    let mut rx = app.bus().subscribe();
    loop {
        if rx.recv().await.is_err() {
            return;
        }
        // 标签页可能已被关闭：这轮同步前先看目标还在不在。
        let alive = this
            .update(cx, |v, _cx| v.has_panel(pane, tab))
            .unwrap_or(false);
        if !alive {
            return;
        }
        sync_panel(&app, &this, cx, pane, tab).await;
    }
}

/// 把某个 `AppState` 的当前状态同步进它对应标签页的 UI 快照并触发重绘。
///
/// 注意只同步**元信息**与当前窗口，不克隆整份条目列表。
async fn sync_panel(
    app: &AppState,
    this: &WeakEntity<RootView>,
    cx: &mut AsyncApp,
    pane_idx: usize,
    tab_idx: usize,
) {
    let path = app.current_path().await;
    let count = app.visible_count().await;
    let sort = app.sort().await;
    let can_back = app.can_go_back().await;
    let can_forward = app.can_go_forward().await;
    let ops = app.operations_snapshot().await;
    let indexed = app.index_count();
    let trash_entries = app.trash_list();
    // app 侧选择是唯一事实来源；⌘A / 键盘移动等改动都从这里回灌 UI。
    let selection_ids = app.selection_ids().await;

    let outcome = this.update(cx, |v, _cx| {
        let ui_path = match v.panel_at(pane_idx, tab_idx) {
            Some(p) => p.path.clone(),
            None => return None,
        };
        v.indexed = indexed;
        v.trash_entries = trash_entries;
        let p = v.panel_at_mut(pane_idx, tab_idx)?;
        // 切换目录或改过滤词后，旧窗口的下标已失效，直接作废。
        if ui_path != path || p.visible_count != count {
            tracing::warn!(
                target: "mo_ui::window",
                pane = pane_idx, tab = tab_idx,
                ui_path = ?ui_path, app_path = ?path,
                ui_count = p.visible_count, app_count = count,
                "sync invalidated window"
            );
            p.window.clear();
            p.window_start = 0;
            p.pending = None;
        }
        p.path = path;
        p.visible_count = count;
        p.sort = sort;
        p.can_back = can_back;
        p.can_forward = can_forward;
        p.ops = ops;
        p.selection.set_from(&selection_ids);
        // 补窗任务在途时绝不动窗口：若按旧窗口范围重取并清 pending，
        // 会与 file_list 渲染闭包的补窗任务竞态——重复派发任务、
        // 窗口被拉回旧范围，滚动时整屏占位符来回闪烁。
        // 元数据回填的刷新不急于一时：补窗落地后下一轮 sync（120ms 后）自然会刷。
        if p.pending.is_some() {
            tracing::debug!(
                target: "mo_ui::window",
                pane = pane_idx, tab = tab_idx,
                pending = ?p.pending, win_start = p.window_start,
                win_len = p.window.len(), "sync skipped: window fetch in flight"
            );
            return None;
        }
        let len = if p.window.is_empty() {
            INITIAL_WINDOW
        } else {
            p.window.len()
        };
        Some(p.window_start..p.window_start + len)
    });
    let Ok(Some(range)) = outcome else {
        // 视图已销毁 / 标签页已关闭 / 补窗任务在途（本轮跳过）。
        return;
    };

    let (dir_path, start, entries) = app.visible_window(range).await;
    let for_thumbs = entries.clone();
    // WeakEntity::update 返回 Result：视图可能已销毁，这里忽略。
    let _ = this.update(cx, |v, cx| {
        let Some(p) = v.panel_at_mut(pane_idx, tab_idx) else {
            return;
        };
        // 取回在途时可能已切换目录：旧目录的快照不能覆盖新目录的窗口。
        if p.path.as_deref() != Some(dir_path.as_path()) {
            return;
        }
        // 只更新窗口内容，不碰 pending——pending 只归补窗任务管。
        p.window_start = start;
        p.window = entries;
        cx.notify();
    });
    // 缩略图只为当前窗口生成。
    app.thumbs().request(app.clone(), for_thumbs);
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let entity_key = entity.clone();

        let visible_panes = if self.split && self.panes.len() > 1 {
            2
        } else {
            1
        };
        // 每个窗格可用的横向宽度：网格 / 画廊按它算列数（uniform_list 必须先知道行数）。
        let viewport_w = window.viewport_size().width.to_f64() as f32;
        let per_pane_w = ((viewport_w - SIDEBAR_WIDTH) / visible_panes as f32 - 24.0).max(160.0);

        // 模态打开时，工具栏 / 状态栏保留，中央区换成模态卡片。
        let body: Div = match &self.modal {
            Modal::None => {
                let mut row = div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .child(sidebar::render(
                        &self.panel().app,
                        &self.panel().path,
                        &entity,
                    ));
                self.ensure_columns(cx);
                for i in 0..visible_panes {
                    row = row.child(render_pane(self, i, &entity, per_pane_w));
                }
                row
            }
            Modal::CommandPalette => self.render_command_palette(&entity),
            Modal::GlobalSearch => self.render_global_search(&entity),
            Modal::QuickLook => self.render_quick_look(),
            Modal::Trash => self.render_trash(),
            Modal::Diff => self.render_diff(),
            Modal::Properties => dialogs::properties(self, &entity),
            Modal::BatchRename => dialogs::batch_rename(self, &entity),
            Modal::Archive => dialogs::archive(self, &entity),
            Modal::DiskUsage => dialogs::disk_usage(self, &entity),
            Modal::Tags => dialogs::tags(&entity, self),
            Modal::Info(text) => render_info(text),
        };

        let panel = self.panel();
        let ops = self.all_ops();
        let app = panel.app.clone();
        let selection_count = panel.selection.count();
        let can_undo = app.can_undo();
        let can_redo = app.can_redo();

        let mut root = div()
            .flex()
            .flex_col()
            .size_full()
            // 右键菜单是绝对定位的浮层，需要一个定位上下文（否则会去找更外层）。
            .relative()
            .bg(theme::surface())
            .text_color(theme::text())
            .track_focus(&self.focus)
            // 最顶部一行：标签页条 +（Win/Linux）窗口控制按钮，与 macOS 红绿灯同高同行。
            .child(render_top_row(self, &entity, window.is_maximized()))
            .child(toolbar::render(
                &app,
                &entity,
                panel.can_back,
                panel.can_forward,
                &panel.path,
                panel.address_editing,
                panel.address.as_ref(),
                panel.view_mode,
            ))
            .child(body)
            .child(progress_panel::render(&ops, &app))
            .child(status_bar::render(
                panel.visible_count,
                &panel.path,
                &panel.query,
                selection_count,
                self.indexed,
                can_undo,
                can_redo,
            ));

        // 键盘路由：全局快捷键 + 模态内导航 + 输入即过滤。
        root.interactivity().on_key_down(move |ev, _window, cx| {
            let key = ev.keystroke.key.as_str();
            let m = &ev.keystroke.modifiers;
            let platform = m.platform;
            let shift = m.shift;
            let plain = !m.control && !m.alt && !m.platform;

            // 右键菜单开着时 Esc 先关菜单（其余按键继续走正常路由，
            // 菜单不该像模态那样吃掉方向键 / 输入即过滤）。
            if key == "escape" {
                let had_menu = entity_key.update(cx, |v, cx| {
                    let had = v.context_menu.is_some();
                    if had {
                        v.close_context_menu(cx);
                    }
                    had
                });
                if had_menu {
                    return;
                }
            }

            // 全局快捷键（任何状态下都可触发）。
            // ⌘Q：裸二进制没有菜单栏，macOS 收不到系统 terminate，
            // 自己接住退出键（cx.quit 走 gpui 的正常退出流程）。
            if platform && key.eq_ignore_ascii_case("q") {
                cx.quit();
                return;
            }
            if platform && shift && key.eq_ignore_ascii_case("p") {
                entity_key.update(cx, |v, cx| {
                    v.modal = Modal::CommandPalette;
                    v.cmd_query.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
                return;
            }
            if platform && key.eq_ignore_ascii_case("f") {
                entity_key.update(cx, |v, cx| {
                    v.modal = Modal::GlobalSearch;
                    v.search_query.clear();
                    v.search_results.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
                return;
            }
            if platform && key.eq_ignore_ascii_case("a") {
                let app = entity_key.update(cx, |v, _cx| v.app());
                let this = entity_key.clone();
                cx.spawn(async move |cx| {
                    app.select_all_visible().await;
                    // app 侧选择是唯一事实来源：全选后回灌 UI 高亮。
                    pull_selection(&app, &this, cx).await;
                })
                .detach();
                return;
            }
            if platform && key.eq_ignore_ascii_case("z") {
                let app = entity_key.update(cx, |v, _cx| v.app());
                if shift {
                    app.redo();
                } else {
                    app.undo();
                }
                return;
            }
            if platform && key.eq_ignore_ascii_case("t") {
                entity_key.update(cx, |v, cx| {
                    v.new_tab(cx, v.active_pane);
                    cx.notify();
                });
                return;
            }
            if platform && key.eq_ignore_ascii_case("w") {
                entity_key.update(cx, |v, cx| {
                    let pane = v.active_pane;
                    let tab = v.pane().active;
                    if !v.close_tab(pane, tab, cx) {
                        cx.notify();
                    }
                });
                return;
            }
            // ⌘⇧[ / ⌘⇧]：同一窗格内切换标签页（Finder 风格）。
            if platform && shift && (key == "[" || key == "]") {
                let step = if key == "[" { -1 } else { 1 };
                entity_key.update(cx, |v, cx| {
                    v.cycle_tab(step);
                    cx.notify();
                });
                return;
            }
            // ⌘⇧D：双栏分栏开关（第二窗格按需创建）。
            if platform && shift && key.eq_ignore_ascii_case("d") {
                entity_key.update(cx, |v, cx| {
                    v.toggle_split(Some(cx), None);
                    cx.notify();
                });
                return;
            }
            // ⌘1..4：切换视图模式（列表 / 网格 / 画廊 / 列视图）。
            if platform && matches!(key, "1" | "2" | "3" | "4") {
                let mode = match key {
                    "1" => ViewMode::List,
                    "2" => ViewMode::Grid,
                    "3" => ViewMode::Gallery,
                    _ => ViewMode::Columns,
                };
                entity_key.update(cx, |v, cx| {
                    v.panel_mut().view_mode = mode;
                    cx.notify();
                });
                return;
            }
            // ⌘I：属性与权限面板（对着选中项，没有选中则当前目录）。
            if platform && key.eq_ignore_ascii_case("i") {
                entity_key.update(cx, |v, cx| {
                    v.open_properties(cx, None);
                    cx.notify();
                });
                return;
            }
            // ⌘C / ⌘X / ⌘V：应用内剪贴板的复制 / 剪切 / 粘贴。
            if platform && key.eq_ignore_ascii_case("c") {
                let app = entity_key.update(cx, |v, _cx| v.app());
                cx.spawn(async move |_cx| {
                    app.copy_selection_to_clipboard().await;
                })
                .detach();
                return;
            }
            if platform && key.eq_ignore_ascii_case("x") {
                let app = entity_key.update(cx, |v, _cx| v.app());
                cx.spawn(async move |_cx| {
                    app.cut_selection_to_clipboard().await;
                })
                .detach();
                return;
            }
            if platform && key.eq_ignore_ascii_case("v") {
                let app = entity_key.update(cx, |v, _cx| v.app());
                let dest = entity_key.update(cx, |v, _cx| v.panel().path.clone());
                cx.spawn(async move |_cx| {
                    let _ = app.paste_clipboard(dest).await;
                })
                .detach();
                return;
            }
            // ⌘⇧← / ⌘⇧→：在窗格之间移动焦点。
            if platform && shift && (key == "left" || key == "right") {
                entity_key.update(cx, |v, cx| {
                    v.switch_pane(if key == "left" { -1 } else { 1 });
                    cx.notify();
                });
                return;
            }

            // 地址栏编辑态：按键基本都归地址栏的真实输入组件——它把退格 / 方向键 /
            // ⌘A / ⌘C⌘X⌘V / ⌘Z / 回车都注册成了 action，**在绑定阶段就被消费掉**
            // （早于本监听器），根本到不了这里。所以这里只兜住没被它消费的那几个：
            // Esc 退出编辑；字符 / 输入法组合一律放行给输入组件。
            // ⚠️ 整段 return 是必要的：否则「输入即过滤」「上下键导航」会跟着一起触发。
            if entity_key.update(cx, |v, _cx| v.panel().address_editing) {
                if key == "escape" {
                    entity_key.update(cx, |v, cx| v.end_address_edit(cx));
                }
                return;
            }

            // 模态内按键。
            if entity_key.update(cx, |v, _cx| v.modal != Modal::None) {
                handle_modal_key(key, plain, &entity_key, cx);
                return;
            }

            // 非模态：键盘导航 + 输入即过滤 + 快速预览 + 删除选中。
            match key {
                "up" | "down" if plain || shift => {
                    let app = entity_key.update(cx, |v, _cx| v.app());
                    let this = entity_key.clone();
                    let step = if key == "up" { -1 } else { 1 };
                    let extend = shift;
                    cx.spawn(async move |cx| {
                        app.move_cursor(step, extend).await;
                        // app 侧选择是唯一事实来源：移动后回灌 UI 高亮。
                        pull_selection(&app, &this, cx).await;
                    })
                    .detach();
                }
                "enter" if plain => {
                    let app = entity_key.update(cx, |v, _cx| v.app());
                    let this = entity_key.clone();
                    cx.spawn(async move |cx| {
                        open_focused(&app, &this, cx).await;
                    })
                    .detach();
                }
                " " if plain => {
                    let app = entity_key.update(cx, |v, _cx| v.app());
                    let this = entity_key.clone();
                    cx.spawn(async move |cx| {
                        open_quick_look(&app, &this, cx).await;
                    })
                    .detach();
                }
                "delete" if plain => {
                    let app = entity_key.update(cx, |v, _cx| v.app());
                    cx.spawn(async move |_cx| {
                        app.delete_selection().await;
                    })
                    .detach();
                }
                "backspace" => {
                    entity_key.update(cx, |v, cx| {
                        if v.panel().query.is_empty() {
                            // 没有过滤词时返回上级目录。
                            let app = v.app();
                            cx.spawn(async move |_weak, _cx| {
                                let _ = app.open_parent().await;
                            })
                            .detach();
                        } else {
                            v.panel_mut().query.pop();
                            v.apply_filter(cx);
                        }
                        cx.notify();
                    });
                }
                "escape" => {
                    entity_key.update(cx, |v, cx| {
                        v.panel_mut().query.clear();
                        v.apply_filter(cx);
                        cx.notify();
                    });
                }
                k if plain && k.chars().count() == 1 => {
                    let ch = k.chars().next().unwrap();
                    entity_key.update(cx, |v, cx| {
                        v.panel_mut().query.push(ch);
                        v.apply_filter(cx);
                        cx.notify();
                    });
                }
                _ => {}
            }
        });

        // 让焦点落在本视图上，否则按键不会派发到这里。
        if !self.focus.is_focused(window) {
            cx.focus_self(window);
        }

        // 右键菜单：绝对定位的浮层，最后挂上去（画在最上层、命中链最前）。
        // 用窗口坐标直接当偏移量——根容器从 (0, 0) 铺满窗口，两者同一套坐标系。
        if let Some(menu) = self.context_menu.clone() {
            let items = crate::context_menu::items(&menu);
            let vs = window.viewport_size();
            root = root.child(crate::context_menu::render(
                &menu,
                &items,
                (vs.width.to_f64() as f32, vs.height.to_f64() as f32),
                &entity,
            ));
        }

        root
    }
}

/// 窗口最顶端一行：标签页条（每个窗格一组）+（Win/Linux）窗口控制按钮。
///
/// 高度钉死 `TOOLBAR_HEIGHT`，让 macOS 原生红绿灯（`traffic_light_position` 按 48 推导）
/// 与此行垂直居中对齐——所以这一行必须是窗口最顶端、且正好 48px 高。交通灯与窗口控制
/// 按钮都和标签页落在同一行（Win11 资源管理器风格）。
fn render_top_row(view: &RootView, entity: &Entity<RootView>, is_maximized: bool) -> Div {
    let is_macos = cfg!(target_os = "macos");
    let visible_panes = if view.split && view.panes.len() > 1 {
        2
    } else {
        1
    };
    // 始终显示标签条（含单标签页场景，仿 Win11 资源管理器）。
    let show_tab_bar = true;

    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(toolbar::TOOLBAR_HEIGHT))
        .flex_shrink_0()
        // macOS 左侧留出沉浸式红绿灯（x=14 + 三键宽度）。
        .pl(if is_macos { px(80.0) } else { px(0.0) })
        .bg(theme::container())
        .border_b_1()
        .border_color(theme::separator())
        // 测试用（release no-op）：tests/layout.rs 断言此行贴顶且高 48px（红绿灯对齐）。
        .debug_selector(|| "mo-toprow".to_string());

    if show_tab_bar {
        for i in 0..visible_panes {
            row = row.child(render_tab_bar(view, i, entity).flex_1().min_w_0());
        }
    }

    if !is_macos {
        // 无系统标题栏：可拖拽空白（吃掉剩余宽度）紧随其后的是贴右缘的窗口控制按钮。
        row = row
            .child(toolbar::drag_strip())
            .child(toolbar::window_controls(is_maximized));
    }
    row
}

/// 一个窗格：中央浏览区（标签页条已上移到窗口最顶端的 `render_top_row`）。
fn render_pane(view: &RootView, pane_idx: usize, entity: &Entity<RootView>, available: f32) -> Div {
    let Some(pane) = view.panes.get(pane_idx) else {
        return div();
    };
    let tab_idx = pane.active;
    let panel = &pane.tabs[tab_idx.min(pane.tabs.len() - 1)];
    let is_active = pane_idx == view.active_pane;

    let mut col = div().flex().flex_col().flex_1().min_w_0().bg(if is_active {
        theme::surface()
    } else {
        theme::container()
    });
    if pane_idx > 0 {
        col = col.border_l_1().border_color(theme::separator());
    }
    let mut col = col.child(
        div()
            .flex()
            .flex_col()
            .flex_1()
            // 测试用（release no-op）：tests/layout.rs 断言中央区位置与尺寸。
            .debug_selector(|| "mo-center".to_string())
            .child(filter_bar(&panel.query, pane_idx))
            .child(match panel.view_mode {
                ViewMode::List => file_list::render(
                    entity,
                    pane_idx,
                    tab_idx,
                    panel.visible_count,
                    &panel.scroll,
                    file_list::ListChrome {
                        cols: &view.cols,
                        sort: panel.sort,
                        dragging: view.dragging_column(),
                        resizing: view.resizing_divider(),
                    },
                )
                .into_any_element(),
                ViewMode::Columns => {
                    columns::render(entity, pane_idx, tab_idx, &panel.columns).into_any_element()
                }
                ViewMode::Grid | ViewMode::Gallery => {
                    let cols = listing::columns_for(panel.view_mode, available);
                    grid::render(
                        entity,
                        pane_idx,
                        tab_idx,
                        panel.visible_count,
                        cols,
                        panel.view_mode,
                        &panel.scroll,
                    )
                    .into_any_element()
                }
            }),
    );

    // 跨窗格拖拽的落点：鼠标在窗格内抬起时结算（行级处理不了才轮到这里）。
    let drop_entity = entity.clone();
    col.interactivity()
        .on_mouse_up(MouseButton::Left, move |ev, _window, cx| {
            let alt = ev.modifiers.alt;
            drop_entity.update(cx, |v, cx| v.drop_on_pane(pane_idx, alt, cx));
        });

    // 空白处右键：弹「目录级」菜单（新建 / 粘贴 / 刷新 / 全选…）。
    // 条目上的右键已经在行内消化掉并 `stop_propagation`，不会走到这里。
    let ctx_entity = entity.clone();
    col.interactivity()
        .on_mouse_down(MouseButton::Right, move |ev, _window, cx| {
            let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
            ctx_entity.update(cx, |v, cx| {
                v.open_context_menu(None, x, y, pane_idx, tab_idx, cx);
            });
        });

    col
}

/// 标签条：每个标签是一个胶囊（显示当前目录名），右侧 ✕ 关闭，末尾 ＋ 新建。
fn render_tab_bar(view: &RootView, pane_idx: usize, entity: &Entity<RootView>) -> Div {
    let Some(pane) = view.panes.get(pane_idx) else {
        return div();
    };
    let active_tab = pane.active;
    let mut bar = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .h_full()
        .px(px(8.0))
        .bg(theme::container());
    if pane_idx > 0 {
        bar = bar.border_l_1().border_color(theme::separator());
    }

    for (i, tab) in pane.tabs.iter().enumerate() {
        let is_active = i == active_tab;
        let mut item = div()
            .id(format!("tab-{pane_idx}-{i}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .h(px(30.0))
            .px(px(10.0))
            .rounded(px(7.0))
            .text_size(px(12.5))
            .max_w(px(200.0))
            .overflow_hidden()
            // 无边框：纯靠底色区分。当前标签用激活灰（accent）与未激活浅灰区分；
            // 蓝底只留给文件列表的选中高亮（selected_bg）。
            .bg(if is_active {
                theme::accent()
            } else {
                theme::hover_bg()
            })
            .text_color(if is_active {
                theme::text()
            } else {
                theme::muted()
            });
        if !is_active {
            item = item.hover(|s| s.bg(theme::separator()));
        }
        let switch_entity = entity.clone();
        item.interactivity().on_click(move |_, _window, cx| {
            switch_entity.update(cx, |v, cx| {
                v.switch_tab(pane_idx, i);
                cx.notify();
            });
        });
        item = item.child(
            div()
                .flex_1()
                .overflow_hidden()
                .truncate()
                .child(text!(tab.title())),
        );

        {
            let close_entity = entity.clone();
            let mut close = div()
                .id(format!("tab-close-{pane_idx}-{i}"))
                .flex()
                .items_center()
                .justify_center()
                .size(px(16.0))
                .rounded(px(4.0))
                .text_color(theme::muted())
                .hover(|s| s.bg(theme::hover_bg()));
            close.interactivity().on_click(move |_, _window, cx| {
                close_entity.update(cx, |v, cx| {
                    if !v.close_tab(pane_idx, i, cx) {
                        cx.notify();
                    }
                });
            });
            item = item.child(close.child(text!("✕".to_string())));
        }
        bar = bar.child(item);
    }

    let new_entity = entity.clone();
    let mut new_btn = div()
        .id(("tab-new", pane_idx))
        .flex()
        .items_center()
        .justify_center()
        .size(px(26.0))
        .rounded(px(6.0))
        .text_color(theme::muted())
        .hover(|s| s.bg(theme::hover_bg()));
    new_btn.interactivity().on_click(move |_, _window, cx| {
        new_entity.update(cx, |v, cx| {
            v.new_tab(cx, pane_idx);
            cx.notify();
        });
    });
    bar.child(new_btn.child(text!("＋".to_string())))
}

/// 模态内按键处理（返回是否已被处理）。
fn handle_modal_key(key: &str, plain: bool, entity: &Entity<RootView>, cx: &mut App) {
    let modal = entity.update(cx, |v, _cx| v.modal.clone());
    match modal {
        Modal::CommandPalette => match key {
            "escape" => close_modal(entity, cx),
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                if v.palette_index > 0 {
                    v.palette_index -= 1;
                }
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                let n = filtered_commands(&v.cmd_query).len();
                if n > 0 {
                    v.palette_index = (v.palette_index + 1).min(n - 1);
                }
                cx.notify();
            }),
            "enter" => on_palette_enter(entity, cx),
            k if plain && k.chars().count() == 1 => {
                let ch = k.chars().next().unwrap();
                entity.update(cx, |v, cx| {
                    v.cmd_query.push(ch);
                    v.palette_index = 0;
                    cx.notify();
                });
            }
            "backspace" => entity.update(cx, |v, cx| {
                v.cmd_query.pop();
                v.palette_index = 0;
                cx.notify();
            }),
            _ => {}
        },
        Modal::GlobalSearch => match key {
            "escape" => close_modal(entity, cx),
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                if v.palette_index > 0 {
                    v.palette_index -= 1;
                }
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                let n = v.search_results.len();
                if n > 0 {
                    v.palette_index = (v.palette_index + 1).min(n - 1);
                }
                cx.notify();
            }),
            "enter" => on_search_enter(entity, cx),
            k if plain && k.chars().count() == 1 => {
                let ch = k.chars().next().unwrap();
                let app = entity.update(cx, |v, cx| {
                    v.search_query.push(ch);
                    cx.notify();
                    v.app()
                });
                // 实时搜索（同步、毫秒级）。
                let q = entity.update(cx, |v, _cx| v.search_query.clone());
                let results = app.global_search(&q, 50);
                entity.update(cx, |v, cx| {
                    v.search_results = results;
                    v.palette_index = 0;
                    cx.notify();
                });
            }
            "backspace" => {
                let app = entity.update(cx, |v, cx| {
                    v.search_query.pop();
                    cx.notify();
                    v.app()
                });
                let q = entity.update(cx, |v, _cx| v.search_query.clone());
                let results = app.global_search(&q, 50);
                entity.update(cx, |v, cx| {
                    v.search_results = results;
                    v.palette_index = 0;
                    cx.notify();
                });
            }
            _ => {}
        },
        Modal::Trash => match key {
            "escape" => close_modal(entity, cx),
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                if v.palette_index > 0 {
                    v.palette_index -= 1;
                }
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                let n = v.trash_entries.len();
                if n > 0 {
                    v.palette_index = (v.palette_index + 1).min(n - 1);
                }
                cx.notify();
            }),
            "enter" => on_trash_restore(entity, cx),
            "delete" => on_trash_purge(entity, cx),
            "e" if plain => on_trash_empty(entity, cx),
            _ => {}
        },
        Modal::Properties => match key {
            "escape" => close_modal(entity, cx),
            "up" | "down" => entity.update(cx, |v, cx| {
                v.form_index = if key == "up" { 0 } else { 1 };
                cx.notify();
            }),
            "left" => entity.update(cx, |v, cx| {
                if let Some(p) = v.prop.as_mut() {
                    p.bit = p.bit.saturating_sub(1);
                }
                cx.notify();
            }),
            "right" => entity.update(cx, |v, cx| {
                if let Some(p) = v.prop.as_mut() {
                    p.bit = (p.bit + 1).min(8);
                }
                cx.notify();
            }),
            " " => entity.update(cx, |v, cx| {
                if let Some(p) = v.prop.as_mut() {
                    p.toggle_bit();
                }
                cx.notify();
            }),
            "enter" => commit_properties(entity, cx),
            "backspace" => entity.update(cx, |v, cx| {
                if v.form_index == 0 {
                    if let Some(p) = v.prop.as_mut() {
                        p.name.pop();
                    }
                }
                cx.notify();
            }),
            k if plain && k.chars().count() == 1 => {
                let ch = k.chars().next().unwrap();
                entity.update(cx, |v, cx| {
                    if v.form_index == 0 {
                        if let Some(p) = v.prop.as_mut() {
                            p.name.push(ch);
                        }
                    }
                    cx.notify();
                });
            }
            _ => {}
        },
        Modal::BatchRename => match key {
            "escape" => close_modal(entity, cx),
            "up" => entity.update(cx, |v, cx| {
                v.form_index = v.form_index.saturating_sub(1);
                cx.notify();
            }),
            "down" => entity.update(cx, |v, cx| {
                v.form_index = (v.form_index + 1).min(5);
                cx.notify();
            }),
            " " => entity.update(cx, |v, cx| {
                match v.form_index {
                    4 => v.rename_spec.use_index = !v.rename_spec.use_index,
                    5 => v.rename_spec.keep_extension = !v.rename_spec.keep_extension,
                    _ => {}
                }
                cx.notify();
            }),
            "enter" => commit_rename(entity, cx),
            "backspace" => entity.update(cx, |v, cx| {
                pop_rename_field(v);
                cx.notify();
            }),
            k if plain && k.chars().count() == 1 => {
                let ch = k.chars().next().unwrap();
                entity.update(cx, |v, cx| {
                    push_rename_field(v, ch);
                    cx.notify();
                });
            }
            _ => {}
        },
        Modal::Archive => match key {
            "escape" => close_modal(entity, cx),
            "enter" => commit_archive(entity, cx),
            "backspace" => entity.update(cx, |v, cx| {
                v.archive_name.pop();
                cx.notify();
            }),
            k if plain && k.chars().count() == 1 => {
                let ch = k.chars().next().unwrap();
                entity.update(cx, |v, cx| {
                    v.archive_name.push(ch);
                    cx.notify();
                });
            }
            _ => {}
        },
        Modal::DiskUsage => match key {
            "escape" => close_modal(entity, cx),
            "up" => entity.update(cx, |v, cx| {
                v.form_index = v.form_index.saturating_sub(1);
                cx.notify();
            }),
            "down" => entity.update(cx, |v, cx| {
                if !v.usage.is_empty() {
                    v.form_index = (v.form_index + 1).min(v.usage.len() - 1);
                }
                cx.notify();
            }),
            "enter" => {
                let (idx, app) = entity.update(cx, |v, _cx| (v.form_index, v.app()));
                if let Some(u) = entity.read(cx).usage.get(idx).cloned() {
                    if u.path.is_dir() {
                        cx.spawn(async move |_cx| {
                            let _ = app.open_directory(&u.path).await;
                        })
                        .detach();
                    }
                    close_modal(entity, cx);
                }
            }
            _ => {}
        },
        Modal::Tags => match key {
            "escape" => close_modal(entity, cx),
            "up" => entity.update(cx, |v, cx| {
                v.form_index = v.form_index.saturating_sub(1);
                cx.notify();
            }),
            "down" => entity.update(cx, |v, cx| {
                v.form_index = (v.form_index + 1).min(mo_app::TAG_COLORS.len() - 1);
                cx.notify();
            }),
            "enter" => {
                let color = mo_app::TAG_COLORS
                    .get(entity.read(cx).form_index)
                    .map(|(k, _)| k.to_string())
                    .unwrap_or_default();
                entity.update(cx, |v, cx| {
                    v.apply_tag(color);
                    cx.notify();
                });
                close_modal(entity, cx);
            }
            "delete" => {
                entity.update(cx, |v, cx| {
                    v.apply_tag(String::new());
                    cx.notify();
                });
                close_modal(entity, cx);
            }
            _ => {}
        },
        Modal::QuickLook | Modal::Diff | Modal::Info(_) => match key {
            "escape" | " " => close_modal(entity, cx),
            _ => {}
        },
        Modal::None => {}
    }
}

/// 回收站：还原选中条目。
fn on_trash_restore(entity: &Entity<RootView>, cx: &mut App) {
    let (entry, app) = entity.update(cx, |v, cx| {
        let e = v.trash_entries.get(v.palette_index).cloned();
        v.palette_index = 0;
        cx.notify();
        (e, v.app())
    });
    if let Some(e) = entry {
        cx.spawn(async move |_cx| {
            app.restore_trash_entry(e.original).await;
        })
        .detach();
    }
}

/// 回收站：永久删除选中条目。
fn on_trash_purge(entity: &Entity<RootView>, cx: &mut App) {
    let (entry, app) = entity.update(cx, |v, cx| {
        let e = v.trash_entries.get(v.palette_index).cloned();
        v.palette_index = 0;
        cx.notify();
        (e, v.app())
    });
    if let Some(e) = entry {
        app.purge_trash_entry(e);
    }
}

/// 回收站：清空。
fn on_trash_empty(entity: &Entity<RootView>, cx: &mut App) {
    let app = entity.update(cx, |v, cx| {
        v.palette_index = 0;
        cx.notify();
        v.app()
    });
    app.empty_trash();
}

fn close_modal(entity: &Entity<RootView>, cx: &mut App) {
    entity.update(cx, |v, cx| {
        v.modal = Modal::None;
        v.cmd_query.clear();
        v.search_query.clear();
        v.search_results.clear();
        v.preview_cache = None;
        v.diff_cache = None;
        v.palette_index = 0;
        cx.notify();
    });
}

/// 命令面板回车：执行选中的命令。
fn on_palette_enter(entity: &Entity<RootView>, cx: &mut App) {
    let (id, app) = entity.update(cx, |v, _cx| {
        let id = filtered_commands(&v.cmd_query)
            .get(v.palette_index)
            .copied();
        (id, v.app())
    });
    match id {
        Some(CommandId::OpenGlobalSearch) => {
            entity.update(cx, |v, cx| {
                v.modal = Modal::GlobalSearch;
                v.search_query.clear();
                v.search_results.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::QuickLook) => {
            let this = entity.clone();
            cx.spawn(async move |cx| {
                open_quick_look(&app, &this, cx).await;
            })
            .detach();
        }
        Some(CommandId::HashSelection) => {
            let this = entity.clone();
            cx.spawn(async move |cx| {
                compute_hash(&app, &this, cx).await;
            })
            .detach();
        }
        Some(CommandId::CompareSelection) => {
            let this = entity.clone();
            cx.spawn(async move |cx| {
                run_compare(&app, &this, cx).await;
            })
            .detach();
        }
        Some(CommandId::OpenTrash) => {
            entity.update(cx, |v, cx| {
                v.trash_entries = v.app().trash_list();
                v.modal = Modal::Trash;
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::NewTab) => {
            entity.update(cx, |v, cx| {
                let pane = v.active_pane;
                v.new_tab(cx, pane);
                v.modal = Modal::None;
                v.cmd_query.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::CloseTab) => {
            entity.update(cx, |v, cx| {
                let (pane, tab) = (v.active_pane, v.panes.get(v.active_pane).map(|p| p.active));
                let mut quitting = false;
                if let Some(tab) = tab {
                    quitting = v.close_tab(pane, tab, cx);
                }
                v.modal = Modal::None;
                v.cmd_query.clear();
                v.palette_index = 0;
                if !quitting {
                    cx.notify();
                }
            });
        }
        Some(CommandId::ToggleSplit) => {
            entity.update(cx, |v, cx| {
                v.toggle_split(Some(cx), None);
                v.modal = Modal::None;
                v.cmd_query.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::OpenTerminal) => {
            entity.update(cx, |v, cx| {
                let app = v.app();
                if let Some(dir) = v.panel().path.clone() {
                    if let Err(e) = app.open_terminal(&dir) {
                        v.modal = Modal::Info(format!("打开终端失败：{e}"));
                        cx.notify();
                        return;
                    }
                }
                v.modal = Modal::None;
                v.cmd_query.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::AddBookmark) => {
            entity.update(cx, |v, cx| {
                let app = v.app();
                if let Some(p) = v.panel().path.clone() {
                    app.add_bookmark(p);
                }
                v.cmd_query.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::RemoveBookmark) => {
            entity.update(cx, |v, cx| {
                let app = v.app();
                if let Some(p) = v.panel().path.clone() {
                    app.remove_bookmark(&p);
                }
                v.cmd_query.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::Properties) => {
            entity.update(cx, |v, cx| {
                v.open_properties(cx, None);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::BatchRename) => {
            entity.update(cx, |v, cx| {
                v.open_batch_rename(cx, None);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::CreateArchive) => {
            entity.update(cx, |v, cx| {
                v.open_archive(cx, None);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::ExtractArchive) => {
            entity.update(cx, |v, cx| {
                v.extract_selected(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::DiskUsage) => {
            entity.update(cx, |v, cx| {
                v.analyze_disk_usage(cx, None);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::TagSelection) => {
            entity.update(cx, |v, cx| {
                v.open_tags(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::CreateSymlink) | Some(CommandId::CreateHardlink) => {
            let hard = matches!(id, Some(CommandId::CreateHardlink));
            let this = entity.clone();
            cx.spawn(async move |cx| {
                let ok = app.create_links(hard).await;
                this.update(cx, |v, cx| {
                    v.modal = Modal::Info(format!(
                        "已创建 {} 个{}",
                        ok,
                        if hard { "硬链接" } else { "符号链接" }
                    ));
                    v.cmd_query.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
            })
            .detach();
        }
        Some(CommandId::CopyClipboard) | Some(CommandId::CutClipboard) => {
            let cut = matches!(id, Some(CommandId::CutClipboard));
            let this = entity.clone();
            cx.spawn(async move |cx| {
                if cut {
                    app.cut_selection_to_clipboard().await;
                } else {
                    app.copy_selection_to_clipboard().await;
                }
                this.update(cx, |v, cx| {
                    v.modal = Modal::None;
                    v.cmd_query.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
            })
            .detach();
        }
        Some(CommandId::PasteClipboard) => {
            let dest = entity.update(cx, |v, _cx| v.panel().path.clone());
            cx.spawn(async move |_cx| {
                let _ = app.paste_clipboard(dest).await;
            })
            .detach();
            entity.update(cx, |v, cx| {
                v.modal = Modal::None;
                v.cmd_query.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(other) => {
            let this = entity.clone();
            cx.spawn(async move |cx| {
                run_command(other, &app).await;
                // SelectAll / ClearSelection / Delete 等命令改了 app 侧选择，回灌 UI。
                pull_selection(&app, &this, cx).await;
                this.update(cx, |v, cx| {
                    v.modal = Modal::None;
                    v.cmd_query.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
            })
            .detach();
        }
        None => {}
    }
}

/// 全局搜索回车：打开选中的结果（目录则进入，文件则预览）。
fn on_search_enter(entity: &Entity<RootView>, cx: &mut App) {
    let (hit, app) = entity.update(cx, |v, _cx| {
        let hit = v.search_results.get(v.palette_index).cloned();
        (hit, v.app())
    });
    if let Some(h) = hit {
        let this = entity.clone();
        cx.spawn(async move |cx| {
            if h.kind.is_dir() {
                let _ = app.open_directory(&h.path).await;
                this.update(cx, |v, cx| {
                    v.modal = Modal::None;
                    v.search_query.clear();
                    v.search_results.clear();
                    cx.notify();
                });
            } else {
                match app.preview(&h.path) {
                    Ok(pv) => {
                        this.update(cx, |v, cx| {
                            v.preview_cache = Some(pv);
                            v.modal = Modal::QuickLook;
                            cx.notify();
                        });
                    }
                    Err(e) => {
                        this.update(cx, |v, cx| {
                            v.modal = Modal::Info(format!("无法预览：{e}"));
                            cx.notify();
                        });
                    }
                }
            }
        })
        .detach();
    }
}

/// 命令面板里的排序命令：与点表头同语义——同列再点翻转方向，换列用自然方向。
async fn sort_via_command(app: &AppState, key: SortKey) {
    let (cur, dir) = app.sort().await;
    let dir = if cur == key {
        dir.flipped()
    } else {
        mo_core::SortDir::natural_for(key)
    };
    app.set_sort(key, dir).await;
}

/// 执行一个「运行即关闭」类命令。
async fn run_command(id: CommandId, app: &AppState) {
    match id {
        CommandId::Refresh => {
            let _ = app.refresh().await;
        }
        CommandId::Back => {
            let _ = app.go_back().await;
        }
        CommandId::Forward => {
            let _ = app.go_forward().await;
        }
        CommandId::Parent => {
            let _ = app.open_parent().await;
        }
        CommandId::SelectAll => app.select_all_visible().await,
        CommandId::ClearSelection => app.clear_selection().await,
        CommandId::DeleteSelection => {
            app.delete_selection().await;
        }
        CommandId::SortName => sort_via_command(app, SortKey::Name).await,
        CommandId::SortSize => sort_via_command(app, SortKey::Size).await,
        CommandId::SortModified => sort_via_command(app, SortKey::Modified).await,
        CommandId::SortKind => sort_via_command(app, SortKey::Kind).await,
        CommandId::IndexCurrent => {
            if let Some(p) = app.current_path().await {
                app.index_root(p, 0);
            }
        }
        CommandId::StopIndexing => app.stop_indexing(),
        CommandId::Undo => app.undo(),
        CommandId::Redo => app.redo(),
        // 这几个由面板特殊处理（需要 RootView 状态或异步剪贴板），不会走到这里。
        CommandId::OpenGlobalSearch
        | CommandId::QuickLook
        | CommandId::HashSelection
        | CommandId::CompareSelection
        | CommandId::OpenTrash
        | CommandId::OpenTerminal
        | CommandId::AddBookmark
        | CommandId::RemoveBookmark
        | CommandId::Properties
        | CommandId::BatchRename
        | CommandId::CreateArchive
        | CommandId::ExtractArchive
        | CommandId::DiskUsage
        | CommandId::TagSelection
        | CommandId::CopyClipboard
        | CommandId::CutClipboard
        | CommandId::PasteClipboard
        | CommandId::NewTab
        | CommandId::CloseTab
        | CommandId::ToggleSplit
        | CommandId::CreateSymlink
        | CommandId::CreateHardlink => {}
    }
}

/// 打开聚焦 / 选中项的快速预览。
async fn open_quick_look(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    let paths = app.selection_paths().await;
    let Some(p) = paths.into_iter().next() else {
        this.update(cx, |v, cx| {
            v.modal = Modal::Info("没有选中文件".to_string());
            cx.notify();
        });
        return;
    };
    match app.preview(&p) {
        Ok(pv) => {
            this.update(cx, |v, cx| {
                v.preview_cache = Some(pv);
                v.modal = Modal::QuickLook;
                cx.notify();
            });
        }
        Err(e) => {
            this.update(cx, |v, cx| {
                v.modal = Modal::Info(format!("无法预览 {p:?}：{e}"));
                cx.notify();
            });
        }
    }
}

/// 计算选中文件的哈希并展示。
async fn compute_hash(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    let paths = app.selection_paths().await;
    if paths.is_empty() {
        this.update(cx, |v, cx| {
            v.modal = Modal::Info("没有选中文件".to_string());
            cx.notify();
        });
        return;
    }
    let mut out = String::from("校验和（MD5 / SHA-1 / SHA-256）：\n\n");
    for (i, p) in paths.iter().enumerate().take(5) {
        match mo_operations::compute_hashes(p, &[HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256]) {
            Ok(h) => {
                out.push_str(&format!("📄 {}\n", p.display()));
                out.push_str(&format!("  MD5      : {}\n", h[&HashAlgo::Md5]));
                out.push_str(&format!("  SHA-1   : {}\n", h[&HashAlgo::Sha1]));
                out.push_str(&format!("  SHA-256 : {}\n\n", h[&HashAlgo::Sha256]));
            }
            Err(e) => out.push_str(&format!("📄 {} 计算失败：{e}\n\n", p.display())),
        }
        if i == 4 && paths.len() > 5 {
            out.push_str(&format!("…共 {} 个文件，仅显示前 5 个", paths.len()));
        }
    }
    this.update(cx, |v, cx| {
        v.modal = Modal::Info(out);
        cx.notify();
    });
}

/// 比较选中的两项：恰好 2 个条目才执行，结果落入 diff 模态。
async fn run_compare(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    let paths = app.selection_paths().await;
    if paths.len() != 2 {
        this.update(cx, |v, cx| {
            v.modal = Modal::Info(format!(
                "比较需要恰好选中 2 个条目，当前选中 {} 个。\n\n提示：按住 Shift 或 ⌘A 选择后，在命令面板（⌘⇧P）执行「比较选中的两项」。",
                paths.len()
            ));
            cx.notify();
        });
        return;
    }
    let (a, b) = (paths[0].clone(), paths[1].clone());
    match app.compare_paths(a, b).await {
        Ok(c) => {
            this.update(cx, |v, cx| {
                v.diff_cache = Some(c);
                v.modal = Modal::Diff;
                cx.notify();
            });
        }
        Err(e) => {
            this.update(cx, |v, cx| {
                v.modal = Modal::Info(format!("比较失败：{e}"));
                cx.notify();
            });
        }
    }
}

/// 把 app 侧选择快照回灌到 UI 本地缓存（app 侧是唯一事实来源）。
async fn pull_selection(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    let ids = app.selection_ids().await;
    this.update(cx, |v, cx| {
        v.panel_mut().selection.set_from(&ids);
        cx.notify();
    });
}

/// Enter：打开聚焦 / 选中项——目录进入，文件快速预览。
async fn open_focused(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    // `selection_paths` 无选中时回退聚焦项，正好是键盘光标语义。
    let Some(p) = app.selection_paths().await.into_iter().next() else {
        return; // 空目录：没有聚焦项，静默。
    };
    if p.is_dir() {
        let _ = app.open_directory(&p).await;
    } else if let Ok(pv) = app.preview(&p) {
        this.update(cx, |v, cx| {
            v.preview_cache = Some(pv);
            v.modal = Modal::QuickLook;
            cx.notify();
        });
    }
}

/// 过滤条：显示当前关键词与提示。
///
/// `pane` 用于生成唯一元素 ID：分栏时每个窗格各渲染一份本条，
/// `text!` 按调用点生成 ID，若无唯一 ID 链会产生重复的 a11y NodeId。
fn filter_bar(query: &str, pane: usize) -> impl IntoElement {
    if query.is_empty() {
        return div().h(px(0.0));
    }
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(24.0))
        .px(px(8.0))
        .bg(theme::hover_bg())
        .border_b_1()
        .border_color(theme::separator())
        .text_color(theme::accent())
        .child(text!(id = format!("filter-q-{pane}"), format!("🔍 {}", query)))
        .child(
            div()
                .text_color(theme::muted())
                .child(text!(id = format!("filter-esc-{pane}"), "（Esc 清除）".to_string())),
        )
}

// ---------- 模态卡片渲染 ----------

impl RootView {
    fn render_command_palette(&self, _entity: &Entity<RootView>) -> Div {
        let list = filtered_commands(&self.cmd_query);
        let idx = self.palette_index;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for (i, id) in list.iter().enumerate() {
            let def = commands().into_iter().find(|c| c.id == *id).unwrap();
            let selected = i == idx;
            let row = div()
                .id(format!("cmd-row-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .p(px(6.0))
                .bg(if selected {
                    gpui_kit::blue()
                } else {
                    gpui_kit::white()
                })
                .text_color(if selected {
                    gpui_kit::white()
                } else {
                    gpui_kit::black()
                })
                .child(text!(def.category.to_string()))
                .child(text!(def.title.to_string()));
            body = body.child(row);
        }
        if list.is_empty() {
            body = body.child(text!("无匹配命令".to_string()));
        }
        modal_card(
            "命令面板",
            &format!("🔍 {}", self.cmd_query),
            body,
            "↑↓ 选择 · Enter 执行 · Esc 关闭（⌘⇧P 打开）",
        )
    }

    fn render_global_search(&self, _entity: &Entity<RootView>) -> Div {
        let idx = self.palette_index;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for (i, hit) in self.search_results.iter().enumerate() {
            let selected = i == idx;
            let row = div()
                .id(format!("search-row-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .p(px(6.0))
                .bg(if selected {
                    gpui_kit::blue()
                } else {
                    gpui_kit::white()
                })
                .text_color(if selected {
                    gpui_kit::white()
                } else {
                    gpui_kit::black()
                })
                .child(text!(hit.name.clone()))
                .child(text!(format!("{}", hit.path.display())));
            body = body.child(row);
        }
        if self.search_results.is_empty() {
            body = body.child(text!("输入关键词搜索整个文件系统（⌘F）".to_string()));
        }
        modal_card(
            &format!("全局搜索（已索引 {} 项）", self.indexed),
            &format!("🔍 {}", self.search_query),
            body,
            "↑↓ 选择 · Enter 打开 · Esc 关闭",
        )
    }

    fn render_quick_look(&self) -> Div {
        let pv = self.preview_cache.clone();
        let body: Div = match pv {
            Some(p) => {
                let text = p.text.clone().unwrap_or_default();
                match p.kind {
                    mo_preview::PreviewKind::Image => {
                        // 图片：直接加载原图路径。
                        if let Some(path) = &p.image {
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(text!(format!("🖼 {}（{} 字节）", p.title, p.size)))
                                .child(img(path.clone()))
                        } else {
                            div().child(text!(text))
                        }
                    }
                    _ => div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(text!(format!("{} · {} 字节", p.title, p.size)))
                        .child(div().overflow_y_scrollbar().h(px(320.0)).child(text!(text))),
                }
            }
            None => div().child(text!("（无预览）".to_string())),
        };
        modal_card("快速预览", "", body, "Space / Esc 关闭")
    }

    fn render_trash(&self) -> Div {
        let idx = self.palette_index;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for (i, e) in self.trash_entries.iter().enumerate() {
            let selected = i == idx;
            let kind = if e.is_dir { "📁" } else { "📄" };
            let row = div()
                .id(format!("trash-row-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .p(px(6.0))
                .bg(if selected {
                    gpui_kit::blue()
                } else {
                    gpui_kit::white()
                })
                .text_color(if selected {
                    gpui_kit::white()
                } else {
                    gpui_kit::black()
                })
                .child(text!(kind.to_string()))
                .child(text!(e.original.to_string_lossy().to_string()))
                .child(text!(human_ago(e.at, now)));
            body = body.child(row);
        }
        if self.trash_entries.is_empty() {
            body = body.child(text!("回收站是空的".to_string()));
        }
        modal_card(
            &format!("回收站（{} 项）", self.trash_entries.len()),
            "",
            body,
            "↑↓ 选择 · Enter 还原 · Delete 永久删除 · E 清空 · Esc 关闭",
        )
    }
    /// 比较 / diff 模态：文件 → 行级 diff；文件夹 → 树比较清单。
    fn render_diff(&self) -> Div {
        let Some(c) = &self.diff_cache else {
            return modal_card(
                "比较",
                "",
                div().child(text!("（无结果）".to_string())),
                "Esc 关闭",
            );
        };
        match c {
            mo_diff::Comparison::Files(f) => self.render_file_diff(f),
            mo_diff::Comparison::Trees(t) => self.render_tree_diff(t),
        }
    }

    /// 文件 diff：逐行渲染区块序列（红删绿增，右侧行号留空表示该侧无此行）。
    fn render_file_diff(&self, f: &mo_diff::FileComparison) -> Div {
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(text!(format!("左：{}（{} 字节）", f.a.display(), f.a_size)))
            .child(text!(format!("右：{}（{} 字节）", f.b.display(), f.b_size)));

        match &f.status {
            mo_diff::FileStatus::Identical => {
                body = body.child(
                    div()
                        .text_color(diff_same_fg())
                        .child(text!("✓ 两个文件完全相同".to_string())),
                );
            }
            mo_diff::FileStatus::BinaryDiff => {
                body = body.child(
                    div()
                        .text_color(diff_del_fg())
                        .child(text!("≠ 二进制文件不同".to_string())),
                );
            }
            mo_diff::FileStatus::TextDiff(td) => {
                // 渲染上限：超大 diff 截断展示，避免一次性铺几万行。
                const MAX_ROWS: usize = 2000;
                let mut shown = 0usize;
                let mut lines = div()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .overflow_y_scrollbar()
                    .h(px(380.0));
                'outer: for op in &td.ops {
                    match op {
                        mo_diff::DiffOp::Equal { old, new, count } => {
                            for i in 0..*count {
                                if shown >= MAX_ROWS {
                                    break 'outer;
                                }
                                lines = lines.child(diff_row(
                                    " ",
                                    Some(old + i),
                                    Some(new + i),
                                    &td.a_lines[old + i],
                                    diff_eq_bg(),
                                    crate::theme::text(),
                                    shown,
                                ));
                                shown += 1;
                            }
                        }
                        mo_diff::DiffOp::Delete { old, count } => {
                            for i in 0..*count {
                                if shown >= MAX_ROWS {
                                    break 'outer;
                                }
                                lines = lines.child(diff_row(
                                    "−",
                                    Some(old + i),
                                    None,
                                    &td.a_lines[old + i],
                                    diff_del_bg(),
                                    diff_del_fg(),
                                    shown,
                                ));
                                shown += 1;
                            }
                        }
                        mo_diff::DiffOp::Insert { new, count } => {
                            for i in 0..*count {
                                if shown >= MAX_ROWS {
                                    break 'outer;
                                }
                                lines = lines.child(diff_row(
                                    "+",
                                    None,
                                    Some(new + i),
                                    &td.b_lines[new + i],
                                    diff_add_bg(),
                                    diff_add_fg(),
                                    shown,
                                ));
                                shown += 1;
                            }
                        }
                    }
                }
                body = body.child(lines);
                if shown >= MAX_ROWS {
                    body = body.child(
                        div()
                            .text_color(crate::theme::muted())
                            .child(text!(format!("（仅显示前 {MAX_ROWS} 行）"))),
                    );
                }
            }
        }
        modal_card("文件比较", "", body, "Esc / Space 关闭")
    }

    /// 文件夹 diff：统计摘要 + 按相对路径排序的条目清单。
    fn render_tree_diff(&self, t: &mo_diff::TreeComparison) -> Div {
        let summary = if t.is_identical() {
            "✓ 两棵目录树完全一致".to_string()
        } else {
            format!(
                "相同文件 {} · 相同目录 {}（未列出）· 不同 {} · 仅左 {} · 仅右 {}",
                t.identical_files, t.identical_dirs, t.different, t.left_only, t.right_only
            )
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(text!(format!("左：{}", t.left.display())))
            .child(text!(format!("右：{}", t.right.display())))
            .child(
                div()
                    .text_color(crate::theme::muted())
                    .child(text!(summary)),
            );

        let mut rows = div()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for (i, e) in t.entries.iter().enumerate() {
            let (label, fg) = match e.status {
                mo_diff::TreeStatus::Identical => ("＝", crate::theme::muted()),
                mo_diff::TreeStatus::Different => ("≠", diff_del_fg()),
                mo_diff::TreeStatus::LeftOnly => ("◀", crate::theme::accent()),
                mo_diff::TreeStatus::RightOnly => ("▶", diff_add_fg()),
            };
            let name = format!("{}{}", e.rel.display(), if e.is_dir { "/" } else { "" });
            rows = rows.child(
                div()
                    // ⚠️ 无 ID 的循环行会让同站点 text! 产生重复 a11y 节点。
                    .id(format!("tree-diff-row-{i}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(6.0))
                    .text_color(fg)
                    .child(div().w(px(20.0)).child(text!(label.to_string())))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .truncate()
                            .child(text!(name)),
                    ),
            );
        }
        if t.entries.is_empty() {
            rows = rows.child(text!("（无条目）".to_string()));
        }
        body = body.child(rows);
        modal_card("文件夹比较", "", body, "Esc / Space 关闭")
    }
}

/// 粗略的相对时间（依赖零；用于回收站条目展示）。
fn human_ago(at: u64, now: u64) -> String {
    let d = now.saturating_sub(at);
    if d < 60 {
        "刚刚".to_string()
    } else if d < 3600 {
        format!("{} 分钟前", d / 60)
    } else if d < 86_400 {
        format!("{} 小时前", d / 3600)
    } else {
        format!("{} 天前", d / 86_400)
    }
}

/// 一个居中的模态卡片（标题 + 输入行 + 内容 + 底部提示）。
pub(crate) fn modal_card(title: &str, input: &str, body: impl IntoElement, hint: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .child(
            div()
                .flex()
                .flex_col()
                .w(px(640.0))
                .bg(gpui_kit::rgb(0xf7f7f7))
                .rounded(px(8.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .p(px(10.0))
                        .bg(gpui_kit::rgb(0xe8e8e8))
                        .child(text!(title.to_string()))
                        .child(text!(hint.to_string())),
                )
                .child(div().p(px(8.0)).child(text!(input.to_string())))
                .child(div().p(px(8.0)).child(body)),
        )
}

/// 纯文本信息卡片（哈希结果等）。
fn render_info(text: &str) -> Div {
    let body = div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .overflow_y_scrollbar()
        .h(px(320.0))
        .child(text!(text.to_string()));
    modal_card("信息", "", body, "Esc 关闭")
}

// ---------- diff 模态辅助 ----------

/// diff 行组件：标记 + 左右行号 + 内容（固定列宽保证纵向对齐）。
fn diff_row(
    marker: &str,
    old_no: Option<usize>,
    new_no: Option<usize>,
    content: &str,
    bg: gpui_kit::Rgba,
    fg: gpui_kit::Rgba,
    seq: usize,
) -> Stateful<Div> {
    let num = |n: Option<usize>| n.map(|v| (v + 1).to_string()).unwrap_or_default();
    div()
        // ⚠️ diff 行数以千计且都出自同一 `text!` 站点：每行必须有唯一 ID，
        // 否则辅助功能开启时同一行的四段文本共享 NodeId → panic。
        .id(format!("diff-line-{seq}"))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(6.0))
        .bg(bg)
        .text_color(fg)
        .child(div().w(px(16.0)).child(text!(marker.to_string())))
        .child(
            div()
                .w(px(44.0))
                .text_color(crate::theme::muted())
                .child(text!(num(old_no))),
        )
        .child(
            div()
                .w(px(44.0))
                .text_color(crate::theme::muted())
                .child(text!(num(new_no))),
        )
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .truncate()
                .child(text!(content.to_string())),
        )
}

/// 相同行底色（白）。
fn diff_eq_bg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0xffffff)
}

/// 删除行底色（浅红）。
fn diff_del_bg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0xfdecec)
}

/// 删除行前景（深红）。
fn diff_del_fg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0x8f1f1f)
}

/// 新增行底色（浅绿）。
fn diff_add_bg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0xe9f6e9)
}

/// 新增行前景（深绿）。
fn diff_add_fg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0x1f6b2a)
}

/// 一致提示的前景色（绿）。
fn diff_same_fg() -> gpui_kit::Rgba {
    diff_add_fg()
}

// ---------- 新模态的打开 / 提交 ----------

/// 批量重命名字段的读写（0 查找 / 1 替换 / 2 前缀 / 3 后缀）。
fn push_rename_field(v: &mut RootView, ch: char) {
    match v.form_index {
        0 => v.rename_spec.find.push(ch),
        1 => v.rename_spec.replace.push(ch),
        2 => v.rename_spec.prefix.push(ch),
        3 => v.rename_spec.suffix.push(ch),
        _ => {}
    }
}

fn pop_rename_field(v: &mut RootView) {
    match v.form_index {
        0 => {
            v.rename_spec.find.pop();
        }
        1 => {
            v.rename_spec.replace.pop();
        }
        2 => {
            v.rename_spec.prefix.pop();
        }
        3 => {
            v.rename_spec.suffix.pop();
        }
        _ => {}
    }
}

/// 属性面板提交：文件名变更 → 重命名；权限变更 → chmod。
fn commit_properties(entity: &Entity<RootView>, cx: &mut App) {
    let Some(p) = entity.update(cx, |v, _cx| v.prop.clone()) else {
        return;
    };
    let app = entity.update(cx, |v, _cx| v.app());
    let old_name = p
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    cx.spawn(async move |_cx| {
        if !p.name.is_empty() && p.name != old_name {
            let to = p.path.with_file_name(&p.name);
            app.rename_many(vec![(p.path.clone(), to)]).await;
        }
        // 权限按修改后的位应用（路径可能已变，但权限属于同一个 inode）。
        let target = p.path.with_file_name(&p.name);
        if let Err(e) = app.set_permissions(target, p.mode).await {
            tracing::warn!("权限修改失败：{e}");
        }
    })
    .detach();
    close_modal(entity, cx);
}

/// 批量重命名提交：按规则算出目标名后逐个提交重命名操作。
fn commit_rename(entity: &Entity<RootView>, cx: &mut App) {
    let (spec, paths) = entity.update(cx, |v, _cx| (v.rename_spec.clone(), v.rename_paths.clone()));
    let app = entity.update(cx, |v, _cx| v.app());
    cx.spawn(async move |_cx| {
        let names: Vec<String> = paths
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
            .collect();
        let targets = mo_core::plan_batch_rename(&names, &spec);
        let pairs: Vec<(PathBuf, PathBuf)> = paths
            .iter()
            .zip(targets)
            .filter_map(|(src, new)| {
                if new.is_empty() {
                    return None;
                }
                let to = src.with_file_name(&new);
                if to == *src {
                    None
                } else {
                    Some((src.clone(), to))
                }
            })
            .collect();
        app.rename_many(pairs).await;
    })
    .detach();
    close_modal(entity, cx);
}

/// 压缩提交：目标文件放在当前目录下。
fn commit_archive(entity: &Entity<RootView>, cx: &mut App) {
    let (name, sources, dest_dir) = entity.update(cx, |v, _cx| {
        (
            v.archive_name.trim().to_string(),
            v.rename_paths.clone(),
            v.panel().path.clone(),
        )
    });
    if name.is_empty() || sources.is_empty() {
        close_modal(entity, cx);
        return;
    }
    let app = entity.update(cx, |v, _cx| v.app());
    cx.spawn(async move |_cx| {
        let dest = match dest_dir {
            Some(d) => d.join(&name),
            None => PathBuf::from(&name),
        };
        if let Err(e) = app.create_archive(dest, sources).await {
            tracing::warn!("压缩失败：{e}");
        }
    })
    .detach();
    close_modal(entity, cx);
}

#[cfg(test)]
mod tests {
    // ⚠️ 这里**不能** `use super::*`：app.rs 顶层有 `use gpui_kit::*`，
    // 会把 gpui 的 `test` 属性宏一起引进来，把内置的 `#[test]` 顶掉，
    // 展开时直接撞递归上限（`recursion limit reached while expanding #[test]`）。
    use std::path::PathBuf;

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{px, TestAppContext};
    use mo_app::AppState;

    use super::RootView;

    /// 进入地址栏编辑态：路径要**预填**，且内容要**整条被选中**。
    ///
    /// 守的是最初的诉求「地址栏编辑时无法选中文本」。旧实现是自绘的
    /// 「字符串 + 假光标 `▏`」，只有追加字符与退格——没有光标下标、没有选区，
    /// 点选 / 拖选 / ⌘A 全都不存在。现在换成框架的真实输入框，进入编辑时
    /// 按 Finder 行为全选整条路径，直接敲字即可替换。
    ///
    /// 路径直接摆好而不是真去导航：`panel.path` 由异步回灌填充，headless 下
    /// 不稳定；这里要测的是「拿 path 去预填 + 全选」这段我们自己的接线。
    /// 用普通 `#[test]`：`#[gpui_kit::test]` 在 crate 内部展开会宏递归爆栈
    /// （集成测试不受影响，见 `file_item.rs` 的同类注释）。
    #[test]
    fn address_edit_prefills_and_selects_the_whole_path() {
        let mut cx = TestAppContext::single();
        // 框架的 `InputState` 依赖 gpui-component 的 Theme 全局
        // （生产环境由 `run()` 里的 `gpui_kit::init` 注册），测试里补上。
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        let state = cx.update(|window, cx| {
            root.update(cx, |v, cx| {
                v.panel_mut().path = Some(PathBuf::from("/tmp/mo-address-test"));
                v.begin_address_edit(window, cx);
                v.panel()
                    .address
                    .clone()
                    .expect("进入编辑态后应当已经建好输入框")
            })
        });

        // **真的渲染一帧**：输入框的绘制路径（Theme 全局、点击/选区 overlay 等）
        // 只有画出来才会暴露问题，只查状态不算数。
        cx.update(|window, cx| window.render_frame(cx));

        let (value, selected) = cx.update(|_window, cx| {
            let s = state.read(cx);
            (s.value().to_string(), s.selected_value().to_string())
        });

        assert_eq!(value, "/tmp/mo-address-test", "地址栏没有预填当前路径");
        assert_eq!(
            selected, value,
            "进入编辑态应当整条路径全选（选中能力就在这里）：value={value:?} selected={selected:?}"
        );
    }

    /// Esc 与失焦都要退出编辑态（面包屑才回得来）。
    #[test]
    fn address_edit_ends_on_escape_and_blur() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        let (after_enter, after_escape) = cx.update(|window, cx| {
            root.update(cx, |v, cx| {
                v.begin_address_edit(window, cx);
                let entered = v.panel().address_editing;
                v.end_address_edit(cx);
                (entered, v.panel().address_editing)
            })
        });
        assert!(after_enter, "begin_address_edit 没有把面板切到编辑态");
        assert!(!after_escape, "end_address_edit 没有退出编辑态");

        // 再次进入必须复用同一个输入状态实体（不重建、不丢历史）。
        let (first, second) = cx.update(|window, cx| {
            root.update(cx, |v, cx| {
                v.begin_address_edit(window, cx);
                let a = v.panel().address.clone();
                v.end_address_edit(cx);
                v.begin_address_edit(window, cx);
                (a, v.panel().address.clone())
            })
        });
        assert_eq!(first.map(|e| e.entity_id()), second.map(|e| e.entity_id()));
    }

    /// 右键菜单：浮层落点必须就是鼠标位置，并且**不能挤动**根容器的 flex 布局。
    ///
    /// 菜单是根容器的绝对定位子节点。绝对定位在 flex 容器里应当完全脱离文档流；
    /// 一旦定位上下文没建好（根容器缺 `relative()`），它就会掉到别处，甚至参与
    /// 布局把文件列表压小——这条测试同时守住这两点。
    #[test]
    fn context_menu_renders_at_the_pointer_without_disturbing_layout() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|window, cx| window.render_frame(cx));
        let list_before = cx.debug_bounds("mo-file-list").expect("文件列表没有渲染");

        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.open_context_menu(None, 240.0, 300.0, 0, 0, cx)
            });
        });
        cx.update(|window, cx| window.render_frame(cx));

        let menu = cx
            .debug_bounds("mo-context-menu")
            .expect("右键菜单没有渲染出来");
        assert!(
            (f32::from(menu.origin.x) - 240.0).abs() < 1.0
                && (f32::from(menu.origin.y) - 300.0).abs() < 1.0,
            "菜单没有落在鼠标位置上：menu={menu:?}（期望 240,300）"
        );
        assert_eq!(
            menu.size.width,
            px(crate::context_menu::MENU_W),
            "菜单宽度被压缩了"
        );
        assert!(
            menu.size.height > px(0.0),
            "菜单高度为 0：条目没有渲染（绝对定位的高度塌了？）"
        );

        // 第一条菜单项要真的画出来了（否则菜单只是个空壳）。
        assert!(
            cx.debug_bounds("mo-ctx-label-0").is_some(),
            "菜单第一项没有渲染"
        );

        let list_after = cx.debug_bounds("mo-file-list").expect("文件列表没有渲染");
        assert_eq!(
            list_before, list_after,
            "右键菜单挤动了文件列表：绝对定位浮层不应当影响根容器的 flex 布局"
        );
    }

    /// 贴着右下角打开时，菜单必须被钳回视口内（否则会被窗口边缘切掉）。
    #[test]
    fn context_menu_is_clamped_inside_the_viewport() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        let (vw, vh) = cx.update(|window, _cx| {
            let s = window.viewport_size();
            (s.width.to_f64() as f32, s.height.to_f64() as f32)
        });

        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                // 故意越过右下角。
                v.open_context_menu(None, vw - 2.0, vh - 2.0, 0, 0, cx);
            });
        });
        cx.update(|window, cx| window.render_frame(cx));

        let menu = cx
            .debug_bounds("mo-context-menu")
            .expect("右键菜单没有渲染");
        assert!(
            f32::from(menu.origin.x + menu.size.width) <= vw,
            "菜单右侧被切出视口：menu={menu:?} viewport={vw}x{vh}"
        );
        assert!(
            f32::from(menu.origin.y + menu.size.height) <= vh,
            "菜单底部被切出视口：menu={menu:?} viewport={vw}x{vh}"
        );
    }

    /// 条目菜单比空白菜单长（多了打开 / 重命名 / 废纸篓…），且 Esc 能关掉。
    #[test]
    fn entry_menu_has_more_items_and_escape_closes_it() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        let blank_h = cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.open_context_menu(None, 100.0, 100.0, 0, 0, cx);
                v.context_menu
                    .as_ref()
                    .map(|m| crate::context_menu::items(m).len())
            })
        });
        let entry_items = cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.open_context_menu(
                    Some((PathBuf::from("/tmp/a.zip"), false)),
                    100.0,
                    100.0,
                    0,
                    0,
                    cx,
                );
                crate::context_menu::items(v.context_menu.as_ref().unwrap()).len()
            })
        });

        assert!(
            entry_items > blank_h.unwrap(),
            "条目菜单项数应当多于空白菜单：{entry_items} vs {blank_h:?}"
        );

        // Esc 走的是按键路由，这里直接验证关闭语义（按键分发在 headless 下不派发）。
        let closed = cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.close_context_menu(cx);
                v.context_menu.is_none()
            })
        });
        assert!(closed, "close_context_menu 没有清掉菜单");
    }
}
