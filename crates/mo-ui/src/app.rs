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

/// 同步计划最多列出这么多项（再多也只报总数，避免一次画几千行）。
const PLAN_LIMIT: usize = 400;

/// 当前打开的模态层（占用中央区；Esc 关闭）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Modal {
    None,
    /// 命令面板。
    CommandPalette,
    /// 全局搜索。
    GlobalSearch,
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
    /// 重复文件查找结果。
    Duplicates,
    /// 自动化工作流执行结果。
    Workflow,
    /// 文件夹同步（配对 / 计划 / 执行）。
    Sync,
    /// 主题选择器（↑↓ 实时预览，Enter 应用并持久化，Esc 还原）。
    Theme,
    /// 布局设置器（侧边栏 / 状态栏 / 斑马纹 / 默认视图 / 恢复默认）。
    Layout,
    /// 快捷键设置器（可重映射 / 解绑 / 捕获按键）。
    Keys,
    /// 扩展管理器（列出已加载的扩展，可启停）。
    Extensions,
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
    /// 打开主题选择器。
    ThemePicker,
    ThemeLight,
    ThemeDark,
    ThemeSystem,
    /// 布局设置器与三条直达开关。
    LayoutPicker,
    /// 快捷键设置器。
    KeysPicker,
    /// 扩展管理器。
    ExtensionsPicker,
    /// 在当前目录查找重复文件。
    FindDuplicates,
    /// 文件夹同步面板。
    FolderSync,
    /// 用户自定义命令（下标指向 `RootView::user_commands`）。
    User(usize),
    /// 自动化工作流（下标指向 `RootView::workflows`）。
    Workflow(usize),
    ToggleSidebar,
    ToggleStatusBar,
    ToggleZebra,
}

struct CmdDef {
    id: CommandId,
    title: String,
    category: String,
}

/// 命令目录（命令面板的数据源）。
///
/// `users` 是用户自定义命令，追加在内建命令之后（第四阶段·自定义命令）。
fn commands_in(users: &[mo_app::UserCommand], workflows: &[mo_app::Workflow]) -> Vec<CmdDef> {
    let mut out = vec![
        CmdDef {
            id: CommandId::OpenGlobalSearch,
            title: "全局搜索…".to_string(),
            category: "搜索".to_string(),
        },
        CmdDef {
            id: CommandId::QuickLook,
            title: "快速预览（Quick Look）".to_string(),
            category: "预览".to_string(),
        },
        CmdDef {
            id: CommandId::ThemePicker,
            title: "主题…（浅色 / 深色 / 跟随系统 / 自定义）".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ThemeLight,
            title: "切换到浅色主题".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ThemeDark,
            title: "切换到深色主题".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ThemeSystem,
            title: "主题跟随系统外观".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::FolderSync,
            title: "文件夹同步…（与配对目录双向 / 镜像）".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::FindDuplicates,
            title: "查找重复文件…（当前目录）".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::ExtensionsPicker,
            title: "扩展…（查看 / 启停已加载的扩展）".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::KeysPicker,
            title: "快捷键…（重映射 / 解绑 / 恢复默认）".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::LayoutPicker,
            title: "布局…（侧边栏 / 状态栏 / 斑马纹 / 默认视图）".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ToggleSidebar,
            title: "显示 / 隐藏侧边栏".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ToggleStatusBar,
            title: "显示 / 隐藏状态栏".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ToggleZebra,
            title: "列表斑马纹开关".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::HashSelection,
            title: "计算选中文件哈希（MD5/SHA-1/SHA-256）".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::CompareSelection,
            title: "比较选中的两项（文件 / 文件夹，含 diff）".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::IndexCurrent,
            title: "索引当前目录（建立全局搜索索引）".to_string(),
            category: "搜索".to_string(),
        },
        CmdDef {
            id: CommandId::StopIndexing,
            title: "停止索引".to_string(),
            category: "搜索".to_string(),
        },
        CmdDef {
            id: CommandId::Refresh,
            title: "刷新".to_string(),
            category: "导航".to_string(),
        },
        CmdDef {
            id: CommandId::Back,
            title: "后退".to_string(),
            category: "导航".to_string(),
        },
        CmdDef {
            id: CommandId::Forward,
            title: "前进".to_string(),
            category: "导航".to_string(),
        },
        CmdDef {
            id: CommandId::Parent,
            title: "上级目录".to_string(),
            category: "导航".to_string(),
        },
        CmdDef {
            id: CommandId::SelectAll,
            title: "全选".to_string(),
            category: "选择".to_string(),
        },
        CmdDef {
            id: CommandId::ClearSelection,
            title: "清除选择".to_string(),
            category: "选择".to_string(),
        },
        CmdDef {
            id: CommandId::DeleteSelection,
            title: "删除选中".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::Undo,
            title: "撤销".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::Redo,
            title: "重做".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::OpenTrash,
            title: "回收站…".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::OpenTerminal,
            title: "在当前目录打开终端".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::AddBookmark,
            title: "把当前目录加入书签".to_string(),
            category: "导航".to_string(),
        },
        CmdDef {
            id: CommandId::RemoveBookmark,
            title: "把当前目录从书签移除".to_string(),
            category: "导航".to_string(),
        },
        CmdDef {
            id: CommandId::Properties,
            title: "属性与权限…（⌘I）".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::BatchRename,
            title: "批量重命名…".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::CreateArchive,
            title: "压缩选中项…".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::ExtractArchive,
            title: "解压到当前目录".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::DiskUsage,
            title: "磁盘空间分析…".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::TagSelection,
            title: "给选中项设置标签…".to_string(),
            category: "工具".to_string(),
        },
        CmdDef {
            id: CommandId::CopyClipboard,
            title: "复制选中（⌘C）".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::CutClipboard,
            title: "剪切选中（⌘X）".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::PasteClipboard,
            title: "粘贴到当前目录（⌘V）".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::NewTab,
            title: "新建标签页（⌘T）".to_string(),
            category: "窗口".to_string(),
        },
        CmdDef {
            id: CommandId::CloseTab,
            title: "关闭标签页（⌘W）".to_string(),
            category: "窗口".to_string(),
        },
        CmdDef {
            id: CommandId::ToggleSplit,
            title: "双栏分栏开 / 关（⌘⇧D）".to_string(),
            category: "窗口".to_string(),
        },
        CmdDef {
            id: CommandId::CreateSymlink,
            title: "创建符号链接（同目录）".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::CreateHardlink,
            title: "创建硬链接（同目录，仅文件）".to_string(),
            category: "操作".to_string(),
        },
        CmdDef {
            id: CommandId::SortName,
            title: "按名称排序".to_string(),
            category: "排序".to_string(),
        },
        CmdDef {
            id: CommandId::SortSize,
            title: "按大小排序".to_string(),
            category: "排序".to_string(),
        },
        CmdDef {
            id: CommandId::SortModified,
            title: "按修改时间排序".to_string(),
            category: "排序".to_string(),
        },
        CmdDef {
            id: CommandId::SortKind,
            title: "按类型排序".to_string(),
            category: "排序".to_string(),
        },
    ];
    for (i, u) in users.iter().enumerate() {
        out.push(CmdDef {
            id: CommandId::User(i),
            title: u.name.clone(),
            category: if u.category.trim().is_empty() {
                "自定义".to_string()
            } else {
                u.category.clone()
            },
        });
    }
    for (i, w) in workflows.iter().enumerate() {
        out.push(CmdDef {
            id: CommandId::Workflow(i),
            title: w.name.clone(),
            category: "工作流".to_string(),
        });
    }
    out
}

/// 按查询串过滤命令（大小写不敏感子串匹配）。
fn filtered_commands_in(
    q: &str,
    users: &[mo_app::UserCommand],
    workflows: &[mo_app::Workflow],
) -> Vec<CommandId> {
    let q = q.trim().to_lowercase();
    commands_in(users, workflows)
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
    /// 快速预览独立窗口（`None` = 未开；已开时复用换内容，不重复开）。
    preview_window: Option<WindowHandle<crate::preview::PreviewWindow>>,
    /// 当前生效的主题名（`light` / `dark` / `system` / 自定义 key）。
    ///
    /// 缓存在这里而不是每帧读配置：`AppState::config()` 每次都要读一遍 JSON，
    /// 渲染帧里读会把主线程吃满。
    theme_name: String,
    /// 最近一帧看到的系统外观是否深色（`system` 主题要靠它跟随切换）。
    appearance_dark: bool,
    /// 主题选择器的光标位。
    theme_index: usize,
    /// 界面布局偏好（侧边栏 / 状态栏 / 斑马纹 / 默认视图），启动时从配置读入。
    ui: mo_app::UiPrefs,
    /// 布局设置器的光标位。
    layout_index: usize,
    /// 当前键表（默认键位 + 配置覆盖）。
    keymap: crate::keys::Keymap,
    /// 快捷键设置器的光标位。
    keys_index: usize,
    /// 正在捕获新键位的动作 id（None = 没在捕获）。
    keys_capturing: Option<&'static str>,
    /// 用户自定义命令快照（打开命令面板时刷新；下标即 CommandId::User 的参数）。
    user_commands: Vec<mo_app::UserCommand>,
    /// 已加载的扩展快照（打开扩展管理器时刷新）。
    extensions: Vec<mo_app::extensions::Extension>,
    /// 工作流快照（打开命令面板时刷新；下标即 CommandId::Workflow 的参数）。
    workflows: Vec<mo_app::Workflow>,
    /// 工作流是否正在执行。
    wf_running: bool,
    /// 取消标志：置位后不再开下一步（当前这一步跑完才停）。
    wf_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 最近一次工作流的执行结果。
    wf_report: Option<mo_app::WorkflowReport>,
    /// 同步：当前选项。
    sync_opts: mo_operations::SyncOptions,
    /// 同步：已生成的计划（dry-run 结果）。
    sync_plan: Option<mo_operations::SyncPlan>,
    /// 同步：最近一次执行报告。
    sync_report: Option<mo_operations::SyncReport>,
    /// 同步：是否正在生成计划 / 执行。
    sync_busy: bool,
    /// 扩展管理器的光标位。
    ext_index: usize,
    /// 重复文件查找结果（`None` = 还没跑过）。
    dedup: Option<mo_operations::DedupReport>,
    /// 重复文件查找是否在跑。
    dedup_running: bool,
    /// 取消标志：扫描循环每读一个文件前检查一次。
    dedup_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    /// 「打开方式」二级菜单的候选应用（菜单打开时对文件目标异步查询注册表）。
    pub(crate) open_with_apps: Vec<mo_app::shell::OpenWithApp>,
    /// 「打开方式」二级菜单是否展开（hover 驱动）。
    pub(crate) ctx_submenu_open: bool,
    /// `Modal::Info` 提示框操作按钮的文字；`None` = 默认「知道了」。
    /// 通过 [`RootView::notice_with_ok`] 设置，关闭提示时清回 `None`。
    pub(crate) notice_ok: Option<String>,
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
        // 启动即套用持久化主题：此时还没有 Window，先按浅色基底解析系统外观，
        // 第一帧 render 里的外观跟随逻辑会在拿到真实外观后纠正。
        let theme_name = app.theme_setting();
        crate::theme::set(crate::theme::resolve(
            &theme_name,
            &app.custom_themes(),
            false,
        ));
        crate::theme::apply_component(cx);
        let ui = app.ui_prefs();
        let view = Self {
            panes: vec![Pane::new(panel_with_prefs(app.clone(), &ui))],
            active_pane: 0,
            split: false,
            focus: cx.focus_handle(),
            modal: Modal::None,
            cmd_query: String::new(),
            palette_index: 0,
            search_query: String::new(),
            search_results: Vec::new(),
            preview_window: None,
            theme_name: theme_name.clone(),
            appearance_dark: false,
            // 选择器光标停在当前主题上。
            theme_index: crate::theme::choices(&app.custom_themes())
                .iter()
                .position(|c| *c == theme_name)
                .unwrap_or(0),
            ui: app.ui_prefs(),
            layout_index: 0,
            keymap: crate::keys::Keymap::build(&app.keybindings()),
            keys_index: 0,
            keys_capturing: None,
            user_commands: Vec::new(),
            extensions: Vec::new(),
            ext_index: 0,
            workflows: Vec::new(),
            wf_running: false,
            wf_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            wf_report: None,
            sync_opts: mo_operations::SyncOptions::default(),
            sync_plan: None,
            sync_report: None,
            sync_busy: false,
            dedup: None,
            dedup_running: false,
            dedup_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
            cols: crate::list_columns::ColumnLayout::from_prefs(&app.column_prefs()),
            header_drag: None,
            header_cells: Vec::new(),
            header_cells_owner: (0, 0),
            context_menu: None,
            open_with_apps: Vec::new(),
            ctx_submenu_open: false,
            notice_ok: None,
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
        pane.tabs.push(panel_with_prefs(app.clone(), &self.ui));
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
            let ui = self.ui.clone();
            self.panes
                .push(Pane::new(panel_with_prefs(app.clone(), &ui)));
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
        pane.tabs.push(panel_with_prefs(app.clone(), &self.ui));
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

    /// 弹出一条信息提示（带遮罩模态框）。`ok` 为操作按钮文字，`None` = 默认「知道了」。
    pub(crate) fn notice(
        &mut self,
        text: impl Into<String>,
        ok: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.notice_ok = ok;
        self.modal = Modal::Info(text.into());
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

        if text.is_empty() {
            cx.notify();
            return;
        }
        let target = PathBuf::from(&text);

        // 先同步判定：路径不存在 / 不是文件夹 → 弹窗提示，且不发起导航
        // （open_directory 会先把路径写进历史再读盘失败，提前拦下可免污染历史）。
        if !target.is_dir() {
            let msg = if target.exists() {
                format!("「{text}」不是一个文件夹。")
            } else {
                format!("找不到「{text}」，请检查路径是否正确。")
            };
            self.notice(msg, None, cx);
            return;
        }

        cx.notify();
        cx.spawn(async move |weak, cx| {
            // 竞态兜底：判定通过后、真正读盘前被删除 / 无权限等，仍弹窗提示。
            if let Err(e) = app.open_directory(&target).await {
                let _ = weak.update(cx, |v, cx| {
                    v.notice(format!("打开「{}」失败：{e}", target.display()), None, cx);
                });
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
            // 文件：用**系统默认应用**打开（资源管理器双击语义）。预览仍可
            // 用空格键 / 右键菜单「快速查看」，不再被双击挤占。
            let app = self.app();
            cx.spawn(async move |_weak, _cx| {
                if let Err(e) = app.open_with_system(&path).await {
                    tracing::warn!("打开 {path:?} 失败：{e}");
                }
            })
            .detach();
        }
    }

    /// 在**独立窗口**里快速预览文件。
    ///
    /// 已开着预览窗口就只换内容并置前（Quick Look 习惯：连按空格翻文件不叠窗口）；
    /// 窗口已被用户关掉（`update` 报错）则当作没开重新创建。
    pub(crate) fn show_preview(&mut self, pv: Preview, cx: &mut Context<Self>) {
        if let Some(handle) = self.preview_window {
            // 复用同一个窗口：换内容（标题在 `set_preview` 里一起改）+ 置前。
            let updated = handle.update(cx, |v, window, c| {
                v.set_preview(pv.clone(), window, c);
                window.activate_window();
            });
            if updated.is_ok() {
                return;
            }
            self.preview_window = None;
        }

        let bounds = WindowBounds::centered(size(px(680.0), px(520.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(bounds),
            titlebar: Some(TitlebarOptions {
                // 标题就是文件名：预览窗一多，靠「快速预览」根本分不清哪个是哪个。
                title: Some(pv.title.clone().into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let this = cx.entity().clone();
        // 建窗闭包要把主视图交给 PreviewWindow（方向键回调），单独克隆一份，
        // 免得 `this` 被移走后拿不到回写句柄。
        let root_for_window = this.clone();
        cx.spawn(async move |_weak, cx| {
            let opened = cx.open_window(options, move |_, cx| {
                cx.new(|cx| crate::preview::PreviewWindow::new(pv, root_for_window, cx))
            });
            match opened {
                Ok(handle) => {
                    this.update(cx, |v, _cx| v.preview_window = Some(handle));
                }
                Err(e) => {
                    this.update(cx, |v, cx| {
                        v.modal = Modal::Info(format!("打开预览窗口失败：{e}"));
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// 预览窗口里按方向键：移动列表焦点并换预览内容。
    ///
    /// 焦点的唯一事实来源仍是 app 侧选择模型，所以这里复用主列表的
    /// `move_cursor` + `pull_selection`——预览翻文件时，主列表的高亮与
    /// 滚动同步跟着走（Finder Quick Look 行为）。到边界 `move_cursor`
    /// 会钳住，重复按键停在同一项上。
    pub(crate) fn preview_step(&mut self, step: isize, cx: &mut Context<Self>) {
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            if app.move_cursor(step, false).await.is_none() {
                return; // 空目录：没有可预览的项。
            }
            pull_selection(&app, &this, cx).await;
            let Some(p) = app.selection_paths().await.into_iter().next() else {
                return;
            };
            if let Ok(pv) = app.preview(&p) {
                this.update(cx, |v, cx| v.show_preview(pv, cx));
            }
        })
        .detach();
    }

    // ------------------------------------------------------------ 主题

    /// 系统外观是否深色。
    fn system_dark(window: &Window) -> bool {
        matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        )
    }

    /// 按当前主题名重解析调色板并落到全局（含 gpui-component 的 Theme）。
    ///
    /// 系统外观取 `self.appearance_dark`（每帧 render 校准的缓存），这样主题
    /// 操作不必到处携带 `Window`——命令面板 / 快捷键 / 鼠标点击共用一条路径。
    fn repaint_theme(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let name = self.theme_name.clone();
        let dark = self.appearance_dark;
        crate::theme::set(crate::theme::resolve(&name, &app.custom_themes(), dark));
        crate::theme::apply_component(cx);
        cx.notify();
    }

    /// 切换主题。`persist` 为真时写回配置（选择器里的实时预览传 false）。
    pub(crate) fn apply_theme(&mut self, name: &str, persist: bool, cx: &mut Context<Self>) {
        if persist {
            self.app().set_theme(name);
        }
        self.theme_name = name.to_string();
        self.repaint_theme(cx);
    }

    /// 打开主题选择器（光标停在当前主题）。
    pub(crate) fn open_theme_picker(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let names = crate::theme::choices(&app.custom_themes());
        self.theme_index = names
            .iter()
            .position(|n| *n == self.theme_name)
            .unwrap_or(0);
        self.modal = Modal::Theme;
        cx.notify();
    }

    /// 主题选择器：↑↓ 移动光标并实时预览（不写配置）。
    fn theme_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let app = self.app();
        let names = crate::theme::choices(&app.custom_themes());
        if names.is_empty() {
            return;
        }
        self.theme_index =
            (self.theme_index as isize + delta).rem_euclid(names.len() as isize) as usize;
        self.theme_name = names[self.theme_index].clone();
        self.repaint_theme(cx);
    }

    /// 主题选择器：Enter 确认当前项并持久化。
    fn theme_commit(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let names = crate::theme::choices(&app.custom_themes());
        if let Some(name) = names.get(self.theme_index).cloned() {
            app.set_theme(&name);
            self.theme_name = name;
        }
        self.modal = Modal::None;
        self.repaint_theme(cx);
    }

    /// 主题选择器：Esc 放弃预览，还原到进入前配置里的主题。
    fn theme_cancel(&mut self, cx: &mut Context<Self>) {
        self.theme_name = self.app().theme_setting();
        self.modal = Modal::None;
        self.repaint_theme(cx);
    }

    fn render_theme(&self, entity: &Entity<RootView>) -> Div {
        let app = self.app();
        let names = crate::theme::choices(&app.custom_themes());
        let mut body = div().flex().flex_col().gap(px(2.0)).p(px(8.0));
        for (i, name) in names.iter().enumerate() {
            let selected = i == self.theme_index;
            let on_click_theme = name.clone();
            let mut row = div()
                .id(format!("theme-row-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .p(px(6.0))
                .rounded(px(4.0))
                .bg(if selected {
                    crate::theme::selected_bg()
                } else {
                    crate::theme::surface()
                })
                .text_color(if selected {
                    crate::theme::selected_text()
                } else {
                    crate::theme::text()
                })
                .child(text!(crate::theme::label(name)));
            // 自定义主题额外显示配置里的 key，方便对着 config.json 改。
            if crate::theme::label(name) != *name {
                row = row.child(text!(format!("（{name}）")));
            }
            if *name == self.theme_name {
                row = row.child(text!("· 当前".to_string()));
            }
            let ent = entity.clone();
            row.interactivity().on_click(move |_ev, _window, cx| {
                ent.update(cx, |v, cx| v.apply_theme(&on_click_theme, true, cx));
            });
            body = body.child(row);
        }
        // 当前调色板的角色色卡：预览时一眼看清每档颜色（也是自定义主题的对照表）。
        let p = crate::theme::current();
        let mut swatches = div().flex().flex_row().flex_wrap().gap(px(6.0)).px(px(8.0));
        for (key, zh) in crate::theme::ROLES {
            let Some(c) = p.role(key) else { continue };
            swatches = swatches.child(
                div()
                    .id(format!("theme-swatch-{key}"))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.0))
                    .w(px(46.0))
                    .child(
                        div()
                            .w(px(28.0))
                            .h(px(14.0))
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(crate::theme::divider())
                            .bg(c),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(crate::theme::muted())
                            .child(text!(zh.to_string())),
                    ),
            );
        }
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .pt(px(8.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(crate::theme::muted())
                        .child(text!("当前调色板".to_string())),
                )
                .child(swatches),
        );
        modal_card(
            "主题",
            "",
            body,
            "↑↓ 预览 · Enter 应用 · Esc 还原 · 自定义主题写在 config.json 的 custom_themes",
        )
    }

    /// 打开扩展管理器。
    pub(crate) fn open_extensions_picker(&mut self, cx: &mut Context<Self>) {
        self.extensions = self.app().extensions();
        self.ext_index = 0;
        self.modal = Modal::Extensions;
        cx.notify();
    }

    /// 启停某个扩展（写回它自己的清单），随后刷新扩展列表。
    fn toggle_extension(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(id) = self.extensions.get(i).map(|e| e.manifest.id.clone()) else {
            return;
        };
        let on = !self.extensions.get(i).is_some_and(|e| e.manifest.enabled);
        if let Err(e) = self.app().set_extension_enabled(&id, on) {
            self.modal = Modal::Info(format!("切换扩展「{id}」失败：{e}"));
            cx.notify();
            return;
        }
        self.extensions = self.app().extensions();
        cx.notify();
    }

    fn render_extensions(&self, entity: &Entity<RootView>) -> Div {
        let mut body = div().flex().flex_col().gap(px(2.0)).p(px(8.0));
        if self.extensions.is_empty() {
            body = body.child(
                div()
                    .text_color(theme::muted())
                    .child(text!("（还没有扩展）".to_string())),
            );
            body = body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .child(text!(
                        "把带 manifest.json 的目录放进配置目录下的 extensions/ 即可".to_string()
                    )),
            );
        }
        for (i, e) in self.extensions.iter().enumerate() {
            let m = &e.manifest;
            let selected = i == self.ext_index;
            let mut row = div()
                .id(format!("ext-row-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .p(px(6.0))
                .rounded(px(4.0))
                .bg(if selected {
                    theme::selected_bg()
                } else {
                    theme::surface()
                })
                .text_color(if selected {
                    theme::selected_text()
                } else {
                    theme::text()
                })
                .child(text!(format!(
                    "{}{}（{} 条命令）",
                    m.name,
                    if m.version.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" {}", m.version)
                    },
                    m.commands.len()
                )))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(if selected {
                            theme::selected_text()
                        } else {
                            theme::muted()
                        })
                        .child(text!(if m.enabled { "已启用" } else { "已停用" })),
                );
            let ent = entity.clone();
            row.interactivity().on_click(move |_ev, _window, cx| {
                ent.update(cx, |v, cx| v.toggle_extension(i, cx));
            });
            body = body.child(row);
        }
        modal_card(
            "扩展",
            "",
            body,
            "↑↓ 选择 · Enter 启用 / 停用 · Esc 关闭 · 扩展=清单+外部程序，不往进程里塞代码",
        )
    }

    /// 顺序执行一个工作流（结果进独立模态，逐步回显）。
    ///
    /// 步骤本身是外部程序，跑起来可能要几十秒，所以放 blocking 池、UI 不卡；
    /// 取消语义是「不再开下一步」——中途强杀外部进程既不可靠也容易留下半成品。
    pub(crate) fn run_workflow_at(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(wf) = self.workflows.get(i).cloned() else {
            return;
        };
        if self.wf_running {
            return;
        }
        use std::sync::atomic::Ordering;
        let ctx = {
            let panel = self.panel();
            let selected = panel
                .window
                .iter()
                .filter(|e| panel.selection.is_selected(&e.id))
                .map(|e| e.path.clone())
                .collect();
            mo_app::usercmds::CommandContext {
                dir: panel.path.clone(),
                selected,
            }
        };
        self.wf_running = true;
        self.wf_report = None;
        self.wf_cancel.store(false, Ordering::Relaxed);
        self.modal = Modal::Workflow;
        let app = self.app();
        let cancel = self.wf_cancel.clone();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let report = app.run_workflow(wf, ctx, cancel).await;
            this.update(cx, |v, cx| {
                v.wf_running = false;
                v.wf_report = Some(report);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 关掉工作流模态；正在跑时 Esc 是「不再继续下一步」。
    fn workflow_dismiss(&mut self, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering;
        if self.wf_running {
            self.wf_cancel.store(true, Ordering::Relaxed);
            return;
        }
        self.modal = Modal::None;
        self.wf_report = None;
        cx.notify();
    }

    fn render_workflow(&self) -> Div {
        let mut body = div().flex().flex_col().gap(px(4.0)).p(px(8.0));
        if self.wf_running {
            body = body.child(
                div()
                    .text_color(theme::muted())
                    .child(text!("正在执行…（Esc：不再继续下一步）".to_string())),
            );
            return modal_card("自动化工作流", "", body, "执行在后台进行，可以继续浏览");
        }
        let Some(r) = self.wf_report.clone() else {
            body = body.child(
                div()
                    .text_color(theme::muted())
                    .child(text!("（还没有执行记录）".to_string())),
            );
            return modal_card("自动化工作流", "", body, "Esc 关闭");
        };
        let head = if let Some(f) = r.failed_at {
            format!("「{}」在第 {} 步失败", r.workflow, f + 1)
        } else if r.cancelled {
            format!("「{}」已取消", r.workflow)
        } else if let Some(b) = &r.blocked {
            format!("「{}」未执行：{b}", r.workflow)
        } else {
            format!("「{}」全部 {} 步成功", r.workflow, r.steps.len())
        };
        body = body.child(
            div()
                .text_color(if r.succeeded() {
                    theme::text()
                } else {
                    diff_del_fg()
                })
                .child(text!(head)),
        );
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar();
        for (i, s) in r.steps.iter().enumerate() {
            let mark = if s.ok() { "✓" } else { "✗" };
            let mut col = div().flex().flex_col().px(px(6.0)).py(px(3.0));
            col = col.child(text!(format!("{mark} 第 {} 步  $ {}", i + 1, s.line)));
            if !s.output.text.trim().is_empty() {
                col = col.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::muted())
                        .child(text!(s.output.text.clone())),
                );
            }
            list = list.child(
                div()
                    .id(format!("wf-step-{i}"))
                    .flex()
                    .flex_row()
                    .items_start()
                    .rounded(px(4.0))
                    .bg(if s.ok() {
                        theme::surface()
                    } else {
                        theme::hover_bg()
                    })
                    .child(col.flex_1()),
            );
        }
        body = body.child(list);
        modal_card(
            "自动化工作流",
            "",
            body,
            "Esc 关闭 · 失败即中止，未跑的步骤不会执行",
        )
    }

    // ------------------------------------------------------------ 文件夹同步

    /// 当前目录及其配对目标；没配对、目标已消失、或与源同一个目录都算没有。
    fn sync_pair(&self) -> Option<(PathBuf, PathBuf)> {
        let src = self.panel().path.clone()?;
        let dst = self.app().sync_target(&src)?;
        if dst.as_os_str().is_empty() || !dst.is_dir() || dst == src {
            return None;
        }
        Some((src, dst))
    }

    /// 打开同步面板。
    pub(crate) fn open_sync_picker(&mut self, cx: &mut Context<Self>) {
        if self.panel().path.is_none() {
            self.modal = Modal::Info("当前没有可同步的目录".to_string());
            cx.notify();
            return;
        }
        // 换目录 / 重开面板后旧计划一律作废。
        self.sync_plan = None;
        self.sync_report = None;
        self.sync_busy = false;
        self.modal = Modal::Sync;
        cx.notify();
    }

    /// 把剪贴板里的路径设为当前目录的同步目标。
    ///
    /// 用剪贴板而不是自绘输入框：本应用已有「拷贝路径」（⌥⌘C / 右键菜单），
    /// 在目标目录上拷一次、回来按一下就能配对，比敲一长串绝对路径省事得多。
    fn set_sync_target_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(src) = self.panel().path.clone() else {
            return;
        };
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text().map(|t| t.trim().to_string()))
        else {
            self.modal = Modal::Info("剪贴板里没有文本".to_string());
            cx.notify();
            return;
        };
        if text.is_empty() {
            self.app().set_sync_target(&src, None);
            self.sync_plan = None;
            self.sync_report = None;
            cx.notify();
            return;
        }
        // 允许「拷贝路径」那种一行一个的多行内容：取第一行作为目录。
        let first = text.lines().next().unwrap_or("").trim();
        let dst = PathBuf::from(first);
        if dst == src {
            self.modal = Modal::Info("源目录与目标目录不能是同一个".to_string());
            cx.notify();
            return;
        }
        if !dst.is_dir() {
            self.modal = Modal::Info(format!(
                "剪贴板里的路径不是存在的目录（或是文件）：{first}\n\n\
                 提示：先在目标目录里按 ⌥C「拷贝路径」，再回来点这里。"
            ));
            cx.notify();
            return;
        }
        self.app().set_sync_target(&src, Some(&dst));
        self.sync_plan = None;
        self.sync_report = None;
        cx.notify();
    }

    /// 解除当前目录的配对。
    fn clear_sync_target(&mut self, cx: &mut Context<Self>) {
        if let Some(src) = self.panel().path.clone() {
            self.app().set_sync_target(&src, None);
        }
        self.sync_plan = None;
        self.sync_report = None;
        cx.notify();
    }

    /// 生成同步计划（只读，不动任何文件）。
    pub(crate) fn build_sync_plan(&mut self, cx: &mut Context<Self>) {
        let Some((src, dst)) = self.sync_pair() else {
            self.modal =
                Modal::Info("还没有可用的目标目录：先把它拷贝到剪贴板再点「设为目标」".to_string());
            cx.notify();
            return;
        };
        if self.sync_busy {
            return;
        }
        self.sync_busy = true;
        self.sync_plan = None;
        self.sync_report = None;
        let opts = self.sync_opts;
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let result = app.sync_plan(src, dst, opts).await;
            this.update(cx, |v, cx| {
                v.sync_busy = false;
                match result {
                    Ok(p) => v.sync_plan = Some(p),
                    Err(e) => v.modal = Modal::Info(e),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 执行计划：复制项就地完成，多余文件交回调用方送回收站。
    pub(crate) fn execute_sync(&mut self, cx: &mut Context<Self>) {
        let Some((src, dst)) = self.sync_pair() else {
            return;
        };
        let Some(plan) = self.sync_plan.clone() else {
            return;
        };
        if self.sync_busy || plan.is_empty() {
            return;
        }
        self.sync_busy = true;
        let trashed = plan
            .actions
            .iter()
            .filter(|a| matches!(a, mo_operations::SyncAction::TrashInTarget { .. }))
            .count();
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let (mut report, victims) = app.sync_apply(src, dst, plan).await;
            if !victims.is_empty() {
                // 删除一律走回收站：有进度、可撤销，绝不永久删除。
                app.trash_paths(victims).await;
            }
            report.trashed = trashed.saturating_sub(report.errors.len());
            this.update(cx, |v, cx| {
                v.sync_busy = false;
                // 执行完两侧应已一致，计划作废，避免重复点执行。
                v.sync_plan = None;
                v.sync_report = Some(report);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 循环切换某个同步选项（方向 / 冲突策略 / 是否清理多余）。
    fn sync_cycle(&mut self, which: usize, cx: &mut Context<Self>) {
        use mo_operations::{SyncConflictPolicy as ConflictPolicy, SyncMode};
        match which {
            0 => {
                self.sync_opts.mode = match self.sync_opts.mode {
                    SyncMode::TwoWay => SyncMode::Mirror,
                    SyncMode::Mirror => SyncMode::TwoWay,
                };
            }
            1 => {
                self.sync_opts.conflict = match self.sync_opts.conflict {
                    ConflictPolicy::NewerWins => ConflictPolicy::KeepBoth,
                    ConflictPolicy::KeepBoth => ConflictPolicy::Skip,
                    ConflictPolicy::Skip => ConflictPolicy::NewerWins,
                };
            }
            2 => self.sync_opts.delete_extras = !self.sync_opts.delete_extras,
            _ => return,
        }
        // 选项一变，旧计划就作废：必须重新 dry-run 才允许执行。
        self.sync_plan = None;
        self.sync_report = None;
        cx.notify();
    }

    /// 一行可点击的设置项。
    fn sync_row(
        &self,
        i: usize,
        label: &str,
        value: &str,
        entity: &Entity<RootView>,
    ) -> Stateful<Div> {
        let mut row = div()
            .id(format!("sync-row-{i}"))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .px(px(6.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .text_color(theme::text())
            .hover(|s| s.bg(theme::hover_bg()))
            .child(text!(label.to_string()))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .truncate()
                    .child(text!(value.to_string())),
            );
        let ent = entity.clone();
        row.interactivity().on_click(move |_ev, _window, cx| {
            ent.update(cx, |v, cx| v.sync_cycle(i, cx));
        });
        row
    }

    /// 一个动作按钮。
    fn sync_button(id: &str, label: &str, accent: bool) -> Stateful<Div> {
        div()
            .id(id.to_string())
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .text_size(px(12.0))
            .bg(if accent {
                theme::selected_bg()
            } else {
                theme::hover_bg()
            })
            .text_color(if accent {
                theme::selected_text()
            } else {
                theme::text()
            })
            .child(text!(label.to_string()))
    }

    fn render_sync(&self, entity: &Entity<RootView>) -> Div {
        use mo_operations::{SyncConflictPolicy as ConflictPolicy, SyncMode};

        let src = self.panel().path.clone().unwrap_or_default();
        let paired = self.sync_pair();
        let mut body = div().flex().flex_col().gap(px(4.0)).p(px(8.0));

        body = body.child(
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .truncate()
                .child(text!(format!("源：{}", src.display()))),
        );
        let target_text = match paired.as_ref() {
            Some((_, dst)) => format!("目标：{}", dst.display()),
            None => "目标：未配对（在目标目录按 ⌥⌘C 拷贝路径，再点下面的「设为目标」）".to_string(),
        };
        body = body.child(
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .truncate()
                .child(text!(target_text)),
        );

        let mode = match self.sync_opts.mode {
            SyncMode::TwoWay => "双向合并",
            SyncMode::Mirror => "单向镜像（源 → 目标）",
        };
        let conflict = match self.sync_opts.conflict {
            ConflictPolicy::NewerWins => "较新的覆盖较旧的",
            ConflictPolicy::KeepBoth => "两边都留（旧版改名保留）",
            ConflictPolicy::Skip => "跳过，交给人判断",
        };
        let extras = if self.sync_opts.mode == SyncMode::Mirror {
            if self.sync_opts.delete_extras {
                "目标端多余的移入回收站"
            } else {
                "保留目标端多余文件"
            }
        } else {
            "双向合并不删除"
        };
        body = body.child(self.sync_row(0, "同步方向", mode, entity));
        body = body.child(self.sync_row(1, "内容冲突时", conflict, entity));
        body = body.child(self.sync_row(2, "多余文件", extras, entity));

        // 动作行。
        let mut actions = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .px(px(6.0))
            .pt(px(4.0));

        let ent = entity.clone();
        let mut btn = Self::sync_button("sync-set-target", "设为目标（取剪贴板）", false);
        btn.interactivity().on_click(move |_ev, _window, cx| {
            ent.update(cx, |v, cx| v.set_sync_target_from_clipboard(cx));
        });
        actions = actions.child(btn);

        if paired.is_some() {
            let ent = entity.clone();
            let mut btn = Self::sync_button("sync-clear", "解除配对", false);
            btn.interactivity().on_click(move |_ev, _window, cx| {
                ent.update(cx, |v, cx| v.clear_sync_target(cx));
            });
            actions = actions.child(btn);

            let ent = entity.clone();
            let mut btn = Self::sync_button(
                "sync-plan",
                if self.sync_busy {
                    "请稍候…"
                } else {
                    "生成计划（只读）"
                },
                false,
            );
            btn.interactivity().on_click(move |_ev, _window, cx| {
                ent.update(cx, |v, cx| v.build_sync_plan(cx));
            });
            actions = actions.child(btn);
        }
        body = body.child(actions);

        if let Some(p) = self.sync_plan.clone() {
            let ent = entity.clone();
            let mut run = Self::sync_button("sync-run", "执行同步", true);
            run.interactivity().on_click(move |_ev, _window, cx| {
                ent.update(cx, |v, cx| v.execute_sync(cx));
            });
            body = body.child(div().flex().flex_row().px(px(6.0)).pt(px(2.0)).child(run));
            body = body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .px(px(6.0))
                    .child(text!(format!(
                        "计划 {} 项 · 预计传输 {} · {} 项已相同",
                        p.actions.len(),
                        crate::file_item::format_size(p.total_bytes()),
                        p.identical
                    ))),
            );
            let mut list = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .px(px(6.0));
            if p.is_empty() {
                list = list.child(text!("（两侧已经一致，无需同步）".to_string()));
            }
            for (i, a) in p.actions.iter().enumerate().take(PLAN_LIMIT) {
                list = list.child(
                    div()
                        .id(format!("sync-plan-{i}"))
                        .text_size(px(11.0))
                        .truncate()
                        .child(text!(a.describe())),
                );
            }
            if p.actions.len() > PLAN_LIMIT {
                list = list.child(text!(format!("…共 {} 项", p.actions.len())));
            }
            body = body.child(list);
        } else if let Some(r) = &self.sync_report {
            body = body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .px(px(6.0))
                    .child(text!(format!(
                        "上次执行：搬运 {} 项 · 回收站 {} 项 · 跳过 {} 项 · 失败 {} 项",
                        r.copied,
                        r.trashed,
                        r.skipped,
                        r.errors.len()
                    ))),
            );
            for (i, (rel, err)) in r.errors.iter().enumerate().take(20) {
                body = body.child(
                    div()
                        .id(format!("sync-err-{i}"))
                        .text_size(px(11.0))
                        .px(px(6.0))
                        .truncate()
                        .child(text!(format!("✗ {rel}：{err}"))),
                );
            }
        } else {
            body = body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .px(px(6.0))
                    .child(text!(
                        "配对 → 生成计划过目 → 才执行。计划阶段不改动任何文件。".to_string()
                    )),
            );
        }
        modal_card(
            "文件夹同步",
            "",
            body,
            "计划只读、执行才动手 · 多余文件一律进回收站（⌘Z 可撤销）",
        )
    }

    /// 在当前目录发起一次重复文件查找（结果进独立模态）。
    ///
    /// 扫描在 blocking 池跑，UI 期间可以继续操作；再按一次取消才停下。
    pub(crate) fn start_dedup(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.panel().path.clone() else {
            self.modal = Modal::Info("当前没有可扫描的目录".to_string());
            cx.notify();
            return;
        };
        if self.dedup_running {
            return;
        }
        use std::sync::atomic::Ordering;
        self.dedup_running = true;
        self.dedup = None;
        self.dedup_cancel.store(false, Ordering::Relaxed);
        self.modal = Modal::Duplicates;
        let app = self.app();
        let cancel = self.dedup_cancel.clone();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let report = app.find_duplicates(root, cancel).await;
            this.update(cx, |v, cx| {
                v.dedup_running = false;
                v.dedup = Some(report);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 请求中止正在跑的扫描。
    fn cancel_dedup(&mut self, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering;
        self.dedup_cancel.store(true, Ordering::Relaxed);
        cx.notify();
    }

    /// 把某一组里除第一份以外的副本移入回收站（可撤销，不永久删除）。
    fn dedup_trash_group(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(group) = self.dedup.as_ref().and_then(|r| r.groups.get(i)).cloned() else {
            return;
        };
        let victims = group.paths[1..].to_vec();
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            app.trash_paths(victims).await;
            // 删完把这一组从结果里摘掉，避免用户重复点同一组。
            this.update(cx, |v, cx| {
                if let Some(r) = v.dedup.as_mut() {
                    r.groups.retain(|g| g != &group);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_dedup(&self, entity: &Entity<RootView>) -> Div {
        let mut body = div().flex().flex_col().gap(px(4.0)).p(px(8.0));
        if self.dedup_running {
            body = body.child(
                div()
                    .text_color(theme::muted())
                    .child(text!("正在扫描当前目录…（Esc 取消）".to_string())),
            );
            return modal_card("重复文件", "", body, "扫描在后台进行，可以继续浏览");
        }
        let Some(report) = self.dedup.clone() else {
            return modal_card("重复文件", "", body, "还没有结果");
        };
        body = body.child(
            div()
                .text_size(px(11.0))
                .text_color(theme::muted())
                .child(text!(format!(
                    "{} 组重复 · 可回收 {} · 扫描 {} 项（跳过 {}）{}",
                    report.groups.len(),
                    crate::file_item::format_size(report.total_wasted()),
                    report.scanned,
                    report.skipped,
                    if report.cancelled { "· 已取消" } else { "" }
                ))),
        );
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar();
        if report.groups.is_empty() {
            list = list.child(
                div()
                    .px(px(6.0))
                    .text_color(theme::muted())
                    .child(text!("（没有发现重复文件）".to_string())),
            );
        }
        for (i, g) in report.groups.iter().enumerate() {
            let mut col = div().flex().flex_col().px(px(6.0)).py(px(4.0));
            col = col.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .child(text!(format!(
                        "{} × {}（可回收 {}）",
                        crate::file_item::format_size(g.size),
                        g.paths.len(),
                        crate::file_item::format_size(g.wasted())
                    ))),
            );
            for (j, p) in g.paths.iter().enumerate() {
                col = col.child(
                    div()
                        .text_size(px(12.0))
                        .truncate()
                        // 第一份是「建议保留」，标出来免得用户删错。
                        .child(text!(format!(
                            "{} {}",
                            if j == 0 { "◆" } else { "  " },
                            p.display()
                        ))),
                );
            }
            let ent = entity.clone();
            let mut row = div()
                .id(format!("dedup-group-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .rounded(px(4.0))
                .hover(|s| s.bg(theme::hover_bg()))
                .child(col.flex_1());
            if g.paths.len() > 1 {
                let ent2 = ent.clone();
                let mut btn = div()
                    .id(format!("dedup-trash-{i}"))
                    .flex_shrink_0()
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .hover(|s| s.bg(theme::selected_bg()))
                    .child(text!("删副本".to_string()));
                btn.interactivity().on_click(move |_ev, _window, cx| {
                    ent2.update(cx, |v, cx| v.dedup_trash_group(i, cx));
                });
                row = row.child(btn);
            }
            list = list.child(row);
        }
        body = body.child(list);
        modal_card(
            "重复文件",
            "",
            body,
            "删副本＝移入回收站（⌘Z 可撤销）· ◆ 为建议保留 · Esc 关闭",
        )
    }

    // ------------------------------------------------------------ 快捷键

    /// 执行一个可绑定动作（键表查出来的 id 落到这里）。
    ///
    /// 动作 id 与 `keys::BINDINGS` 一一对应；不认识的 id 静默忽略
    /// （配置里手写的新 id 不该让按键路由崩掉）。
    pub(crate) fn dispatch_action(&mut self, id: &str, cx: &mut Context<Self>) {
        let entity = cx.entity().clone();
        match id {
            "app.quit" => cx.quit(),
            "palette.open" => {
                // 打开面板时刷新一次：用户可能刚改过配置或丢了新清单 / 扩展。
                let exts = selected_ext_names(self.panel());
                self.user_commands = self.app().user_commands(&exts);
                self.workflows = self.app().workflows();
                self.modal = Modal::CommandPalette;
                self.cmd_query.clear();
                self.palette_index = 0;
                cx.notify();
            }
            "search.global" => {
                self.modal = Modal::GlobalSearch;
                self.search_query.clear();
                self.search_results.clear();
                self.palette_index = 0;
                cx.notify();
            }
            "select.all" => {
                let app = self.app();
                cx.spawn(async move |_weak, cx| {
                    app.select_all_visible().await;
                    // app 侧选择是唯一事实来源：全选后回灌 UI 高亮。
                    pull_selection(&app, &entity, cx).await;
                })
                .detach();
            }
            "edit.undo" => self.app().undo(),
            "edit.redo" => self.app().redo(),
            "tab.new" => {
                let pane = self.active_pane;
                self.new_tab(cx, pane);
                cx.notify();
            }
            "tab.close" => {
                let pane = self.active_pane;
                let tab = self.pane().active;
                if !self.close_tab(pane, tab, cx) {
                    cx.notify();
                }
            }
            "tab.prev" => {
                self.cycle_tab(-1);
                cx.notify();
            }
            "tab.next" => {
                self.cycle_tab(1);
                cx.notify();
            }
            "pane.split" => {
                self.toggle_split(Some(cx), None);
                cx.notify();
            }
            "pane.prev" => {
                self.switch_pane(-1);
                cx.notify();
            }
            "pane.next" => {
                self.switch_pane(1);
                cx.notify();
            }
            key @ ("view.list" | "view.grid" | "view.gallery" | "view.columns") => {
                let mode = match key {
                    "view.grid" => ViewMode::Grid,
                    "view.gallery" => ViewMode::Gallery,
                    "view.columns" => ViewMode::Columns,
                    _ => ViewMode::List,
                };
                self.panel_mut().view_mode = mode;
                cx.notify();
            }
            "file.properties" => {
                self.open_properties(cx, None);
                cx.notify();
            }
            "file.duplicate" => {
                let app = self.app();
                // 与右键菜单同语义：只复制当前选中的那些（按可见顺序）。
                let paths = {
                    let panel = self.panel();
                    panel
                        .window
                        .iter()
                        .filter(|e| panel.selection.is_selected(&e.id))
                        .map(|e| e.path.clone())
                        .collect::<Vec<_>>()
                };
                if paths.is_empty() {
                    return;
                }
                cx.spawn(async move |_weak, _cx| {
                    app.duplicate_paths(paths).await;
                })
                .detach();
            }
            "clipboard.copy" => {
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    app.copy_selection_to_clipboard().await;
                })
                .detach();
            }
            "clipboard.cut" => {
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    app.cut_selection_to_clipboard().await;
                })
                .detach();
            }
            "clipboard.paste" => {
                let app = self.app();
                let dest = self.panel().path.clone();
                cx.spawn(async move |_weak, _cx| {
                    let _ = app.paste_clipboard(dest).await;
                })
                .detach();
            }
            "clipboard.copy_path" => {
                // 路径是普通文本，直接进系统剪贴板（不经过内部文件剪贴板）。
                let text = {
                    let panel = self.panel();
                    panel
                        .window
                        .iter()
                        .filter(|e| panel.selection.is_selected(&e.id))
                        .map(|e| e.path.to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            "nav.parent" => {
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    let _ = app.open_parent().await;
                })
                .detach();
            }
            "file.trash" => {
                let app = self.app();
                cx.spawn(async move |_weak, _cx| {
                    app.delete_selection().await;
                })
                .detach();
            }
            "list.rename" => {
                self.open_batch_rename(cx, None);
                cx.notify();
            }
            "list.open" => {
                let app = self.app();
                cx.spawn(async move |_weak, cx| {
                    open_focused(&app, &entity, cx).await;
                })
                .detach();
            }
            "list.preview" => {
                let app = self.app();
                cx.spawn(async move |_weak, cx| {
                    open_quick_look(&app, &entity, cx).await;
                })
                .detach();
            }
            "keys.open" => self.open_keys_picker(cx),
            other => tracing::debug!("未绑定的动作 id：{other}"),
        }
    }

    /// 打开快捷键设置器。
    pub(crate) fn open_keys_picker(&mut self, cx: &mut Context<Self>) {
        self.keys_index = 0;
        self.keys_capturing = None;
        self.modal = Modal::Keys;
        cx.notify();
    }

    /// 设置器里当前聚焦的动作 id。
    fn keys_focused(&self) -> Option<&'static str> {
        crate::keys::BINDINGS.get(self.keys_index).map(|b| b.id)
    }

    /// 把一次按键捕获为当前动作的新键位。
    fn keys_capture(&mut self, combo: &crate::keys::KeyCombo, cx: &mut Context<Self>) {
        let Some(id) = self.keys_capturing else {
            return;
        };
        self.keys_capturing = None;
        if combo.is_modified() {
            // 冲突检查：同一键位不能挂两个动作，否则永远只有先注册的那个响应。
            if let Some(other) = self.keymap.conflict(id, combo) {
                self.modal = Modal::Info(format!(
                    "{other} 已经占用了 {}。\n\n请先改掉那一个，或换个组合键。",
                    combo.format()
                ));
                cx.notify();
                return;
            }
        }
        self.keymap.rebind(id, combo.clone());
        self.app().set_keybinding(id, &combo.spec());
        cx.notify();
    }

    /// 解绑当前动作（键位置空，命中时吞键）。
    fn keys_unbind(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.keys_focused() else {
            return;
        };
        if let Some(combo) = self.keymap.combo_of(id).cloned() {
            self.keymap.unbind(id, combo);
        }
        self.app().set_keybinding(id, "");
        self.keys_capturing = None;
        cx.notify();
    }

    /// 恢复当前动作的默认键位。
    fn keys_reset_one(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.keys_focused() else {
            return;
        };
        self.keymap.reset(id);
        self.app().clear_keybinding(id);
        self.keys_capturing = None;
        cx.notify();
    }

    /// 全部动作回到默认键位。
    fn keys_reset_all(&mut self, cx: &mut Context<Self>) {
        self.keymap.reset_all();
        self.app().reset_keybindings();
        self.keys_capturing = None;
        cx.notify();
    }

    fn render_keys(&self, entity: &Entity<RootView>) -> Div {
        let mut body = div().flex().flex_col().gap(px(1.0)).p(px(6.0));
        body = body.child(
            div()
                .text_size(px(11.0))
                .text_color(theme::muted())
                .px(px(4.0))
                .pb(px(4.0))
                .child(text!(match self.keys_capturing {
                    Some(id) => format!(
                        "请按下要绑给「{}」的组合键（Esc 取消，Delete 解绑）",
                        crate::keys::Keymap::label(id)
                    ),
                    None => "↑↓ 选择 · Enter 捕获新键位 · Delete 解绑 · R 恢复该项默认 · Esc 关闭"
                        .to_string(),
                })),
        );
        let mut list = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar();
        for (i, b) in crate::keys::BINDINGS.iter().enumerate() {
            let selected = i == self.keys_index;
            let capturing = self.keys_capturing == Some(b.id);
            let combo = self
                .keymap
                .combo_of(b.id)
                .map(|c| c.format())
                .unwrap_or_else(|| "（未绑定）".to_string());
            let row = div()
                .id(format!("keys-row-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .px(px(6.0))
                .h(px(22.0))
                .rounded(px(3.0))
                .bg(if capturing {
                    theme::selected_bg()
                } else if selected {
                    theme::hover_bg()
                } else {
                    theme::surface()
                })
                .text_color(if capturing {
                    theme::selected_text()
                } else {
                    theme::text()
                })
                .child(text!(b.label.to_string()))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(if capturing {
                            theme::selected_text()
                        } else {
                            theme::muted()
                        })
                        .child(text!(if capturing {
                            "按下新键位…"
                        } else {
                            &combo
                        })),
                );
            list = list.child(row);
        }
        let mut reset_row = div()
            .id("keys-reset-all")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .p(px(6.0))
            .rounded(px(4.0))
            .text_color(theme::muted())
            .hover(|s| s.bg(theme::hover_bg()))
            .child(text!("恢复全部默认键位".to_string()));
        let reset_ent = entity.clone();
        reset_row.interactivity().on_click(move |_ev, _window, cx| {
            reset_ent.update(cx, |v, cx| v.keys_reset_all(cx));
        });
        body = body.child(list).child(reset_row);
        modal_card(
            "快捷键",
            "",
            body,
            "↑↓ 选择 · Enter 捕获 · Delete 解绑 · R 复位 · Esc 关闭",
        )
    }

    // ------------------------------------------------------------ 布局

    /// 布局设置器的行：`(标题, 当前值)`。最后一行是动作（恢复默认）。
    fn layout_rows(&self) -> Vec<(String, String)> {
        let on = |b: bool| if b { "开" } else { "关" };
        let mode = ViewMode::from_key(&self.ui.view_mode)
            .unwrap_or_default()
            .label();
        vec![
            ("显示侧边栏".to_string(), on(self.ui.sidebar).to_string()),
            ("显示状态栏".to_string(), on(self.ui.status_bar).to_string()),
            ("列表斑马纹".to_string(), on(self.ui.zebra).to_string()),
            (
                "新标签页默认视图".to_string(),
                format!("{mode}（Enter 切换）"),
            ),
            ("恢复默认布局".to_string(), String::new()),
        ]
    }

    /// 执行一条用户自定义命令，输出用信息卡片回显（第四阶段·自定义命令）。
    pub(crate) fn run_user_command_at(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(cmd) = self.user_commands.get(i).cloned() else {
            return;
        };
        // 占位符上下文：当前目录 + 按可见顺序的选中项。
        let ctx = {
            let panel = self.panel();
            let selected = panel
                .window
                .iter()
                .filter(|e| panel.selection.is_selected(&e.id))
                .map(|e| e.path.clone())
                .collect();
            mo_app::usercmds::CommandContext {
                dir: panel.path.clone(),
                selected,
            }
        };
        self.modal = Modal::None;
        let app = self.app();
        let name = cmd.name.clone();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let res = app.run_user_command(cmd, ctx).await;
            this.update(cx, |v, cx| {
                v.modal = Modal::Info(match res {
                    Err(e) => format!("「{name}」未执行：{e}"),
                    Ok((line, out)) => {
                        let code = out
                            .code
                            .map_or_else(|| "无法启动".to_string(), |c| c.to_string());
                        let body = if out.text.trim().is_empty() {
                            "（无输出）".to_string()
                        } else {
                            out.text
                        };
                        format!("「{name}」\n$ {line}\n退出码 {code}\n\n{body}")
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// 打开布局设置器。
    pub(crate) fn open_layout_picker(&mut self, cx: &mut Context<Self>) {
        self.layout_index = 0;
        self.modal = Modal::Layout;
        cx.notify();
    }

    /// 立刻把当前 `ui` 写回配置。
    fn persist_ui(&self) {
        let ui = self.ui.clone();
        self.app().set_ui_prefs(ui);
    }

    /// 触发布局设置器里的一行（开关取反 / 视图循环 / 恢复默认）。
    pub(crate) fn layout_activate(&mut self, index: usize, cx: &mut Context<Self>) {
        match index {
            0 => self.ui.sidebar = !self.ui.sidebar,
            1 => self.ui.status_bar = !self.ui.status_bar,
            2 => self.ui.zebra = !self.ui.zebra,
            3 => {
                // 按当前值循环到下一档视图。
                let next = ViewMode::from_key(&self.ui.view_mode)
                    .unwrap_or_default()
                    .next();
                self.ui.view_mode = next.key().to_string();
            }
            4 => {
                // 恢复默认：配置落盘 + 内存里的列布局也一起复位。
                self.app().reset_layout();
                self.ui = mo_app::UiPrefs::default();
                self.cols = crate::list_columns::ColumnLayout::new();
                self.persist_ui();
                self.modal = Modal::None;
                cx.notify();
                return;
            }
            _ => return,
        }
        self.persist_ui();
        cx.notify();
    }

    /// 直接改单个界面开关（命令面板的三条直达命令用）。
    pub(crate) fn toggle_ui_flag(&mut self, which: u8, cx: &mut Context<Self>) {
        match which {
            0 => self.ui.sidebar = !self.ui.sidebar,
            1 => self.ui.status_bar = !self.ui.status_bar,
            2 => self.ui.zebra = !self.ui.zebra,
            _ => return,
        }
        self.persist_ui();
        cx.notify();
    }

    fn render_layout(&self, entity: &Entity<RootView>) -> Div {
        let mut body = div().flex().flex_col().gap(px(2.0)).p(px(8.0));
        for (i, (label, value)) in self.layout_rows().into_iter().enumerate() {
            let selected = i == self.layout_index;
            let mut row = div()
                .id(format!("layout-row-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .p(px(6.0))
                .rounded(px(4.0))
                .bg(if selected {
                    theme::selected_bg()
                } else {
                    theme::surface()
                })
                .text_color(if selected {
                    theme::selected_text()
                } else {
                    theme::text()
                })
                .child(text!(label))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(if selected {
                            theme::selected_text()
                        } else {
                            theme::muted()
                        })
                        .child(text!(value)),
                );
            let ent = entity.clone();
            row.interactivity().on_click(move |_ev, _window, cx| {
                ent.update(cx, |v, cx| v.layout_activate(i, cx));
            });
            body = body.child(row);
        }
        modal_card(
            "布局",
            "",
            body,
            "↑↓ 选择 · Enter 切换 · 改动即时生效并写入 config.json · Esc 关闭",
        )
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
            // 调列宽在移动过程中已即时生效；这里收尾时把宽度写回配置，
            // 并重绘一次让分隔线从「拖动中」的加粗态回到常态。
            self.app().save_column_prefs(self.cols.to_prefs());
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
                // 列序是布局偏好的一部分：拖完就落盘，重启后保持。
                self.app().save_column_prefs(self.cols.to_prefs());
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
            target: target_path.clone(),
            is_dir,
            selected: paths.len(),
            paths,
        });
        self.ctx_submenu_open = false;

        // 文件目标：异步查「打开方式」候选应用（读注册表），回来后二级菜单可用。
        match target_path.filter(|_| !is_dir) {
            Some(p) => {
                self.open_with_apps.clear();
                let app = self.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    let apps = app.open_with_candidates(&p).await;
                    this.update(cx, |v, cx| {
                        v.open_with_apps = apps;
                        cx.notify();
                    });
                })
                .detach();
            }
            None => self.open_with_apps.clear(),
        }
        cx.notify();
    }

    /// 关闭右键菜单（点空白 / Esc / 执行完动作）。
    pub(crate) fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            self.ctx_submenu_open = false;
            cx.notify();
        }
    }

    /// 展开「打开方式」二级菜单（hover 触发；已展开则幂等）。
    pub(crate) fn open_ctx_submenu(&mut self, cx: &mut Context<Self>) {
        if !self.ctx_submenu_open {
            self.ctx_submenu_open = true;
            cx.notify();
        }
    }

    /// 收起「打开方式」二级菜单。
    pub(crate) fn close_ctx_submenu(&mut self, cx: &mut Context<Self>) {
        if self.ctx_submenu_open {
            self.ctx_submenu_open = false;
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
            A::OpenWith(idx) => {
                // 用二级菜单里选中的应用打开：ProgID 在候选列表里按序号取。
                let Some(p) = target else { return };
                let Some(app_item) = self.open_with_apps.get(idx) else {
                    return;
                };
                let progid = app_item.progid.clone();
                let app = self.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    if let Err(e) = app.open_with_app(&p, &progid).await {
                        this.update(cx, |v, cx| {
                            v.modal = Modal::Info(format!("打开失败：{e}"));
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            A::OpenWithOther => {
                // 系统的「打开方式」选择对话框（openas 动词）。
                let Some(p) = target else { return };
                let app = self.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    if let Err(e) = app.open_with_dialog(&p).await {
                        this.update(cx, |v, cx| {
                            v.modal = Modal::Info(format!("打开失败：{e}"));
                            cx.notify();
                        });
                    }
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
        // 跟随系统：外观翻转时重解析调色板。只在真的变了时才做，避免每帧读配置。
        let dark = Self::system_dark(window);
        if dark != self.appearance_dark {
            self.appearance_dark = dark;
            if self.theme_name == "system" {
                self.repaint_theme(cx);
            }
        }

        let entity = cx.entity().clone();
        let entity_key = entity.clone();

        let visible_panes = if self.split && self.panes.len() > 1 {
            2
        } else {
            1
        };
        // 每个窗格可用的横向宽度：网格 / 画廊按它算列数（uniform_list 必须先知道行数）。
        let viewport_w = window.viewport_size().width.to_f64() as f32;
        let sidebar_w = if self.ui.sidebar { SIDEBAR_WIDTH } else { 0.0 };
        let per_pane_w = ((viewport_w - sidebar_w) / visible_panes as f32 - 24.0).max(160.0);

        // 模态打开时，工具栏 / 状态栏保留，中央区换成模态卡片。
        // 例外：`Modal::Info` 是带遮罩的提示框，下层内容照常渲染（见文末浮层），
        // 所以这里与 `None` 一样渲染正常浏览区。
        let body: Div = match &self.modal {
            Modal::None | Modal::Info(_) => {
                let mut row = div().flex().flex_row().flex_1().min_w_0();
                // 侧边栏可关（配置 `ui.sidebar`）；关掉时不参与宽度计算。
                if self.ui.sidebar {
                    row = row.child(sidebar::render(
                        &self.panel().app,
                        &self.panel().path,
                        &entity,
                    ));
                }
                self.ensure_columns(cx);
                for i in 0..visible_panes {
                    row = row.child(render_pane(self, i, &entity, per_pane_w));
                }
                row
            }
            Modal::CommandPalette => self.render_command_palette(&entity),
            Modal::GlobalSearch => self.render_global_search(&entity),
            Modal::Trash => self.render_trash(),
            Modal::Diff => self.render_diff(),
            Modal::Properties => dialogs::properties(self, &entity),
            Modal::BatchRename => dialogs::batch_rename(self, &entity),
            Modal::Archive => dialogs::archive(self, &entity),
            Modal::DiskUsage => dialogs::disk_usage(self, &entity),
            Modal::Tags => dialogs::tags(&entity, self),
            Modal::Theme => self.render_theme(&entity),
            Modal::Layout => self.render_layout(&entity),
            Modal::Keys => self.render_keys(&entity),
            Modal::Extensions => self.render_extensions(&entity),
            Modal::Duplicates => self.render_dedup(&entity),
            Modal::Workflow => self.render_workflow(),
            Modal::Sync => self.render_sync(&entity),
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
            .child(progress_panel::render(&ops, &app));

        // 状态栏可关（配置 `ui.status_bar`）。
        if self.ui.status_bar {
            root = root.child(status_bar::render(
                panel.visible_count,
                &panel.path,
                &panel.query,
                selection_count,
                self.indexed,
                can_undo,
                can_redo,
            ));
        }

        // 键盘路由：全局快捷键 + 模态内导航 + 输入即过滤。
        root.interactivity().on_key_down(move |ev, _window, cx| {
            let key = ev.keystroke.key.as_str();
            let m = &ev.keystroke.modifiers;
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

            // 全局快捷键：一律查键表（用户可重映射 / 解绑，见 `keys` 模块）。
            //
            // ⚠️ 这里必须是唯一入口：原先每个快捷键一个
            // `if platform && key.eq_ignore_ascii_case("t")` 的硬编码分支，
            // 用户改了键、旧键位照样生效，等于改不动。硬编码已全部删除。
            let combo = crate::keys::KeyCombo::from_keystroke(&ev.keystroke);
            if combo.is_modified() {
                let hit = entity_key.read(cx).keymap.lookup(&combo);
                if let Some(action) = hit {
                    entity_key.update(cx, |v, cx| v.dispatch_action(action, cx));
                    return;
                }
                // 显式解绑的键位要吞掉这次按键：漏下去会触发系统 / 输入组件的默认行为。
                if entity_key.read(cx).keymap.is_unbound(&combo) {
                    return;
                }
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
                handle_modal_key(key, plain, &combo, &entity_key, cx);
                return;
            }

            // 裸键（回车 / 空格 / Delete / F2）同样走键表，用户可改。
            // 没命中才继续往下走结构性按键：退格删过滤词、Esc 清过滤、
            // 方向键移动焦点、单字符输入即过滤——这些不是「动作」，
            // 让它们也进键表反而会把「按退格删一个过滤字符」这种手感弄没。
            if !combo.is_modified() {
                let hit = entity_key.read(cx).keymap.lookup(&combo);
                if let Some(action) = hit {
                    entity_key.update(cx, |v, cx| v.dispatch_action(action, cx));
                    return;
                }
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
                    tracing::info!("[kbd] 命中 type-to-filter 兜底分支 ch={:?}", ch);
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
        // ⚠️ 地址栏编辑态例外：此刻焦点应归地址栏输入框。若在这里抢回来，输入框会
        // 立刻收到 Blur → 触发 end_address_edit，表现为「点编辑闪一下又退回显示态」。
        if !self.focus.is_focused(window) && !self.panel().address_editing {
            cx.focus_self(window);
        }

        // 信息提示：带遮罩的模态框。下层内容照常渲染、透过半透明遮罩可见，但被
        // `.occlude()` 挡住点不到；居中一张卡片。点遮罩空白 / 操作按钮 / Esc 关闭。
        // 按钮文字可经 `notice_with_ok` 自定义，默认「知道了」。
        if let Modal::Info(msg) = self.modal.clone() {
            let label = self
                .notice_ok
                .clone()
                .unwrap_or_else(|| "知道了".to_string());
            root = root.child(render_notice_overlay(&msg, &label, &entity));
        }

        // 右键菜单：绝对定位的浮层，最后挂上去（画在最上层、命中链最前）。
        // 用窗口坐标直接当偏移量——根容器从 (0, 0) 铺满窗口，两者同一套坐标系。
        if let Some(menu) = self.context_menu.clone() {
            let items = crate::context_menu::items(&menu, &self.open_with_apps);
            let vs = window.viewport_size();
            root = root.child(crate::context_menu::render(
                &menu,
                &items,
                (vs.width.to_f64() as f32, vs.height.to_f64() as f32),
                &entity,
                self.ctx_submenu_open,
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
                        zebra: view.ui.zebra,
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
        // 左内边距给标签，右侧**不留**：尾部拖拽带（`drag_filler`）要一直铺到
        // 窗口右缘，把 48px 顶栏里 AppKit 不管的那半截死区全部吃下。
        .pl(px(8.0))
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
        // 测试用（release no-op）：tests/layout.rs 断言拖拽带在「＋」右侧。
        .debug_selector(|| "mo-tab-new".to_string())
        .hover(|s| s.bg(theme::hover_bg()));
    new_btn.interactivity().on_click(move |_, _window, cx| {
        new_entity.update(cx, |v, cx| {
            v.new_tab(cx, pane_idx);
            cx.notify();
        });
    });
    // 「＋」右侧的空白铺一条拖拽带：48px 顶栏只有上沿 ~28pt 归 AppKit，
    // 下面那截原本谁都不管 —— 标签右侧的空白因此拖不动窗口。
    bar.child(new_btn.child(text!("＋".to_string())))
        .child(toolbar::drag_filler())
}

/// 当前选中项的扩展名集合（小写、含点），供扩展的 `when_ext` 条件判断。
fn selected_ext_names(panel: &crate::panel::Panel) -> Vec<String> {
    let mut out: Vec<String> = panel
        .window
        .iter()
        .filter(|e| panel.selection.is_selected(&e.id))
        .filter_map(|e| {
            e.path
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| format!(".{}", x.to_ascii_lowercase()))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 新建面板：套用配置里的默认视图模式（第四阶段·自定义布局）。
///
/// 认不出的 `ui.view_mode` 回落 `List`——用户手改配置写错字符串不该让新标签页崩掉。
fn panel_with_prefs(app: AppState, ui: &mo_app::UiPrefs) -> Panel {
    let mode = ViewMode::from_key(&ui.view_mode).unwrap_or_default();
    Panel::new(app).with_view_mode(mode)
}

/// 模态内按键处理（返回是否已被处理）。
fn handle_modal_key(
    key: &str,
    plain: bool,
    combo: &crate::keys::KeyCombo,
    entity: &Entity<RootView>,
    cx: &mut App,
) {
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
                let n = filtered_commands_in(&v.cmd_query, &v.user_commands, &v.workflows).len();
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
            "space" => entity.update(cx, |v, cx| {
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
            "space" => entity.update(cx, |v, cx| {
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
        Modal::Extensions => match key {
            "escape" => close_modal(entity, cx),
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                v.ext_index = v.ext_index.saturating_sub(1);
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                let n = v.extensions.len();
                if n > 0 {
                    v.ext_index = (v.ext_index + 1).min(n - 1);
                }
                cx.notify();
            }),
            "enter" => entity.update(cx, |v, cx| {
                let i = v.ext_index;
                v.toggle_extension(i, cx);
            }),
            _ => {}
        },
        Modal::Keys => {
            // 捕获态：下一次按键就是新键位（Esc 取消，不绑）。
            let capturing = entity.update(cx, |v, _cx| v.keys_capturing.is_some());
            if capturing {
                if key == "escape" {
                    entity.update(cx, |v, cx| {
                        v.keys_capturing = None;
                        cx.notify();
                    });
                    return;
                }
                let combo = combo.clone();
                entity.update(cx, |v, cx| v.keys_capture(&combo, cx));
                return;
            }
            match key {
                "escape" => close_modal(entity, cx),
                "up" | "arrowup" => entity.update(cx, |v, cx| {
                    v.keys_index = v.keys_index.saturating_sub(1);
                    cx.notify();
                }),
                "down" | "arrowdown" => entity.update(cx, |v, cx| {
                    let n = crate::keys::BINDINGS.len();
                    v.keys_index = (v.keys_index + 1).min(n - 1);
                    cx.notify();
                }),
                "enter" => entity.update(cx, |v, cx| {
                    v.keys_capturing = v.keys_focused();
                    cx.notify();
                }),
                "delete" | "backspace" => entity.update(cx, |v, cx| v.keys_unbind(cx)),
                "r" => entity.update(cx, |v, cx| v.keys_reset_one(cx)),
                _ => {}
            }
        }
        Modal::Layout => match key {
            "escape" => close_modal(entity, cx),
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                let n = v.layout_rows().len();
                if n > 0 {
                    v.layout_index = (v.layout_index + n - 1) % n;
                }
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                let n = v.layout_rows().len();
                if n > 0 {
                    v.layout_index = (v.layout_index + 1) % n;
                }
                cx.notify();
            }),
            "enter" => entity.update(cx, |v, cx| {
                let i = v.layout_index;
                v.layout_activate(i, cx);
            }),
            _ => {}
        },
        Modal::Theme => match key {
            "escape" => entity.update(cx, |v, cx| v.theme_cancel(cx)),
            "up" | "arrowup" => entity.update(cx, |v, cx| v.theme_move(-1, cx)),
            "down" | "arrowdown" => entity.update(cx, |v, cx| v.theme_move(1, cx)),
            "enter" => entity.update(cx, |v, cx| v.theme_commit(cx)),
            _ => {}
        },
        Modal::Workflow => {
            if key == "escape" {
                entity.update(cx, |v, cx| v.workflow_dismiss(cx));
            }
        }
        Modal::Sync => {
            if key == "escape" {
                close_modal(entity, cx);
            }
        }
        Modal::Duplicates => {
            // 正在扫描时 Esc 是「取消扫描」，有结果时 Esc 才是「关掉模态」。
            if key == "escape" {
                entity.update(cx, |v, cx| {
                    if v.dedup_running {
                        v.cancel_dedup(cx);
                    } else {
                        v.modal = Modal::None;
                        v.dedup = None;
                        cx.notify();
                    }
                });
            }
        }
        Modal::Diff | Modal::Info(_) => match key {
            "escape" | "space" => close_modal(entity, cx),
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
        v.notice_ok = None;
        v.cmd_query.clear();
        v.search_query.clear();
        v.search_results.clear();
        v.diff_cache = None;
        v.palette_index = 0;
        cx.notify();
    });
}

/// 命令面板回车：执行选中的命令。
fn on_palette_enter(entity: &Entity<RootView>, cx: &mut App) {
    let (id, app) = entity.update(cx, |v, _cx| {
        let id = filtered_commands_in(&v.cmd_query, &v.user_commands, &v.workflows)
            .get(v.palette_index)
            .copied();
        (id, v.app())
    });
    match id {
        Some(CommandId::User(i)) => {
            entity.update(cx, |v, cx| v.run_user_command_at(i, cx));
        }
        Some(CommandId::Workflow(i)) => {
            entity.update(cx, |v, cx| v.run_workflow_at(i, cx));
        }
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
        Some(CommandId::ThemePicker) => {
            entity.update(cx, |v, cx| {
                // open_theme_picker 自己会把 modal 换成主题选择器。
                v.open_theme_picker(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::FolderSync) => {
            entity.update(cx, |v, cx| {
                v.open_sync_picker(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::FindDuplicates) => {
            entity.update(cx, |v, cx| {
                v.start_dedup(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::ExtensionsPicker) => {
            entity.update(cx, |v, cx| {
                v.open_extensions_picker(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::KeysPicker) => {
            entity.update(cx, |v, cx| {
                v.open_keys_picker(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::LayoutPicker) => {
            entity.update(cx, |v, cx| {
                v.open_layout_picker(cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(
            id @ (CommandId::ToggleSidebar | CommandId::ToggleStatusBar | CommandId::ToggleZebra),
        ) => {
            let which = match id {
                CommandId::ToggleStatusBar => 1,
                CommandId::ToggleZebra => 2,
                _ => 0,
            };
            entity.update(cx, |v, cx| {
                v.modal = Modal::None;
                v.toggle_ui_flag(which, cx);
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(id @ (CommandId::ThemeLight | CommandId::ThemeDark | CommandId::ThemeSystem)) => {
            let name = match id {
                CommandId::ThemeDark => "dark",
                CommandId::ThemeSystem => "system",
                _ => "light",
            };
            entity.update(cx, |v, cx| {
                v.modal = Modal::None;
                v.apply_theme(name, true, cx);
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
                            // 预览是独立窗口：搜索模态照常关掉。
                            v.modal = Modal::None;
                            v.search_query.clear();
                            v.search_results.clear();
                            v.show_preview(pv, cx);
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
        | CommandId::CreateHardlink
        // 主题四则走 UI 层（on_palette_enter），这里只占位保持穷尽。
        | CommandId::ThemePicker
        | CommandId::ThemeLight
        | CommandId::ThemeDark
        | CommandId::ThemeSystem
        | CommandId::LayoutPicker
        | CommandId::KeysPicker
        | CommandId::ExtensionsPicker
        | CommandId::FindDuplicates
        | CommandId::FolderSync
        | CommandId::Workflow(_)
        | CommandId::ToggleSidebar
        | CommandId::ToggleStatusBar
        | CommandId::ToggleZebra
        | CommandId::User(_) => {}
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
                v.show_preview(pv, cx);
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

/// 打开聚焦 / 选中项：目录进入，文件用**系统默认应用**打开。
///
/// ⚠️ 这里绝不能退回「文件走预览」：预览是空格（`list.preview`）的专属语义，
/// 打开键一旦去预览，Windows 上 Enter 和空格就变成同一件事了。macOS 上
/// Enter 更是重命名、⌘↓ 才是打开（见 `keys::default_spec`）。
async fn open_focused(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    // `selection_paths` 无选中时回退聚焦项，正好是键盘光标语义。
    let Some(p) = app.selection_paths().await.into_iter().next() else {
        return; // 空目录：没有聚焦项，静默。
    };
    if p.is_dir() {
        let _ = app.open_directory(&p).await;
        return;
    }
    if let Err(e) = app.open_with_system(&p).await {
        this.update(cx, |v, cx| {
            v.modal = Modal::Info(format!("无法打开 {p:?}：{e}"));
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
        .child(text!(
            id = format!("filter-q-{pane}"),
            format!("🔍 {}", query)
        ))
        .child(div().text_color(theme::muted()).child(text!(
            id = format!("filter-esc-{pane}"),
            "（Esc 清除）".to_string()
        )))
}

// ---------- 模态卡片渲染 ----------

impl RootView {
    fn render_command_palette(&self, _entity: &Entity<RootView>) -> Div {
        let list = filtered_commands_in(&self.cmd_query, &self.user_commands, &self.workflows);
        let idx = self.palette_index;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for (i, id) in list.iter().enumerate() {
            let def = commands_in(&self.user_commands, &self.workflows)
                .into_iter()
                .find(|c| c.id == *id)
                .unwrap();
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

/// 信息提示的模态框：半透明遮罩 + 居中卡片。
///
/// 与旧的「替换中央内容区」式弹窗不同——下层照常渲染、透过遮罩可见，但被
/// `.occlude()` 挡住，模态期间点不到。点遮罩空白或「知道了」关闭（Esc / 空格
/// 见 `handle_modal_key`）。正文区限高可滚动，长消息（如哈希结果）不撑破窗口。
fn render_notice_overlay(msg: &str, ok_label: &str, entity: &Entity<RootView>) -> impl IntoElement {
    let backdrop_close = entity.clone();
    let ok_close = entity.clone();
    div()
        .id("notice-backdrop")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_kit::rgb(0x000000).alpha(0.35))
        .occlude()
        // 点遮罩空白处关闭。
        .on_click(move |_, _window, cx| {
            backdrop_close.update(cx, |v, cx| {
                v.modal = Modal::None;
                v.notice_ok = None;
                cx.notify();
            });
        })
        .child(
            div()
                .id("notice-card")
                .w(px(440.0))
                .flex()
                .flex_col()
                .gap(px(18.0))
                .p(px(22.0))
                .rounded(px(12.0))
                .bg(theme::surface())
                .text_color(theme::text())
                .shadow_lg()
                // 点卡片本身不冒泡到遮罩，否则点正文会误关。
                .on_click(|_, _window, cx| cx.stop_propagation())
                .child(
                    div()
                        .text_size(px(14.0))
                        .max_h(px(360.0))
                        .overflow_y_scrollbar()
                        .child(text!(msg.to_string())),
                )
                .child(
                    div().flex().flex_row().justify_end().child(
                        div()
                            .id("notice-ok")
                            .flex()
                            .items_center()
                            .justify_center()
                            .px(px(18.0))
                            .h(px(30.0))
                            .rounded(px(7.0))
                            // 主色按钮：用品牌蓝 `selected_bg`（`accent` 角色在本主题里是灰）。
                            .bg(theme::selected_bg())
                            .text_color(theme::selected_text())
                            .text_size(px(13.0))
                            .on_click(move |_, _window, cx| {
                                ok_close.update(cx, |v, cx| {
                                    v.modal = Modal::None;
                                    v.notice_ok = None;
                                    cx.notify();
                                });
                            })
                            .child(text!(ok_label.to_string())),
                    ),
                ),
        )
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

/// 相同行底色：跟随内容区底色——深色主题下写死白会把整屏 diff 打回浅色。
fn diff_eq_bg() -> gpui_kit::Rgba {
    crate::theme::surface()
}

/// 删除行底色（浅色档浅红 / 深色档暗红）。
fn diff_del_bg() -> gpui_kit::Rgba {
    if crate::theme::is_dark() {
        gpui_kit::rgb(0x3a2226)
    } else {
        gpui_kit::rgb(0xfdecec)
    }
}

/// 删除行前景（浅色档深红 / 深色档提亮，保证在暗底上读得清）。
fn diff_del_fg() -> gpui_kit::Rgba {
    if crate::theme::is_dark() {
        gpui_kit::rgb(0xff9f9a)
    } else {
        gpui_kit::rgb(0x8f1f1f)
    }
}

/// 新增行底色（浅色档浅绿 / 深色档暗绿）。
fn diff_add_bg() -> gpui_kit::Rgba {
    if crate::theme::is_dark() {
        gpui_kit::rgb(0x1f3325)
    } else {
        gpui_kit::rgb(0xe9f6e9)
    }
}

/// 新增行前景（浅色档深绿 / 深色档提亮）。
fn diff_add_fg() -> gpui_kit::Rgba {
    if crate::theme::is_dark() {
        gpui_kit::rgb(0x8fe0a0)
    } else {
        gpui_kit::rgb(0x1f6b2a)
    }
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
    use std::collections::HashMap;
    use std::path::PathBuf;

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{px, TestAppContext};
    use mo_app::AppState;

    use super::{Modal, RootView};

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
        crate::isolate_config_for_tests();
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
        crate::isolate_config_for_tests();
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
        crate::isolate_config_for_tests();
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
        crate::isolate_config_for_tests();
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
        crate::isolate_config_for_tests();
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
                    .map(|m| crate::context_menu::items(m, &v.open_with_apps).len())
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
                crate::context_menu::items(v.context_menu.as_ref().unwrap(), &v.open_with_apps)
                    .len()
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

    /// 主题选择器：打开时光标停在当前主题；↑↓ 换预览项并真的改到全局调色板；
    /// Esc 还原（不写配置——配置是用户文件，测试绝不落盘）。
    #[test]
    fn theme_picker_previews_and_restores() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        let (names, start_index, start_theme) = cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.open_theme_picker(cx);
                (
                    crate::theme::choices(&v.app().custom_themes()),
                    v.theme_index,
                    v.theme_name.clone(),
                )
            })
        });
        assert_eq!(
            &names[..3],
            ["light", "dark", "system"],
            "内置三档主题必须都在候选里"
        );
        assert_eq!(
            start_theme, names[start_index],
            "光标应停在当前主题上（start={start_index} names={names:?}）"
        );

        // ↓ 预览下一档：调色板真的换了，且没有落盘（theme_name 变了但配置没动）。
        let after_down = cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.theme_move(1, cx);
                v.theme_name.clone()
            })
        });
        assert_ne!(after_down, start_theme, "↓ 之后预览项应当变化");
        let want = crate::theme::resolve(&after_down, &HashMap::new(), false);
        assert_eq!(crate::theme::current(), want, "全局调色板应跟随预览项");

        // Esc 还原到配置里的主题。
        let restored = cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.theme_cancel(cx);
                (v.theme_name.clone(), v.modal.clone())
            })
        });
        assert_eq!(restored.0, start_theme, "Esc 应当还原预览");
        assert_eq!(restored.1, Modal::None, "Esc 之后选择器应当关掉");
        cx.update(|window, cx| window.render_frame(cx));
    }

    /// 组合键要真的经过键表 → 派发这条链路（headless 派发按键，不靠模拟输入）。
    ///
    /// 守的是这次改造的核心风险：键表接上了但按键路由没走它（表现是「改了键
    /// 没反应」）。顺带守住 Windows 的修饰键映射——gpui 在 Windows 上把
    /// `platform` 给的是 Win 键，Ctrl 在 `control` 里。
    #[test]
    fn global_chords_route_through_keymap() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        // `add_window_view` 直接给回 VisualTestContext，按键派发用它。
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();
        cx.run_until_parked();

        // 平台主修饰键的写法：macOS 是 cmd，其它平台是 ctrl。
        let main = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        let tabs = |cx: &mut gpui_kit::VisualTestContext| {
            cx.update(|_window, cx| root.update(cx, |v, _cx| v.pane().tabs.len()))
        };

        // Ctrl/Cmd+T → tab.new
        let before = tabs(cx);
        cx.simulate_keystrokes(&format!("{main}-t"));
        assert_eq!(tabs(cx), before + 1, "{main}-t 应当新建标签页");

        // Ctrl/Cmd+Shift+P → 命令面板
        cx.simulate_keystrokes(&format!("{main}-shift-p"));
        let modal = cx.update(|_window, cx| root.update(cx, |v, _cx| v.modal.clone()));
        assert_eq!(modal, Modal::CommandPalette, "命令面板快捷键应当生效");

        // 改绑之后旧键位必须失效：这是「自定义快捷键」的最低要求。
        cx.update(|_window, cx| {
            root.update(cx, |v, _cx| {
                let mut overrides = HashMap::new();
                overrides.insert("tab.new".to_string(), format!("{main}+alt+t"));
                v.keymap = crate::keys::Keymap::build(&overrides);
            })
        });
        let before = tabs(cx);
        cx.simulate_keystrokes(&format!("{main}-t"));
        assert_eq!(tabs(cx), before, "改绑后旧键位不该还有反应");
        cx.simulate_keystrokes(&format!("{main}-alt-t"));
        assert_eq!(tabs(cx), before + 1, "新键位应当生效");
    }

    /// 键表要真的驱动按键语义：改绑后旧键位失效、解绑吞键，派发动作可用。
    ///
    /// ⚠️ 这里刻意**不写配置文件**：配置目录是整个测试进程共享的一份，
    /// 并发跑的测试会互相读到对方写的配置（表现为随机失败）。
    /// 「配置 → 键表」这一环由 `keys.rs` 的单测覆盖，这里只测「键表 → 行为」。
    #[test]
    fn keymap_drives_lookup_and_dispatch() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        let overrides: HashMap<String, String> = [
            ("tab.new".to_string(), "cmd+alt+n".to_string()),
            ("tab.close".to_string(), String::new()),
        ]
        .into_iter()
        .collect();
        cx.update(|_window, cx| {
            root.update(cx, |v, _cx| {
                v.keymap = crate::keys::Keymap::build(&overrides)
            });
        });

        let probe = |v: &RootView, spec: &str| {
            v.keymap
                .lookup(&crate::keys::KeyCombo::parse(spec).unwrap())
                .map(|s| s.to_string())
        };
        let hits = cx.update(|_window, cx| {
            root.update(cx, |v, _cx| {
                (
                    probe(v, "cmd+alt+n"),
                    probe(v, "cmd+t"),
                    probe(v, "cmd+w"),
                    v.keymap
                        .is_unbound(&crate::keys::KeyCombo::parse("cmd+w").unwrap()),
                )
            })
        });
        assert_eq!(hits.0.as_deref(), Some("tab.new"), "改绑的键位应当命中");
        assert_eq!(hits.1, None, "旧键位应当失效");
        assert_eq!(hits.2, None, "解绑后不该有动作");
        assert!(hits.3, "解绑的键位应当被吞掉，而不是漏给系统");

        // 派发动作仍然可用：tab.new 真的多出一个标签页。
        let before = cx.update(|_w, cx| root.update(cx, |v, _cx| v.pane().tabs.len()));
        cx.update(|_w, cx| {
            root.update(cx, |v, cx| v.dispatch_action("tab.new", cx));
        });
        let after = cx.update(|_w, cx| root.update(cx, |v, _cx| v.pane().tabs.len()));
        assert_eq!(after, before + 1, "tab.new 应当新建标签页");
    }
}
