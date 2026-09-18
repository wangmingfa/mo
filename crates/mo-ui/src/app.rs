use std::path::PathBuf;

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
}

/// 一次拖拽：从哪个窗格的哪个标签页拖出了哪些路径。
#[derive(Clone)]
pub(crate) struct DragState {
    pub(crate) pane: usize,
    pub(crate) tab: usize,
    pub(crate) paths: Vec<PathBuf>,
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

    /// 关闭标签页（最后一个标签页保留：关掉就没有浏览区了）。
    pub(crate) fn close_tab(&mut self, pane_idx: usize, tab_idx: usize) {
        let Some(pane) = self.panes.get_mut(pane_idx) else {
            return;
        };
        if pane.tabs.len() <= 1 {
            return;
        }
        pane.tabs.remove(tab_idx);
        pane.active = pane.active.min(pane.tabs.len() - 1);
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
    pub(crate) fn toggle_split(&mut self, cx: Option<&mut Context<Self>>) {
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
                // 第二窗格同样从 Home 起步。
                tab_loop(app, weak, cx, idx, 0, true).await;
            })
            .detach();
        } else if !self.split && self.panes.len() > 1 {
            self.panes.truncate(1);
            self.active_pane = 0;
        }
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

    /// 进入地址栏编辑态：用当前完整路径预填输入框（供工具栏点击调用）。
    pub fn begin_address_edit(&mut self) {
        let p = self.panel_mut();
        p.address_editing = true;
        p.address_input = p
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
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
    pub(crate) fn open_properties(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let Some(path) = self.panel().path.clone() else {
            return;
        };
        let target = self
            .panel()
            .selection
            .selected_ids()
            .iter()
            .next()
            .and_then(|id| self.panel().window.iter().find(|e| e.id == *id))
            .map(|e| e.path.clone())
            .unwrap_or(path.clone());
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

    /// 打开批量重命名：快照当前选中的路径。
    pub(crate) fn open_batch_rename(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_, cx| {
            let paths = app.selection_paths().await;
            this.update(cx, |v, cx| {
                if paths.is_empty() {
                    v.modal = Modal::Info("没有选中文件".to_string());
                } else {
                    v.rename_paths = paths;
                    v.rename_spec = RenameSpec::default();
                    v.form_index = 0;
                    v.modal = Modal::BatchRename;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 打开压缩对话框。
    pub(crate) fn open_archive(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_, cx| {
            let paths = app.selection_paths().await;
            this.update(cx, |v, cx| {
                if paths.is_empty() {
                    v.modal = Modal::Info("没有选中文件".to_string());
                } else {
                    let hint = paths
                        .first()
                        .and_then(|p| p.file_stem())
                        .map(|s| format!("{}.zip", s.to_string_lossy()))
                        .unwrap_or_else(|| "archive.zip".to_string());
                    v.rename_paths = paths;
                    v.archive_name = hint;
                    v.modal = Modal::Archive;
                }
                cx.notify();
            });
        })
        .detach();
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

    /// 磁盘空间分析：统计当前目录下每个子项的大小。
    pub(crate) fn analyze_disk_usage(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let Some(root) = self.panel().path.clone() else {
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
        // 只有「存在多个标签页或处于分栏」时才显示标签条：单标签页保持原来的干净外观。
        let show_tab_bar = self.split || self.panes.first().is_some_and(|p| p.tabs.len() > 1);
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
                    row = row.child(render_pane(self, i, &entity, show_tab_bar, per_pane_w));
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
            .bg(theme::surface())
            .text_color(theme::text())
            .track_focus(&self.focus)
            .child(toolbar::render(
                &app,
                &entity,
                panel.can_back,
                panel.can_forward,
                &panel.path,
                panel.address_editing,
                &panel.address_input,
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
                    v.close_tab(pane, tab);
                    cx.notify();
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
                    v.toggle_split(Some(cx));
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
            // ⌘I：属性与权限面板。
            if platform && key.eq_ignore_ascii_case("i") {
                entity_key.update(cx, |v, cx| {
                    v.open_properties(cx);
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

            // 地址栏编辑态：独占普通按键（优先于模态与列表导航）。
            if entity_key.update(cx, |v, _cx| v.panel().address_editing) {
                handle_address_key(key, plain, &entity_key, cx);
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

        root
    }
}

/// 一个窗格：标签条（按需）+ 中央浏览区。
fn render_pane(
    view: &RootView,
    pane_idx: usize,
    entity: &Entity<RootView>,
    tab_bar: bool,
    available: f32,
) -> Div {
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
    if tab_bar {
        col = col.child(render_tab_bar(view, pane_idx, entity));
    }
    let mut col = col.child(
        div()
            .flex()
            .flex_col()
            .flex_1()
            // 测试用（release no-op）：tests/layout.rs 断言中央区位置与尺寸。
            .debug_selector(|| "mo-center".to_string())
            .child(filter_bar(&panel.query))
            .child(match panel.view_mode {
                ViewMode::List => file_list::render(
                    entity,
                    pane_idx,
                    tab_idx,
                    panel.visible_count,
                    &panel.scroll,
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
        .h(px(30.0))
        .flex_shrink_0()
        .px(px(8.0))
        .bg(theme::container())
        .border_b_1()
        .border_color(theme::separator());

    for (i, tab) in pane.tabs.iter().enumerate() {
        let is_active = i == active_tab;
        let mut item = div()
            .id(format!("tab-{pane_idx}-{i}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .h(px(22.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .text_size(px(12.0))
            .max_w(px(180.0))
            .overflow_hidden()
            .bg(if is_active {
                theme::surface()
            } else {
                theme::container()
            })
            .text_color(if is_active {
                theme::text()
            } else {
                theme::muted()
            });
        if !is_active {
            item = item.hover(|s| s.bg(theme::hover_bg()));
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

        if pane.tabs.len() > 1 {
            let close_entity = entity.clone();
            let mut close = div()
                .id(format!("tab-close-{pane_idx}-{i}"))
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.0))
                .rounded(px(4.0))
                .text_color(theme::muted())
                .hover(|s| s.bg(theme::hover_bg()));
            close.interactivity().on_click(move |_, _window, cx| {
                close_entity.update(cx, |v, cx| {
                    v.close_tab(pane_idx, i);
                    cx.notify();
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
        .size(px(22.0))
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
/// 地址栏编辑态的按键：输入 / 退格 / 回车跳转 / Esc 取消。
fn handle_address_key(key: &str, plain: bool, entity: &Entity<RootView>, cx: &mut App) {
    match key {
        "escape" => entity.update(cx, |v, cx| {
            let p = v.panel_mut();
            p.address_editing = false;
            p.address_input.clear();
            cx.notify();
        }),
        "enter" => {
            let (app, target) = entity.update(cx, |v, _cx| {
                let p = v.panel_mut();
                p.address_editing = false;
                let text = p.address_input.trim().to_string();
                p.address_input.clear();
                (p.app.clone(), PathBuf::from(text))
            });
            if target.as_os_str().is_empty() {
                return;
            }
            cx.spawn(async move |_cx| {
                if let Err(e) = app.open_directory(&target).await {
                    eprintln!("打开失败: {e}");
                }
            })
            .detach();
        }
        "backspace" => entity.update(cx, |v, cx| {
            v.panel_mut().address_input.pop();
            cx.notify();
        }),
        k if plain && k.chars().count() == 1 => {
            let ch = k.chars().next().unwrap();
            entity.update(cx, |v, cx| {
                v.panel_mut().address_input.push(ch);
                cx.notify();
            });
        }
        _ => {}
    }
}

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
                if let Some(tab) = tab {
                    v.close_tab(pane, tab);
                }
                v.modal = Modal::None;
                v.cmd_query.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::ToggleSplit) => {
            entity.update(cx, |v, cx| {
                v.toggle_split(Some(cx));
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
                v.open_properties(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::BatchRename) => {
            entity.update(cx, |v, cx| {
                v.open_batch_rename(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::CreateArchive) => {
            entity.update(cx, |v, cx| {
                v.open_archive(cx);
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
                v.analyze_disk_usage(cx);
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
        CommandId::SortName => app.set_sort(SortKey::Name).await,
        CommandId::SortSize => app.set_sort(SortKey::Size).await,
        CommandId::SortModified => app.set_sort(SortKey::Modified).await,
        CommandId::SortKind => app.set_sort(SortKey::Kind).await,
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
fn filter_bar(query: &str) -> impl IntoElement {
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
        .child(text!(format!("🔍 {}", query)))
        .child(
            div()
                .text_color(theme::muted())
                .child(text!("（Esc 清除）".to_string())),
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
        for e in &t.entries {
            let (label, fg) = match e.status {
                mo_diff::TreeStatus::Identical => ("＝", crate::theme::muted()),
                mo_diff::TreeStatus::Different => ("≠", diff_del_fg()),
                mo_diff::TreeStatus::LeftOnly => ("◀", crate::theme::accent()),
                mo_diff::TreeStatus::RightOnly => ("▶", diff_add_fg()),
            };
            let name = format!("{}{}", e.rel.display(), if e.is_dir { "/" } else { "" });
            rows = rows.child(
                div()
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
) -> Div {
    let num = |n: Option<usize>| n.map(|v| (v + 1).to_string()).unwrap_or_default();
    div()
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
