use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
// `on_prepaint` 挂在 `ElementExt` 上（回收站面板的斑马纹铺满一屏要靠它回写高度）。
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::*;
use mo_app::AppState;
use mo_core::{RenameSpec, SortKey};
use mo_operations::{HashAlgo, OperationHandle, OperationStatus, TrashEntry};
use mo_preview::{Preview, PreviewKind};
use mo_search::SearchHit;

use crate::dialogs;
use crate::list_view;
use crate::listing;
use crate::panel::{ColumnData, Pane, Panel, ViewMode};
use crate::{columns, file_list, grid, progress_panel, sidebar, status_bar, theme, toolbar};

/// 首次同步时抓取的窗口大小。
pub(crate) const INITIAL_WINDOW: usize = 200;

/// 侧边栏宽度：网格视图用它推算每个窗格的可用宽度。
const SIDEBAR_WIDTH: f32 = 188.0;

/// 地址栏编辑态的占位文字（空输入时显示）。
pub(crate) const ADDRESS_PLACEHOLDER: &str = "输入路径，回车跳转";

/// 「连接到服务器」地址框的占位文字（空输入时显示）。
///
/// 回答两件事：现在支持哪些协议、长什么样。写全 scheme 名（含 `davs`）——
/// 「webdav 该输哪个前缀」正是用户最容易卡住的点，详细写法看对话框里的
/// 「使用说明」折叠区。
pub(crate) const CONNECT_ADDRESS_PLACEHOLDER: &str =
    "支持 ftp / sftp / webdav / davs，例：davs://主机/路径";

/// 同步计划最多列出这么多项（再多也只报总数，避免一次画几千行）。
const PLAN_LIMIT: usize = 400;

/// 磁盘地图铺到第几层：再深的块已经小到看不出是谁，点进去再展开更划算
/// （`AppState::usage_tree` 的 `max_depth` 用的就是它）。
pub(crate) const USAGE_MAP_DEPTH: usize = 3;

/// 磁盘地图上值得写名字的最小面积（占画布的比例）：比这还小就只留颜色，
/// 写上去也是被裁掉半截的乱码。
pub(crate) const USAGE_MAP_LABEL_AREA: f32 = 0.006;

/// 传输速度采样的上一次观测（按操作 ID 记账）。
///
/// 进度面板每 ≈150ms 拿一次快照，两次快照的 `done` 差分即是瞬时速度；
/// 小文件场景瞬时值抖得很厉害，用 EMA 平滑后才像人话里的「速度」。
#[derive(Clone, Copy)]
struct OpSample {
    /// 上次观测到的已完成字节数。
    done: u64,
    /// 上次观测的时间点。
    at: Instant,
    /// 指数平滑后的字节速度（B/s）。
    ema: f32,
}

/// 内容搜索的四个开关。
///
/// 单独一个结构而不是四个 bool：渲染四个胶囊、构造 `ContentQuery`、以及
/// 「改过就置 dirty」都要拿同一组值，散成四个字段容易漏改一处。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ContentOptions {
    /// 区分大小写。
    case_sensitive: bool,
    /// 正则模式。
    regex: bool,
    /// 全词匹配。
    whole_word: bool,
    /// 命中行上下各带一行上下文。
    context: bool,
}

impl ContentOptions {
    /// 四个开关在界面上的顺序（渲染与键盘循环都按它）。
    const ORDER: [fn(&mut Self); 4] = [
        |o| o.case_sensitive = !o.case_sensitive,
        |o| o.regex = !o.regex,
        |o| o.whole_word = !o.whole_word,
        |o| o.context = !o.context,
    ];

    fn get(self, i: usize) -> bool {
        match i {
            0 => self.case_sensitive,
            1 => self.regex,
            2 => self.whole_word,
            _ => self.context,
        }
    }

    fn label(i: usize) -> &'static str {
        match i {
            0 => "Aa",
            1 => ".*",
            2 => "整词",
            _ => "上下文",
        }
    }

    fn hint(i: usize) -> &'static str {
        match i {
            0 => "区分大小写",
            1 => "正则",
            2 => "全词匹配",
            _ => "带上下文行",
        }
    }
}

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
    /// 内容搜索（grep）：在一个目录里按文件内容找。
    ContentSearch,
    /// 统一设置窗口：界面 / 外观 / 快捷键三个标签页（见 [`SettingsTab`]）。
    ///
    /// 原先是主题、布局、快捷键三个各开各的弹窗，入口散在命令面板与用户命令里；
    /// 收进一个带标签栏的窗口后入口只换标签页，不再各自为政。
    Settings,
    /// 扩展管理器（列出已加载的扩展，可启停）。
    Extensions,
    /// 连接到服务器（输入远程地址，进入 FTP 等远程浏览）。
    ConnectServer,
    /// 「服务器要求登录」——用户名 + 密码（星号）+ 记住密码。
    ///
    /// 只在服务器**真的拒绝**了匿名登录时出现（`ConnectFailure::NeedsCredentials`），
    /// 所以地址里没写凭据不等于会弹它：匿名能进就直接进了。
    ConnectAuth,
    /// 回收站危险操作确认（清空）。
    ///
    /// 回收站面板**保留在遮罩后面**（渲染上把这一档当成 `Modal::Trash` 画），
    /// Enter / 红色确认按钮执行，Esc / 取消 / 点遮罩回到面板。
    ConfirmTrash(TrashConfirm),
    /// 回收站条目重命名（macOS 惯例：面板里 Enter 就是重命名）。
    ///
    /// 携带被改名的条目（落点路径），新名字暂存在 [`RootView::trash_rename_name`]；
    /// Esc / 取消回到面板，Enter 提交。
    TrashRename(Box<TrashEntry>),
}

/// [`Modal::ConfirmTrash`] 携带的待确认动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrashConfirm {
    /// 清空回收站（条目数在渲染时从 `trash_entries` 现读，避免快照过期）。
    Empty,
}

/// 统一设置窗口（[`Modal::Settings`](Modal)）里的标签页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    /// 界面：侧边栏 / 状态栏 / 斑马纹 / 默认视图 / 图标缩放 / 分组。
    Layout,
    /// 外观：主题实时预览 + 当前调色板色卡。
    Theme,
    /// 快捷键：重映射 / 解绑 / 捕获。
    Keys,
}

impl SettingsTab {
    /// 全部标签页（顺序即 ← / → 循环切换的顺序）。
    const ALL: [SettingsTab; 3] = [SettingsTab::Layout, SettingsTab::Theme, SettingsTab::Keys];

    /// 标签栏上的显示名。
    fn label(self) -> &'static str {
        match self {
            SettingsTab::Layout => "界面",
            SettingsTab::Theme => "外观",
            SettingsTab::Keys => "快捷键",
        }
    }
}

/// 认证弹窗的状态。
///
/// 与 [`Modal::ConnectAuth`] 一起用：`modal` 决定「在不在这个界面」，
/// 这里存界面上的东西。字段合成一个 `Option` 而不是散在 `RootView` 上，
/// 是因为它们必须同生同灭——取消一次漏清一个，下次打开就会带着上次的
/// 用户名 / 错误信息。
pub(crate) struct ConnectAuthState {
    /// 要连的服务器（`scheme://host:port`），提交时原样回传。
    pub endpoint: String,
    /// 上次尝试的失败原因，作为弹窗里的说明文字。
    pub hint: Option<String>,
    /// 用户名 / 密码不对时的错误。
    pub error: Option<String>,
    /// 用户名初值：地址里写了 `ftp://alice@host` 就带过来，别让用户再敲一遍。
    pub user_seed: String,
    /// 用户名输入框。
    pub user: Option<Entity<InputState>>,
    /// 密码输入框（`masked(true)`，绘制层显示成星号）。
    pub pass: Option<Entity<InputState>>,
    /// 两个输入框的事件订阅句柄。
    ///
    /// ⚠️ 必须持有：`Subscription` 一 drop 就退订，回车那一下（这个弹窗里
    /// 最主要的一个键）就再也收不到了。
    pub subs: Vec<Subscription>,
    /// 「记住密码」是否勾上。默认勾上——用户填完凭据多半就是希望下次别再填。
    pub remember: bool,
    /// 是否已经做过**首次**聚焦。
    ///
    /// 之后不再抢焦点：用户名 / 密码之间要能 Tab 切换，点「记住密码」也会把
    /// 焦点带走，每帧强拉回来就打断这些操作了。（地址框那边是另一个尺度——
    /// 只有一个输入框，每帧确保聚焦反而更好用。）
    pub focused: bool,
}

impl ConnectAuthState {
    /// 新建：`detail` 是上次尝试的失败原因（首次进来时是 `None`）。
    fn new(endpoint: String, detail: Option<String>, user_seed: &str) -> Self {
        Self {
            endpoint,
            hint: detail,
            error: None,
            user_seed: user_seed.to_string(),
            user: None,
            pass: None,
            subs: Vec::new(),
            remember: true,
            focused: false,
        }
    }
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
    /// 按**文件内容**搜索（grep）当前目录。
    ContentSearch,
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
    /// 连接到服务器…（FTP 等远程协议）。
    ConnectServer,
    /// 断开当前远程连接，回到本地浏览。
    DisconnectServer,
    /// 在系统的文件管理器里显示（macOS = 在访达中显示）：平台没实现时不进目录。
    RevealInFileManager,
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
    /// 显示 / 隐藏隐藏文件（访达的 ⌘⇧. 那一个开关）。
    ToggleHidden,
    /// 反选：可见条目里没选中的换上来。
    InvertSelection,
    /// 放大图标（网格 / 画廊）。
    ZoomIn,
    /// 缩小图标（网格 / 画廊）。
    ZoomOut,
    /// 图标大小回到 1.0×。
    ZoomReset,
    /// 列表分组循环切换（无 → 类型 → 日期），并落为新标签页默认值。
    GroupingCycle,
    /// 把选中项收进**暂存区**（跨目录累积，见 `mo-app::staging` 的模块文档）。
    StageSelection,
    /// 开合暂存区抽屉。
    ToggleStaging,
    /// 清空暂存区。
    ClearStaging,
    /// 把暂存区整批**复制**到当前目录。
    StagedCopyHere,
    /// 把暂存区整批**移动**到当前目录（提交后清空清单）。
    StagedMoveHere,
    /// 分栏模式下对比两侧目录，按差异给条目着色。
    ToggleCompare,
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
            id: CommandId::ContentSearch,
            title: "按内容搜索（grep）…".to_string(),
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
            id: CommandId::ConnectServer,
            title: "连接到服务器…（FTP / SFTP / WebDAV 等远程协议）".to_string(),
            category: "导航".to_string(),
        },
        CmdDef {
            id: CommandId::DisconnectServer,
            title: "断开远程连接".to_string(),
            category: "导航".to_string(),
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
            id: CommandId::ToggleHidden,
            title: "显示 / 隐藏隐藏文件".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ZoomIn,
            title: "放大图标（网格 / 画廊）".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ZoomOut,
            title: "缩小图标（网格 / 画廊）".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::ZoomReset,
            title: "图标大小还原（1×）".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::GroupingCycle,
            title: "列表分组：无 → 按类型 → 按日期".to_string(),
            category: "外观".to_string(),
        },
        CmdDef {
            id: CommandId::StageSelection,
            title: "收集选中到暂存区".to_string(),
            category: "暂存区".to_string(),
        },
        CmdDef {
            id: CommandId::ToggleStaging,
            title: "暂存区面板（开 / 关）".to_string(),
            category: "暂存区".to_string(),
        },
        CmdDef {
            id: CommandId::StagedCopyHere,
            title: "把暂存区复制到当前目录".to_string(),
            category: "暂存区".to_string(),
        },
        CmdDef {
            id: CommandId::StagedMoveHere,
            title: "把暂存区移动到当前目录".to_string(),
            category: "暂存区".to_string(),
        },
        CmdDef {
            id: CommandId::ClearStaging,
            title: "清空暂存区".to_string(),
            category: "暂存区".to_string(),
        },
        CmdDef {
            id: CommandId::ToggleCompare,
            title: "对比两侧目录（分栏差异着色）".to_string(),
            category: "视图".to_string(),
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
            id: CommandId::InvertSelection,
            title: "反选（选中其余全部）".to_string(),
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
    // 平台原生的那一条（在访达中显示）：**没有实现的平台不列**
    // ——列出来点了只会报「不支持」，不如不出现。
    if mo_platform::supports_reveal() {
        out.push(CmdDef {
            id: CommandId::RevealInFileManager,
            title: format!("{}（选中项 / 当前目录）", mo_platform::reveal_label()),
            category: "操作".to_string(),
        });
    }
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

/// 「选择其他应用…」的选择器：复用命令面板的那层壳（输入即过滤 + ↑↓ + Enter），
/// 只是列表从「命令」换成了「应用」。
///
/// macOS 没有 Windows `openas` 那样的系统对话框，所以 Mo 自己列一份。
pub(crate) struct AppPicker {
    /// 要拿应用打开的**那个文件**。
    pub path: PathBuf,
    /// 系统推荐的在前（LaunchServices 给的候选），其余已装应用在后，已去重。
    pub apps: Vec<mo_app::shell::OpenWithApp>,
}

/// 选择器的过滤：应用名命中即可（大小写不敏感）。
fn filtered_apps(q: &str, apps: &[mo_app::shell::OpenWithApp]) -> Vec<mo_app::shell::OpenWithApp> {
    let q = q.trim().to_lowercase();
    apps.iter()
        .filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q))
        .cloned()
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
    ///
    /// `pub(crate)` 仅为测试注入（见 `crate::inject_usage_tree_for_tests`）。
    pub(crate) modal: Modal,
    /// 命令面板过滤词。
    cmd_query: String,
    /// 命令面板 / 搜索结果的高亮下标。
    palette_index: usize,
    /// 回收站面板的**多选**集合（行下标）。UI 本地状态：`trash_entries` 快照
    /// 内容真的变了才作废（见 `sync_panel` 里的比对——任何总线事件都会重跑
    /// sync，不能见事件就清）。`pub(crate)` 仅为测试探针读取。
    pub(crate) trash_selected: BTreeSet<usize>,
    /// 回收站 shift 连选的锚点行（缺失时退化为单选）。
    trash_anchor: Option<usize>,
    /// 回收站面板的视图模式。列表 / 网格 / 画廊可用；列视图（Miller 列）逐级
    /// 展开目录，对回收站这种平铺列表无意义，入口全部不接（按钮置灰）。
    pub(crate) trash_view_mode: ViewMode,
    /// 回收站条目大小缓存（键 = 实际落点路径）。账本里没有大小，唯一的来源是
    /// 对落点 stat——在后台线程做（性能红线：阻塞 IO 不进主线程），回填后渲染
    /// 只查表。按落点路径键控：还原 / 清空后留下的旧键无害，重进回收站只补缺。
    trash_sizes: HashMap<PathBuf, u64>,
    /// 全局搜索过滤词。
    search_query: String,
    /// 全局搜索结果。
    search_results: Vec<SearchHit>,
    /// 快速预览独立窗口（`None` = 未开；已开时复用换内容，不重复开）。
    pub(crate) preview_window: Option<WindowHandle<crate::preview::PreviewWindow>>,
    /// 预览**代际**：每次 `show_preview` 递增。
    ///
    /// 图片的降采样副本在后台生成，回来时可能已经换了预览对象（翻页）——
    /// 带上代际比对，对不上就丢弃，否则会把上一张的图贴到当前预览上。
    preview_seq: u64,
    /// 当前生效的主题名（`light` / `dark` / `system` / 自定义 key）。
    ///
    /// 缓存在这里而不是每帧读配置：`AppState::config()` 每次都要读一遍 JSON，
    /// 渲染帧里读会把主线程吃满。
    theme_name: String,
    /// 最近一帧看到的系统外观是否深色（`system` 主题要靠它跟随切换）。
    appearance_dark: bool,
    /// 主题选择器的光标位。
    theme_index: usize,
    /// 统一设置窗口当前停在的标签页。
    settings_tab: SettingsTab,
    /// 传输速度采样（进度面板估速用，见 [`OpSample`]）。
    op_stats: HashMap<u64, OpSample>,
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
    /// 回收站条目快照（回收站面板数据源）。`pub(crate)` 仅为测试注入。
    pub(crate) trash_entries: Vec<TrashEntry>,
    /// 回收站面板内容区高度（prepaint 回写）——「斑马纹铺满一屏」算补足行数用，
    /// 与文件列表的 `list_origin` 同一套机制（回收站没有 pane/tab，单独立一个字段）。
    trash_body_h: f32,
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
    /// 回收站条目重命名的新名字（`Modal::TrashRename` 的输入缓冲）。
    pub(crate) trash_rename_name: String,
    /// 磁盘空间分析结果（条形图的数据源）。
    pub(crate) usage: Vec<mo_app::DirUsage>,
    /// 磁盘地图的数据源（懒加载：切到地图视图才建树）。
    pub(crate) usage_tree: Option<mo_app::UsageTree>,
    /// 这一轮分析的是哪个目录（地图下钻后会变）。
    pub(crate) usage_root: Option<std::path::PathBuf>,
    /// 磁盘分析当前停在哪一种视图：`false` = 条形图，`true` = 磁盘地图。
    pub(crate) usage_map: bool,
    /// 分栏对比是否开着（两侧按差异给行上色 + 底部图例条）。
    pub(crate) compare: bool,
    /// 这一轮对比的是哪两个目录（任一变了就重比一次）。
    pub(crate) compare_roots: Option<(PathBuf, PathBuf)>,
    /// 对比统计 `(仅左侧, 仅右侧, 内容不同)`，图例条上显示。
    pub(crate) compare_stats: Option<(usize, usize, usize)>,
    // ---- 内容搜索（grep）----
    /// 内容搜索词。
    pub(crate) content_query: String,
    /// 四个开关（区分大小写 / 正则 / 整词 / 带上下文），渲染成四枚可点的胶囊。
    pub(crate) content_opts: ContentOptions,
    /// 这一轮搜的是哪个目录（打开面板时定下来，之后不再跟着导航变）。
    pub(crate) content_root: Option<PathBuf>,
    /// 最近一次搜索的结果（`None` = 还没搜过）。
    pub(crate) content_report: Option<mo_search::ContentReport>,
    /// 结果行选中下标（在**扁平化的命中行**上走，不跨文件头）。
    pub(crate) content_index: usize,
    /// 后台搜索是否在跑（跑着就显示「搜索中…」，并置灰「搜索」按钮）。
    pub(crate) content_busy: bool,
    /// 改过查询词 / 开关但还没重搜：Enter 这时才是「搜索」，否则是「跳到命中」。
    pub(crate) content_dirty: bool,
    /// 中断旗标：关面板 / 转义 / 改查询时置位，让后台那一轮早点收工。
    pub(crate) content_stop: Arc<AtomicBool>,
    /// 进行中的拖拽（鼠标按下时记录、抬起时结算）。
    pub(crate) drag: Option<DragState>,
    /// 列表视图的鼠标框选（rubber-band）进行态；`None` = 没在框选。
    pub(crate) box_selection: Option<BoxSelection>,
    /// 各 (窗格, 标签页) 列表视图的内容区左上角与高度（窗口坐标），prepaint 回写。
    ///
    /// 框选取坐标用：把鼠标 y 折算成可见行下标需要「列表顶在哪 + 滚了多少」；
    /// 高度给「斑马纹铺满一屏」算行数（`list_fill_rows`）。
    pub(crate) list_origin: std::collections::HashMap<(usize, usize), (f32, f32, f32)>,
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
    /// 左下角传输浮层是否展开（小块点击切换；点外面 / Esc 收起）。
    pub(crate) ops_open: bool,
    /// 底部暂存区抽屉是否展开（收集时自动展开，让用户看见收进去了什么）。
    pub(crate) staging_open: bool,
    /// 「打开方式」二级菜单的候选应用（菜单打开时对文件目标异步查询注册表）。
    pub(crate) open_with_apps: Vec<mo_app::shell::OpenWithApp>,
    /// 「选择其他应用…」的选择器（复用命令面板的那层壳，见 [`AppPicker`]）。
    pub(crate) app_picker: Option<AppPicker>,
    /// 「打开方式」二级菜单是否展开（hover 驱动）。
    pub(crate) ctx_submenu_open: bool,
    /// 「连接到服务器」对话框的地址输入框（对话框开着期间存在，见 `sync_connect_input`）。
    ///
    /// 与地址栏走同一条路——框架的真实 [`InputState`]，而不是自绘的「字符串 +
    /// 假光标 `▏`」。自绘那版没有选区：⌘A / ⌘C / ⌘V 全都落空，按键穿透到本视图
    /// 的键表，变成「全选文件列表」这类浏览区动作（用户报的就是这个）。
    pub(crate) connect_input: Option<Entity<InputState>>,
    /// `connect_input` 的事件订阅句柄。
    ///
    /// ⚠️ 必须持有：gpui 的 `Subscription` 一旦 drop 就退订，回车那一下（对话框里
    /// 最主要的一个键）就再也收不到了。
    pub(crate) connect_sub: Option<Subscription>,
    /// 连接失败时的错误提示（保留对话框展示）。
    pub(crate) connect_error: Option<String>,
    /// 「连接到服务器」对话框里列出的**已记住的服务器**。
    ///
    /// 存快照而不是每帧重读：`saved_servers()` 每次都要读一遍配置文件，而这份
    /// 列表在对话框开着期间不会变（重新打开时再读一次）。
    pub(crate) connect_servers: Vec<mo_app::SavedServer>,
    /// 认证弹窗（用户名 / 密码 / 记住密码）的状态；`None` = 不在认证态。
    pub(crate) connect_auth: Option<ConnectAuthState>,
    /// 「使用说明」折叠区是否展开（默认折叠，展开后列出各协议的地址写法）。
    pub(crate) connect_help_open: bool,
    /// `Modal::Info` 提示框操作按钮的文字；`None` = 默认「知道了」。
    /// 通过 [`RootView::notice`] 的 `ok` 参数设置，关闭提示时清回 `None`。
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

/// 列表视图的鼠标框选（rubber-band）进行态。
///
/// 坐标都是**窗口坐标**（与表头拖拽、右键菜单落点同一套），框选结束后清回 `None`。
/// 起点记录的是按下那一刻的鼠标位置；拖动中实时更新 `cursor`，按 y 带映射成可见
/// 行下标范围。只作用于列表视图——网格 / 画廊 / 列视图的命中几何不同，框选先不覆盖。
pub(crate) struct BoxSelection {
    pub(crate) pane: usize,
    pub(crate) tab: usize,
    /// 是否以「加选」模式进行（按住 ⌘/Ctrl 或 Shift 时不替换既有选择）。
    pub(crate) extend: bool,
    /// 按下起点（窗口坐标）。
    pub(crate) origin: (f32, f32),
    /// 当前光标（窗口坐标）。
    pub(crate) cursor: (f32, f32),
}

/// 橡皮筋两端 y → 行区间 `(min, max)`：行带按**顶边**对齐（floor），矩形与
/// 行带不相交（全在最后一行之下 / 第一行之上）时返回 `None`。
///
/// 抽成纯函数是为了能单测：这半行错位曾经让「列表空白处单击一下」选中
/// 最后一行（round 把行带错开 + clamp 把界外硬折到边界，双坑叠加）。
fn box_row_range(
    y1: f32,
    y2: f32,
    list_top: f32,
    scroll_y: f32,
    count: usize,
) -> Option<(usize, usize)> {
    const ROW_H: f32 = 24.0;
    const PAD: f32 = 12.0;
    // 与 `start_box_selection_if_empty` 同一把折算：`scroll_y` 是 gpui 滚动偏移，
    // 「向下滚为负」，这里同样要减它（见那边关于符号的注释）。
    let at = |y: f32| -> isize { ((y - list_top - PAD - scroll_y) / ROW_H).floor() as isize };
    let a = at(y1);
    let b = at(y2);
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    if hi < 0 || lo >= count as isize {
        return None;
    }
    Some((
        lo.max(0) as usize,
        hi.min(count as isize - 1).max(0) as usize,
    ))
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
            trash_selected: BTreeSet::new(),
            trash_anchor: None,
            search_query: String::new(),
            search_results: Vec::new(),
            preview_window: None,
            preview_seq: 0,
            theme_name: theme_name.clone(),
            appearance_dark: false,
            // 选择器光标停在当前主题上。
            theme_index: crate::theme::choices(&app.custom_themes())
                .iter()
                .position(|c| *c == theme_name)
                .unwrap_or(0),
            settings_tab: SettingsTab::Layout,
            op_stats: HashMap::new(),
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
            trash_body_h: 0.0,
            trash_view_mode: ViewMode::List,
            trash_sizes: HashMap::new(),
            diff_cache: None,
            form_index: 0,
            prop: None,
            rename_spec: RenameSpec::default(),
            rename_paths: Vec::new(),
            archive_name: String::new(),
            trash_rename_name: String::new(),
            usage: Vec::new(),
            usage_tree: None,
            usage_root: None,
            usage_map: false,
            compare: false,
            compare_roots: None,
            compare_stats: None,
            content_query: String::new(),
            content_opts: ContentOptions::default(),
            content_root: None,
            content_report: None,
            content_index: 0,
            content_busy: false,
            content_dirty: true,
            content_stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            drag: None,
            box_selection: None,
            list_origin: std::collections::HashMap::new(),
            cols: crate::list_columns::ColumnLayout::from_prefs(&app.column_prefs()),
            header_drag: None,
            header_cells: Vec::new(),
            header_cells_owner: (0, 0),
            context_menu: None,
            ops_open: false,
            staging_open: false,
            open_with_apps: Vec::new(),
            app_picker: None,
            ctx_submenu_open: false,
            connect_input: None,
            connect_sub: None,
            connect_error: None,
            connect_servers: Vec::new(),
            connect_auth: None,
            connect_help_open: false,
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

    /// 由相邻两次快照的进度差分估每个进行中操作的速度，顺带算剩余时间。
    ///
    /// 只需要「上次到这次搬了多少字节」，所以在渲染路径上差分即可，不用让
    /// 操作层再埋一套计时器。返回 `op_id -> (字节速度 B/s, 预计剩余秒)`；
    /// 首次观测（没有上一次）速度为 0，UI 层就不显示速度。非 Running 的操作
    /// 清样本——暂停期间不累计字节，恢复后从零重新观测，避免把暂停时长摊进速度。
    fn op_speeds(&mut self, ops: &[OperationHandle]) -> HashMap<u64, (f32, f64)> {
        let now = Instant::now();
        let mut out = HashMap::new();
        for op in ops {
            if op.status != OperationStatus::Running {
                self.op_stats.remove(&op.id);
                continue;
            }
            let (done, total) = op.progress;
            let speed = match self.op_stats.get_mut(&op.id) {
                Some(s) => {
                    let dt = now.duration_since(s.at).as_secs_f64();
                    // 采样间隔太短（<50ms）不可信：差分常数级抖动会被放大。
                    if dt >= 0.05 {
                        let inst = (done.saturating_sub(s.done)) as f64 / dt;
                        s.ema = s.ema * 0.6 + inst as f32 * 0.4;
                        s.done = done;
                        s.at = now;
                    }
                    s.ema as f64
                }
                None => {
                    self.op_stats.insert(
                        op.id,
                        OpSample {
                            done,
                            at: now,
                            ema: 0.0,
                        },
                    );
                    0.0
                }
            };
            let eta = if speed > 1.0 && total > done {
                (total - done) as f64 / speed
            } else {
                0.0
            };
            out.insert(op.id, (speed as f32, eta));
        }
        out
    }

    /// 左下角传输浮层开合（小块点击切换）。
    pub(crate) fn toggle_ops_popover(&mut self, cx: &mut Context<Self>) {
        self.ops_open = !self.ops_open;
        cx.notify();
    }

    /// 从视图的快照缓存里摘掉指定操作（乐观更新）。
    ///
    /// 扫帚「一键清除已完成」用：`dismiss_operation` 只删 `OperationManager`
    /// 的账，视图快照要等进度泵下一拍才同步——等那一拍，泵带来的中间快照会
    /// 把浮层打成空、触发自动收起（headless 测试里 manager 是真空的，必现）。
    /// 这里摘完立刻生效，生产与测试行为一致；泵稍后同步来的快照与此一致。
    pub(crate) fn remove_ops_from_snapshot(&mut self, ids: &[u64]) {
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                tab.ops.retain(|op| !ids.contains(&op.id));
            }
        }
    }

    // ---------------------------------------------------------------- 暂存区

    /// 展开 / 收起暂存区抽屉。
    pub(crate) fn toggle_staging(&mut self, cx: &mut Context<Self>) {
        self.staging_open = !self.staging_open;
        cx.notify();
    }

    /// 把当前选择收进暂存区。
    ///
    /// 收完**自动展开**抽屉：看不见结果的操作容易让人以为没生效（收集不像复制
    /// 有进度条，也不像删除会让列表少一行）。
    pub(crate) fn stage_selection(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let this = cx.entity().clone();
        self.staging_open = true;
        cx.spawn(async move |_weak, cx| {
            let added = app.stage_selection().await;
            this.update(cx, |v, cx| {
                if added == 0 {
                    // 多半是「其实没选中」：说清楚，别让人对着空抽屉猜。
                    v.notice("没有可收集的文件：先选中一些条目".to_string(), None, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 抽屉行尾的 ×：移除一条。
    pub(crate) fn unstage(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.app().unstage(&path);
        cx.notify();
    }

    /// 清空暂存区。
    pub(crate) fn clear_staged(&mut self, cx: &mut Context<Self>) {
        self.app().clear_staged();
        cx.notify();
    }

    /// 把暂存区整批投递到当前目录（`move_` = 移动，否则复制）。
    ///
    /// 走 `AppState::paste_staged` 而不是自己拼操作：跨端点（上传 / 下载 / 远程内
    /// 复制）的判据在那一头，UI 这里复制一份就是两份会漂的判据。
    pub(crate) fn paste_staged(&mut self, cx: &mut Context<Self>, move_: bool) {
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let ids = app.paste_staged(None, move_).await;
            this.update(cx, |v, cx| {
                if ids.is_empty() {
                    v.notice("暂存区是空的，或当前目录不可用".to_string(), None, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------------------------------------------------------------- 分栏对比

    /// 开 / 关分栏对比：两侧按差异给行上色，并在下方给出图例。
    pub(crate) fn toggle_compare(&mut self, cx: &mut Context<Self>) {
        if self.compare {
            self.stop_compare(cx);
            return;
        }
        if !self.split || self.panes.len() < 2 {
            self.notice("分栏对比需要两个窗格：先按 ⌘⇧D 分栏".to_string(), None, cx);
            return;
        }
        let left = self
            .panes
            .first()
            .and_then(|p| p.tabs.get(p.active))
            .and_then(|t| t.path.clone());
        let right = self
            .panes
            .get(1)
            .and_then(|p| p.tabs.get(p.active))
            .and_then(|t| t.path.clone());
        match (left, right) {
            (Some(l), Some(r)) => self.run_compare(l, r, cx),
            _ => self.notice("两个窗格都要停在一个目录上".to_string(), None, cx),
        }
    }

    /// 比较 `left` / `right` 两个目录，结果写进两页的 `Panel::diff`。
    fn run_compare(&mut self, left: PathBuf, right: PathBuf, cx: &mut Context<Self>) {
        self.compare = true;
        self.compare_roots = Some((left.clone(), right.clone()));
        self.compare_stats = None;
        let t0 = self.panes.first().map(|p| p.active).unwrap_or(0);
        let t1 = self.panes.get(1).map(|p| p.active).unwrap_or(0);
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let (l, r) = (left.clone(), right.clone());
            // 递归比较走 `std::fs`：几万个文件的活儿不能压在 async worker 上
            // （会排在 UI 的补窗任务前面），与 `analyze_usage` 同一条纪律。
            let res = app
                .spawn_blocking(move || mo_diff::compare_trees(&l, &r))
                .await;
            this.update(cx, |v, cx| match res {
                Ok(Ok(cmp)) => {
                    let (lm, rm) = compare_maps(&cmp, &left, &right);
                    v.compare_stats = Some((cmp.left_only, cmp.right_only, cmp.different));
                    if let Some(p) = v.panel_at_mut(0, t0) {
                        p.diff = Some(Arc::new(lm));
                    }
                    if let Some(p) = v.panel_at_mut(1, t1) {
                        p.diff = Some(Arc::new(rm));
                    }
                    cx.notify();
                }
                Ok(Err(e)) => {
                    v.compare = false;
                    v.compare_roots = None;
                    v.notice(format!("对比失败：{e}"), None, cx);
                }
                Err(e) => {
                    v.compare = false;
                    v.compare_roots = None;
                    v.notice(format!("对比失败：{e}"), None, cx);
                }
            });
        })
        .detach();
    }

    /// 收掉对比：清两页的行状态与图例。
    fn stop_compare(&mut self, cx: &mut Context<Self>) {
        self.compare = false;
        self.compare_roots = None;
        self.compare_stats = None;
        for i in 0..self.panes.len() {
            let t = self.panes[i].active;
            if let Some(p) = self.panel_at_mut(i, t) {
                p.diff = None;
            }
        }
        cx.notify();
    }

    /// 目录变了就重比一次（`sync_panel` 里调：那里才知道导航落地了）。
    ///
    /// 判据是「两页当前路径 != 上次比的那两个」——`sync_panel` 跑得很勤（元数据
    /// 回填也会触发），少了这一步每次都会重扫一遍整棵树。
    fn refresh_compare_if_stale(&mut self, cx: &mut Context<Self>) {
        if !self.compare {
            return;
        }
        let Some((a, b)) = self.compare_roots.clone() else {
            return;
        };
        let left = self
            .panes
            .first()
            .and_then(|p| p.tabs.get(p.active))
            .and_then(|t| t.path.clone());
        let right = self
            .panes
            .get(1)
            .and_then(|p| p.tabs.get(p.active))
            .and_then(|t| t.path.clone());
        if left.as_deref() == Some(a.as_path()) && right.as_deref() == Some(b.as_path()) {
            return;
        }
        match (left, right) {
            (Some(l), Some(r)) => self.run_compare(l, r, cx),
            // 有一边的目录没了（关了分栏 / 关了标签页）：对比失去了对象。
            _ => self.stop_compare(cx),
        }
    }

    /// 跳到下一个 / 上一个**有差异**的条目（`step` 为 +1 / -1）。
    pub(crate) fn jump_diff(&mut self, cx: &mut Context<Self>, step: isize) {
        let pane = self.active_pane;
        let tab = self.panes.get(pane).map(|p| p.active).unwrap_or(0);
        let Some(panel) = self.panel_at(pane, tab) else {
            return;
        };
        let Some(map) = panel.diff.clone() else {
            return;
        };
        let focused = panel.selection.focused();
        // 窗口快照按**列表行**排（含组头），跳的行号与滚动用的是同一个空间。
        let rows = panel.window.clone();
        if rows.is_empty() {
            return;
        }
        let cur = focused
            .and_then(|f| {
                rows.iter()
                    .position(|r| matches!(r, mo_app::WindowRow::Entry(e) if e.id == f))
            })
            .unwrap_or(0);
        let n = rows.len();
        let order: Vec<usize> = if step > 0 {
            (1..=n).map(|k| (cur + k) % n).collect()
        } else {
            (1..=n).map(|k| (cur + n - k % n) % n).collect()
        };
        let hit = order.into_iter().find(|&i| {
            rows[i]
                .entry()
                .and_then(|e| map.get(&e.path))
                .is_some_and(|s| !matches!(s, mo_diff::TreeStatus::Identical))
        });
        let Some(hit) = hit else {
            return;
        };
        let Some(entry) = rows[hit].entry() else {
            return;
        };
        let id = entry.id;
        // 先滚到位（行下标只有 UI 侧知道），再让 app 侧选中——顺序反了会先选中
        // 再滚，中间那一帧的高亮会停在旧位置。
        if let Some(p) = self.panel_at_mut(pane, tab) {
            p.scroll
                .scroll_to_item(hit, gpui_kit::ScrollStrategy::Nearest);
        }
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            app.select(id).await;
            pull_selection(&app, &this, cx).await;
        })
        .detach();
    }

    // ---------------------------------------------------------------- 内容搜索

    /// 打开内容搜索面板：范围就定在**当前目录**（打开那一刻定死）。
    ///
    /// 范围不跟着导航漂移是刻意的：搜索过程中用户会想点开结果看看，若 range
    /// 跟着当前目录走，一次跳转就把已经搜出来的结果作废了。
    pub(crate) fn open_content_search(&mut self, cx: &mut Context<Self>) {
        let root = self
            .panel()
            .path
            .clone()
            .or_else(|| self.panel().opening.clone());
        let Some(root) = root else {
            self.notice("当前没有打开的目录".to_string(), None, cx);
            return;
        };
        self.content_root = Some(root);
        self.content_report = None;
        self.content_index = 0;
        self.content_dirty = true;
        self.modal = Modal::ContentSearch;
        cx.notify();
    }

    /// 跑一次内容搜索（后台 blocking）。
    ///
    /// 只在按 Enter 时跑，不做「输入即搜」：这一头要真读每个文件的字节，
    /// 每敲一个字扫一遍整棵子树是拿 IO 换回显，会把机器占死。
    fn run_content_search(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.content_root.clone() else {
            return;
        };
        if self.content_query.is_empty() {
            self.notice("先输入要找的内容".to_string(), None, cx);
            return;
        }
        // 上一轮还在跑就先叫停：它的结果已经过期，回来也会被这一轮覆盖。
        self.content_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        self.content_stop = stop.clone();
        self.content_busy = true;
        self.content_dirty = false;
        self.content_index = 0;

        let mut q = mo_search::ContentQuery::new(&self.content_query);
        q.case_sensitive = self.content_opts.case_sensitive;
        q.regex = self.content_opts.regex;
        q.whole_word = self.content_opts.whole_word;
        q.context = if self.content_opts.context { 1 } else { 0 };
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let res = app.content_search(root, q, stop).await;
            this.update(cx, |v, cx| {
                v.content_busy = false;
                match res {
                    Ok(report) => v.content_report = Some(report),
                    Err(e) => {
                        v.content_report = None;
                        v.notice(e.to_string(), None, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 切一个搜索开关（区分大小写 / 正则 / 整词 / 上下文）。
    fn toggle_content_opt(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(f) = ContentOptions::ORDER.get(i) {
            f(&mut self.content_opts);
            // 改了开关就等于改了问题：Enter 该重搜，而不是跳到旧结果上。
            self.content_dirty = true;
            cx.notify();
        }
    }

    /// 扁平化的命中行：`(文件下标, 行下标)`。
    ///
    /// ↑↓ 走的是这个一维序列（跳过纯上下文行——它们只是陪衬，不该占用一次
    /// 按键）。文件头不进序列：它不可跳，只在渲染时作为分组标题出现。
    fn content_hits(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        if let Some(r) = &self.content_report {
            for (fi, f) in r.files.iter().enumerate() {
                for (li, l) in f.lines.iter().enumerate() {
                    if !l.context {
                        out.push((fi, li));
                    }
                }
            }
        }
        out
    }

    /// 打开当前选中的命中：跳到它所在目录并选中该文件。
    ///
    /// 跳目录而不是直接 `open_entry`：命中多半是文本文件，双击语义（打开）
    /// 会把它当可执行文件丢给系统，那不是用户在搜索结果里按 Enter 想要的。
    fn open_content_hit(&mut self, cx: &mut Context<Self>) {
        let hits = self.content_hits();
        let Some((fi, _li)) = hits.get(self.content_index).copied() else {
            return;
        };
        let Some(path) = self
            .content_report
            .as_ref()
            .and_then(|r| r.files.get(fi))
            .map(|f| f.path.clone())
        else {
            return;
        };
        let dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(path.clone());
        self.modal = Modal::None;
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            // 内容搜索只搜本机，所以走本地入口（与全局搜索命中目录同理）。
            let _ = app.open_local(&dir).await;
            // 目录读回来后按路径选中：选择只认 FileId，换算在 app 侧
            // （`AppState::select_path`）。找不到就只跳目录，不硬选。
            if app.select_path(&path).await {
                pull_selection(&app, &this, cx).await;
            }
        })
        .detach();
    }

    /// 关掉内容搜索面板（顺手叫停在跑的那一轮）。
    fn close_content_search(&mut self, cx: &mut Context<Self>) {
        self.content_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.content_busy = false;
        self.modal = Modal::None;
        cx.notify();
    }

    /// 收起传输浮层（点浮层外 / Esc）。本来就收着时不动，也不触发重绘。
    pub(crate) fn close_ops_popover(&mut self, cx: &mut Context<Self>) {
        if self.ops_open {
            self.ops_open = false;
            cx.notify();
        }
    }

    /// 当前是否停在回收站面板（含其上的确认卡 / 重命名卡）。
    pub(crate) fn is_in_trash(&self) -> bool {
        matches!(
            self.modal,
            Modal::Trash | Modal::ConfirmTrash(_) | Modal::TrashRename(_)
        )
    }

    /// 切换「当前活动表面」的视图模式：回收站面板开着切回收站，否则切浏览面板。
    /// 工具栏按钮与 `view.*` 动作共用这一条收口（两者不许各写一套判断）。
    pub(crate) fn set_view_mode_for_active_surface(&mut self, mode: ViewMode) {
        if self.is_in_trash() {
            // 列视图对平铺的回收站无意义：忽略（按钮已置灰，这里是兜底）。
            if !matches!(mode, ViewMode::Columns) {
                self.trash_view_mode = mode;
            }
        } else {
            self.panel_mut().view_mode = mode;
        }
    }

    /// 后台回填回收站条目大小（表头「大小」列的数据源）。
    ///
    /// 账本（`index.json`）没记大小，唯一来源是对**实际落点**做 stat——阻塞 IO
    /// 按红线放后台线程（[`AppState::spawn_blocking`]），回填后 notify 重绘；渲染
    /// 路径只查 [`RootView::trash_sizes`]，永远不碰文件系统。目录不 stat（Finder
    /// 对文件夹的大小同样是「—」）。缓存按落点路径键控、只补缺失的键：还原 /
    /// 清空后留下的旧键无害，重进回收站或重拉列表后新条目自动补齐。
    ///
    /// ⚠️ fire-and-forget：调用方（开面板 / sync）**不等它**。回填若挂在
    /// `sync_panel` 的 await 链上，每轮同步都会多出一个外部唤醒点，headless 的
    /// 确定性调度器等不到就返回（实测：双击打开目录后列表断言直接红）。
    fn ensure_trash_sizes(&mut self, cx: &mut Context<Self>) {
        let missing: Vec<PathBuf> = self
            .trash_entries
            .iter()
            .filter(|e| !e.is_dir && !self.trash_sizes.contains_key(&e.trashed))
            .map(|e| e.trashed.clone())
            .collect();
        if missing.is_empty() {
            return;
        }
        let app = self.app();
        cx.spawn(async move |this, cx| {
            let measured = app
                .spawn_blocking(move || {
                    missing
                        .into_iter()
                        .map(|p| {
                            let size = std::fs::metadata(&p).map(|m| m.len()).ok();
                            (p, size)
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default();
            this.update(cx, |v, cx| {
                for (p, size) in measured {
                    if let Some(size) = size {
                        v.trash_sizes.insert(p, size);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 打开回收站面板（侧栏入口 / 命令面板共用同一条路）。
    pub(crate) fn open_trash_panel(&mut self, cx: &mut Context<Self>) {
        self.trash_entries = self.app().trash_list();
        self.modal = Modal::Trash;
        self.palette_index = 0;
        // 不默认选中第一条（用户要求）：进来什么都没选，第一眼干干净净。
        self.trash_selected.clear();
        self.trash_anchor = None;
        // 大小列：后台 stat 落点回填（fire-and-forget，见 `ensure_trash_sizes`）。
        self.ensure_trash_sizes(cx);
        cx.notify();
    }

    /// 回收站 shift 连选：以锚点为起点**覆盖**到 `i`（锚点缺失退化为单选）。
    fn trash_select_range(&mut self, i: usize) {
        let a = self.trash_anchor.unwrap_or(i);
        let (lo, hi) = if a <= i { (a, i) } else { (i, a) };
        self.trash_selected = (lo..=hi).collect();
    }

    /// 回收站 ↑↓ 移动游标：普通移动 = 单选替换（锚点跟过去）；
    /// `extend`（shift）= 保持锚点、从锚点覆盖到新游标。
    fn trash_move_cursor(&mut self, delta: isize, extend: bool) {
        let n = self.trash_entries.len();
        if n == 0 {
            return;
        }
        let next = (self.palette_index as isize + delta).clamp(0, n as isize - 1) as usize;
        self.palette_index = next;
        if extend {
            if self.trash_anchor.is_none() {
                self.trash_anchor = Some(next);
            }
            self.trash_select_range(next);
        } else {
            self.trash_selected.clear();
            self.trash_selected.insert(next);
            self.trash_anchor = Some(next);
        }
    }

    /// 回收站 ⌘/Ctrl+A：全选。
    fn trash_select_all(&mut self) {
        let n = self.trash_entries.len();
        self.trash_selected = (0..n).collect();
    }

    /// 导航类入口（侧栏 / 工具栏 / 地址栏）发起跳转时，把次级视图退回浏览态。
    ///
    /// 用户点「桌面」是要**去桌面**，不是「下层面板换一页、人还留在回收站里」
    /// （用户报：从回收站点侧栏，地址栏变了但列表没变）。命令面板路径在
    /// `dispatch_action` 里已自带关闭；侧栏直接调 `AppState`、工具栏走
    /// `spawn_nav`，这两条路此前都绕过了它。B 类对话框有遮罩挡着点不到侧栏，
    /// 无条件置 `None` 是安全的。
    pub(crate) fn leave_secondary_view(&mut self, cx: &mut Context<Self>) {
        if self.modal != Modal::None {
            self.modal = Modal::None;
            self.palette_index = 0;
            self.cmd_query.clear();
            cx.notify();
        }
    }

    /// 全部窗格 / 标签页的 `AppState`。
    ///
    /// 「显示隐藏文件」这类**全局开关**要逐个标签页同步：每个标签页各持一份
    /// `AppState`，而开关在 `AppState` 里是进程内镜像（热路径不能每次读盘），
    /// 只改当前那一份就会出现「切到另一个标签页又变回去」。
    pub(crate) fn all_apps(&self) -> Vec<AppState> {
        let mut out = Vec::new();
        for pane in &self.panes {
            for tab in &pane.tabs {
                out.push(tab.app.clone());
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
        // 关标签页**不动远程连接**：用户纠正过的语义是「关标签页不断开，只有退出应用
        // 才断开」，所以连接活在进程级 `SessionRegistry` 里，比标签页活得久——侧边栏
        // 那条列表在别的标签页里照样看得见、点得回去。
        //
        // 但后台泵得停：`Panel` 掉出 `Vec` 并不等于 `AppState` 被释放，泵各自握着一份
        // 克隆且在 `loop` 里永不返回，不置停止位就一直空转着。
        if let Some(tab) = pane.tabs.get(tab_idx) {
            tab.app.stop_pumps();
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

    // ------------------------------------------------------------ 连接到服务器

    /// 打开「连接到服务器」对话框：清空上次的错误与认证态，准备接收地址。
    ///
    /// 地址输入框不在这里建——建 `InputState` / 聚焦都要 `Window`，而这个方法的
    /// 调用点未必有（命令面板那条路就没有）。交给 [`RootView::sync_connect_inputs`]，
    /// 由 `render` 每帧收口。
    pub(crate) fn open_connect_dialog(&mut self, cx: &mut Context<Self>) {
        self.connect_error = None;
        self.connect_auth = None;
        // 「记住的服务器」在打开这一刻取一次快照：列在下面的那几行要能点，
        // 而每帧重读一遍配置文件没必要（列表在对话框开着期间不会变）。
        self.connect_servers = self.app().saved_servers();
        self.modal = Modal::ConnectServer;
        cx.notify();
    }

    /// 连接相关的输入框：开着就确保存在、握着焦点；关了就丢掉。
    ///
    /// 放在 `render` 里而不是「打开 / 关闭」两处：
    /// * 入口未必有 `Window`（同上），而建输入框 / 聚焦都离不开它；
    /// * 关闭有多条路（Esc / 取消 / 点遮罩 / 连接成功），在这里统一收口不会漏。
    fn sync_connect_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_connect_address(window, cx);
        self.sync_connect_auth(window, cx);
    }

    /// 地址对话框的输入框。
    ///
    /// **每帧都确保聚焦**而不是只在建的时候聚焦一次：对话框里点一下空白或按钮
    /// 会把焦点带走，那之后打字就没反应了。（认证弹窗有多个控件，尺度不同，
    /// 见 `ConnectAuthState::focused` 的说明。）
    ///
    /// 焦点必须在输入框上，键盘穿透那个 bug 才算真的修好：⌘A / ⌘C / ⌘V / ⌘Z 由
    /// 输入组件在**绑定阶段**消费掉（早于本视图的 `on_key_down`），落到浏览区
    /// 那边就成了「全选后面的文件」。
    fn sync_connect_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal != Modal::ConnectServer {
            // 关掉了：实体与订阅一起丢（下次打开重建，上一轮的地址、选区、
            // 撤销历史都不跟过来）。
            self.connect_input = None;
            self.connect_sub = None;
            return;
        }
        if self.connect_input.is_none() {
            let state =
                cx.new(|cx| InputState::new(window, cx).placeholder(CONNECT_ADDRESS_PLACEHOLDER));
            let sub = cx.subscribe_in(
                &state,
                window,
                |this: &mut Self, _state, ev: &InputEvent, _window, cx| {
                    // 单行输入框把回车报成 `PressEnter`（它在绑定阶段就吃掉了
                    // `enter`，根视图那边收不到），这里接住它去连接。
                    if matches!(ev, InputEvent::PressEnter { .. }) {
                        this.connect_submit(cx);
                    }
                },
            );
            self.connect_input = Some(state);
            self.connect_sub = Some(sub);
        }
        let Some(state) = self.connect_input.clone() else {
            return;
        };
        if !state.read(cx).focus_handle(cx).is_focused(window) {
            state.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// 认证弹窗的用户名 / 密码输入框。
    ///
    /// 密码框靠 `InputState::masked(true)`——**「密码显示成星号」就是它**：掩码
    /// 发生在绘制层，`value()` 取到的仍是明文，提交时直接拿去登录。
    fn sync_connect_auth(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 不在认证态：状态一起丢掉，免得下次进来带着上次的用户名 / 错误。
        if self.modal != Modal::ConnectAuth {
            self.connect_auth = None;
            return;
        }
        let Some(auth) = self.connect_auth.as_mut() else {
            return;
        };
        if auth.user.is_none() {
            let user = cx.new(|cx| InputState::new(window, cx));
            let pass = cx.new(|cx| InputState::new(window, cx).masked(true));
            if !auth.user_seed.is_empty() {
                let seed = auth.user_seed.clone();
                user.update(cx, |s, cx| s.set_value(seed, window, cx));
            }
            // 两个框都接回车——用户在哪个框上按都该发起连接。
            let sub_user = cx.subscribe_in(
                &user,
                window,
                |this: &mut Self, _s, ev: &InputEvent, _w, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. }) {
                        this.connect_auth_submit(cx);
                    }
                },
            );
            let sub_pass = cx.subscribe_in(
                &pass,
                window,
                |this: &mut Self, _s, ev: &InputEvent, _w, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. }) {
                        this.connect_auth_submit(cx);
                    }
                },
            );
            auth.subs = vec![sub_user, sub_pass];
            auth.user = Some(user);
            auth.pass = Some(pass);
        }

        // 只在刚打开时聚焦一次：用户名空就停在用户名上；已经有用户名（地址里
        // 带过来的，或上次试过）就直接跳到密码——「密码敲错了再试一次」是这个
        // 弹窗的主路径，让用户少按一次 Tab。
        if !auth.focused {
            auth.focused = true;
            let (user, pass) = (auth.user.clone(), auth.pass.clone());
            let user_empty = user
                .as_ref()
                .is_some_and(|s| s.read(cx).value().trim().is_empty());
            match (user_empty, user, pass) {
                (true, Some(u), _) => u.update(cx, |s, cx| s.focus(window, cx)),
                (false, _, Some(p)) => p.update(cx, |s, cx| s.focus(window, cx)),
                _ => {}
            }
        }
    }

    /// 用对话框里输入的地址发起连接。
    pub(crate) fn connect_submit(&mut self, cx: &mut Context<Self>) {
        let text = self
            .connect_input
            .as_ref()
            .map(|s| s.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        self.start_connect(text, cx);
    }

    /// 点「已记住的服务器」里的一行：直接用存下的凭据连这台。
    pub(crate) fn connect_to(&mut self, endpoint: String, cx: &mut Context<Self>) {
        self.start_connect(endpoint, cx);
    }

    /// 发起一次连接，把结果落到 UI 状态上。
    ///
    /// 地址框回车与点服务器列表共用这条路（真正的建 socket 在 `connect_remote`
    /// 内部的 blocking 池里，这里只管等）。
    fn start_connect(&mut self, address: String, cx: &mut Context<Self>) {
        let app = self.app();
        self.connect_error = None;
        if let Some(auth) = self.connect_auth.as_mut() {
            auth.error = None;
        }
        cx.notify();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let outcome = app.connect_remote(&address).await;
            let connected = outcome.is_ok();
            this.update(cx, |v, cx| {
                v.on_connect_result(outcome, cx);
                if connected {
                    // 连上了就把这台记进列表（**不写密码**——密码要不要记住由
                    // 认证弹窗里那个勾决定，这里不替用户做主）。
                    v.remember_active_server(cx);
                }
            });
        })
        .detach();
    }

    /// 认证弹窗的「连接 / 回车」：带上刚填的凭据再连一次。
    pub(crate) fn connect_auth_submit(&mut self, cx: &mut Context<Self>) {
        let Some(auth) = self.connect_auth.as_ref() else {
            return;
        };
        let endpoint = auth.endpoint.clone();
        let remember = auth.remember;
        let user = auth
            .user
            .as_ref()
            .map(|s| s.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let password = auth
            .pass
            .as_ref()
            .map(|s| s.read(cx).value().to_string())
            .unwrap_or_default();

        if user.is_empty() {
            if let Some(auth) = self.connect_auth.as_mut() {
                auth.error = Some("用户名不能为空".to_string());
            }
            cx.notify();
            return;
        }
        if let Some(auth) = self.connect_auth.as_mut() {
            auth.error = None;
        }
        cx.notify();

        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let outcome = app
                .connect_remote_with_credentials(&endpoint, &user, &password)
                .await;
            this.update(cx, |v, cx| {
                let connected = outcome.is_ok();
                v.on_connect_result(outcome, cx);
                if connected {
                    // 勾了「记住密码」才把密码交给钥匙串；没勾也照样把这台记进
                    // 列表——下次打开对话框只需补个密码。
                    v.remember_server_async(
                        endpoint.clone(),
                        user.clone(),
                        remember.then(|| password.clone()),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// 认证弹窗的「取消 / Esc」：退回地址对话框，而不是整个关掉。
    ///
    /// 用户按 Esc 多半是想改地址（端口敲错、主机名打错），把刚敲的地址一起
    /// 丢掉太粗暴。
    pub(crate) fn connect_auth_cancel(&mut self, cx: &mut Context<Self>) {
        self.connect_auth = None;
        self.connect_error = None;
        self.modal = Modal::ConnectServer;
        cx.notify();
    }

    /// 忘掉一台服务器：从列表里删掉，并清掉钥匙串里存下的密码。
    pub(crate) fn connect_forget_server(&mut self, endpoint: String, cx: &mut Context<Self>) {
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let app2 = app.clone();
            let result = app
                .spawn_blocking(move || app2.forget_server(&endpoint))
                .await;
            this.update(cx, |v, cx| {
                // 列表变了，重新取一份快照给下一帧渲染。
                v.connect_servers = v.app().saved_servers();
                if let Ok(Err(e)) = result {
                    v.connect_error = Some(e);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 连接结果的统一落点（地址回车 / 点列表 / 认证提交都会走到）。
    ///
    /// * `Ok` —— 关掉对话框（记住服务器由调用方另行处理）；
    /// * `NeedsCredentials` —— 换到认证弹窗，并带上这次用的用户名；
    /// * `Message` —— 就地显示错误，对话框留着让用户改地址重试。
    pub(crate) fn on_connect_result(
        &mut self,
        outcome: Result<(), mo_app::ConnectFailure>,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(()) => {
                self.modal = Modal::None;
                self.connect_error = None;
                self.connect_auth = None;
            }
            Err(mo_app::ConnectFailure::NeedsCredentials {
                endpoint,
                user,
                detail,
            }) => {
                self.connect_auth = Some(ConnectAuthState::new(
                    endpoint,
                    (!detail.is_empty()).then_some(detail),
                    &user,
                ));
                self.modal = Modal::ConnectAuth;
            }
            Err(mo_app::ConnectFailure::Message(msg)) => match self.connect_auth.as_mut() {
                // 认证弹窗里失败：错误留在弹窗内（用户正在改的那几个框旁边）。
                Some(auth) => auth.error = Some(msg),
                None => self.connect_error = Some(msg),
            },
        }
        cx.notify();
    }

    /// 连接成功后把这台服务器记进「记住的服务器」（不含密码）。
    fn remember_active_server(&self, cx: &mut Context<Self>) {
        // 刚连上，所以「正在浏览的那条」就是刚建的这条；这里要的是它的地址
        // （含用户名），用来填「记住的服务器」那一行。
        let Some(url) = self.app().remote_url() else {
            return;
        };
        let endpoint = url.endpoint();
        let user = url.user.clone().unwrap_or_default();
        self.remember_server_async(endpoint, user, None, cx);
    }

    /// 记下一台服务器（可选地把密码写进钥匙串），放在 blocking 池里做。
    ///
    /// 不能留在 UI 线程上的两个理由：macOS 写钥匙串可能弹系统授权框；Linux 上
    /// 没有 Secret Service 时这一步会走完整条失败路径。
    ///
    /// 失败只在**用户勾了记住密码**时才打扰他——顺手记进列表那一步失败了
    /// 就静默算了（浏览本身没受影响）；但用户主动勾的「记住密码」没记住，
    /// 必须给个说法。
    fn remember_server_async(
        &self,
        endpoint: String,
        user: String,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let wanted = password.is_some();
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            let app2 = app.clone();
            let result = app
                .spawn_blocking(move || app2.remember_server(&endpoint, &user, password.as_deref()))
                .await;
            if wanted {
                if let Ok(Err(e)) = result {
                    this.update(cx, |v, cx| v.notice(e, None, cx));
                }
            }
        })
        .detach();
    }

    /// 「连接到服务器」对话框：走 [`dialog_overlay`]（带遮罩的浮层，非替换中央区）。
    ///
    /// 它是个小对话框，不该把整个浏览区顶掉。地址框是框架的**真实输入框**
    /// （见 [`RootView::sync_connect_address`]）；回车连接、Esc 取消见 `handle_modal_key`。
    ///
    /// 记住过的服务器会列在地址框下面——点一行直接用存下的凭据连，不用再敲地址。
    /// 地址里**不需要也不建议**写用户名密码：服务器要凭据时会自己弹认证框
    /// （见 [`RootView::render_connect_auth`]）。
    fn render_connect(&self, entity: &Entity<RootView>) -> impl IntoElement {
        let mut body = div().flex().flex_col().gap(px(10.0));
        body = body.child(
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child(text!("输入服务器地址，回车连接：".to_string())),
        );
        // 外框与底色仍由这一层画（与地址栏编辑态同款）：`appearance(false)` +
        // `bordered(false)` 把框架自带的 shadcn 边框关掉，别叠一层进来。
        let mut field = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(28.0))
            .px(px(6.0))
            .rounded(px(6.0))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::accent())
            // 测试用（release no-op）：断言地址框确实渲染出来了。
            .debug_selector(|| "mo-connect-field".to_string());
        if let Some(state) = &self.connect_input {
            field = field.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .child(
                        Input::new(state)
                            .appearance(false)
                            .bordered(false)
                            .small()
                            .text_size(px(13.0))
                            .p(px(0.0)),
                    ),
            );
        }
        body = body.child(field);

        // 「使用说明」折叠区：默认收起，点一下展开各协议的地址写法。
        // 放在地址框正下方——用户敲地址卡住时最需要它，但平时不该占对话框高度。
        let ent_help = entity.clone();
        let mut help_toggle = div()
            .id("mo-connect-help-toggle")
            .flex()
            .items_center()
            .h(px(22.0))
            .px(px(6.0))
            .rounded(px(4.0))
            .text_size(px(12.0))
            .text_color(theme::muted())
            .hover(|s| s.bg(theme::hover_bg()))
            // 测试用（release no-op）：定位这个折叠开关。
            .debug_selector(|| "mo-connect-help-toggle".to_string())
            .child(text!(if self.connect_help_open {
                "使用说明 ▾".to_string()
            } else {
                "使用说明 ▸".to_string()
            }));
        help_toggle.interactivity().on_click(move |_, _window, cx| {
            ent_help.update(cx, |v, cx| {
                v.connect_help_open = !v.connect_help_open;
                cx.notify();
            });
        });
        body = body.child(help_toggle);

        if self.connect_help_open {
            // 每种已支持的协议一行：地址写法 + 说明。行各自带 .id()——同一 text!
            // 站点在循环里渲染多次时，祖先 ID 链不同才不会撞 a11y 节点（debug 会崩）。
            const LINES: [(&str, &str); 5] = [
                (
                    "ftp://主机[:端口]",
                    "FTP，默认端口 21；匿名连接可不写用户名",
                ),
                ("sftp://主机[:端口]", "SFTP（SSH），默认端口 22"),
                (
                    "webdav://主机[:端口]",
                    "WebDAV over HTTP，默认端口 80（dav:// 等价）",
                ),
                (
                    "davs://主机[:端口]",
                    "WebDAV over HTTPS，默认端口 443（坚果云 / Nextcloud 等）",
                ),
                (
                    "通用：协议://用户名:密码@主机[:端口]/路径",
                    "密码可不写，需要时会弹框询问；服务器子路径直接接在后面（如 /remote.php/dav）",
                ),
            ];
            let mut panel = div()
                .id("mo-connect-help-panel")
                .flex()
                .flex_col()
                .gap(px(4.0))
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(theme::container())
                .border_1()
                .border_color(theme::separator())
                // 测试用（release no-op）：断言展开面板确实渲染出来了。
                .debug_selector(|| "mo-connect-help-panel".to_string());
            for (i, (usage, note)) in LINES.iter().enumerate() {
                panel = panel.child(
                    div()
                        .id(("mo-connect-help-line", i))
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.0))
                                .child(text!(usage.to_string())),
                        )
                        .child(
                            div()
                                .min_w(px(0.0))
                                .text_size(px(11.0))
                                .text_color(theme::muted())
                                .child(text!(note.to_string())),
                        ),
                );
            }
            body = body.child(panel);
        }

        // 记住过的服务器：点一行直接用存下的凭据连。
        if !self.connect_servers.is_empty() {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::muted())
                    .child(text!("已记住的服务器（点击直接连接）：".to_string())),
            );
            // 列表外框 + 斑马纹：与文件列表同款（奇数行 `theme::zebra()`，偶数行
            // `theme::surface()`），外面套一圈 `separator()` 描边把整块收进对话框里。
            // 圆角靠首/末行自己 `rounded_t` / `rounded_b` 收——这个 fork 不把子元素
            // 裁进父级圆角，行底色会把框的两个角切方（见 `dialog_header` 的说明）。
            let count = self.connect_servers.len();
            let list = div()
                .flex()
                .flex_col()
                .rounded(px(6.0))
                .border_1()
                .border_color(theme::separator())
                .bg(theme::surface())
                // 测试用（release no-op）：断言列表确实渲染出来了。
                .debug_selector(|| "mo-connect-servers".to_string())
                .child({
                    let mut rows = div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .max_h(px(120.0))
                        .overflow_y_scrollbar();
                    for (i, server) in self.connect_servers.iter().enumerate() {
                        rows = rows.child(self.saved_server_row(i, count, server, entity));
                    }
                    rows
                });
            body = body.child(list);
        }

        if let Some(err) = &self.connect_error {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(diff_del_fg())
                    .child(text!(err.clone())),
            );
        }

        let mut actions = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap(px(8.0))
            .pt(px(4.0));
        let ent_go = entity.clone();
        let mut connect_btn = Self::sync_button("connect-go", "连接", true);
        connect_btn.interactivity().on_click(move |_, _window, cx| {
            ent_go.update(cx, |v, cx| v.connect_submit(cx));
        });
        let ent_cancel = entity.clone();
        let mut cancel_btn = Self::sync_button("connect-cancel", "取消", false);
        cancel_btn.interactivity().on_click(move |_, _window, cx| {
            ent_cancel.update(cx, |v, cx| {
                v.modal = Modal::None;
                v.connect_error = None;
                v.connect_auth = None;
                cx.notify();
            });
        });
        actions = actions.child(cancel_btn).child(connect_btn);
        body = body.child(actions);

        dialog_overlay(
            entity,
            "连接到服务器",
            "",
            body,
            // 底部提示已去：协议写法在 placeholder 与「使用说明」折叠区里都有。
            "",
        )
    }

    /// 「已记住的服务器」里的一行：行首一枚协议图标，点整行直连，右侧 ✕ 忘掉它
    /// （含钥匙串里的密码）。
    ///
    /// 行底色沿用文件列表的斑马纹（奇数行 [`theme::zebra`]）。整块列表外圈还有边框
    /// 与圆角，而这个 fork 不会把子元素裁进父级圆角，所以贴边的首/末行要自己收上/下
    /// 两个角，否则矩形底色会把框角切方（同 `context_menu::item_hover_bg`）。
    fn saved_server_row(
        &self,
        index: usize,
        count: usize,
        server: &mo_app::SavedServer,
        entity: &Entity<RootView>,
    ) -> impl IntoElement {
        let mut row = div()
            .id(("mo-connect-server", index))
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(px(8.0))
            .h(px(26.0))
            .px(px(8.0))
            .bg(if index % 2 == 1 {
                theme::zebra()
            } else {
                theme::surface()
            })
            .hover(|s| s.bg(theme::hover_bg()))
            // 测试用（release no-op）：按序号定位某一行。
            .debug_selector(move || format!("mo-connect-server-{index}"))
            // 协议图标：一眼分清这行是共享文件夹、FTP 还是网盘。颜色必须显式给——
            // gpui 的 svg 不继承父级文字色，不给就不画。
            .child(
                div()
                    .flex_shrink_0()
                    // 测试用（release no-op）：断言行首图标位渲染出来了。
                    .debug_selector(move || format!("mo-connect-server-icon-{index}"))
                    .child(crate::icons::icon(
                        crate::icons::protocol_icon(&server.endpoint),
                        14.0,
                        theme::muted(),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(12.0))
                    .child(text!(server.endpoint.clone())),
            );
        // 框是 `rounded(6)` + 1px 描边，行收 5px 才贴得住内侧弧线。
        let radius = px(5.0);
        row = match (index == 0, index + 1 == count) {
            (true, true) => row.rounded(radius),
            (true, false) => row.rounded_t(radius),
            (false, true) => row.rounded_b(radius),
            (false, false) => row,
        };
        if !server.user.is_empty() {
            row = row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .child(text!(server.user.clone())),
            );
        }

        // ✕：忘掉这台——列表与钥匙串里的密码一起清掉。
        let ent_forget = entity.clone();
        let endpoint = server.endpoint.clone();
        let mut forget = div()
            .id(("mo-connect-forget", index))
            .px(px(6.0))
            .rounded(px(3.0))
            .text_size(px(11.0))
            .text_color(theme::muted())
            .hover(|s| s.bg(theme::hover_bg()))
            .child(text!("✕".to_string()));
        forget.interactivity().on_click(move |_, _window, cx| {
            // 别让「忘掉」顺带触发整行的「连接」。
            cx.stop_propagation();
            let endpoint = endpoint.clone();
            ent_forget.update(cx, |v, cx| v.connect_forget_server(endpoint, cx));
        });
        row = row.child(forget);

        let ent_go = entity.clone();
        let endpoint = server.endpoint.clone();
        row.interactivity().on_click(move |_, _window, cx| {
            let endpoint = endpoint.clone();
            ent_go.update(cx, |v, cx| v.connect_to(endpoint, cx));
        });
        row
    }

    /// 认证弹窗：服务器要求登录时出现（用户名 + 密码 + 记住密码）。
    ///
    /// 只在服务器**真的拒绝**了匿名登录后才出现——「地址里没写凭据」不等于会弹它。
    /// 密码框是 `InputState::masked(true)`，**绘制层显示成星号**；勾了「记住密码」
    /// 就把凭据交给系统钥匙串（`mo_app` 的 `credentials`），而不是写进明文配置。
    fn render_connect_auth(&self, entity: &Entity<RootView>) -> AnyElement {
        let Some(auth) = self.connect_auth.as_ref() else {
            // 状态还没建（`sync_connect_auth` 在 render 之前就跑过了，正常到不了
            // 这里）：给个空壳，别 panic。
            return div().into_any_element();
        };
        let mut body = div().flex().flex_col().gap(px(10.0));
        body = body.child(
            div()
                .text_size(px(12.0))
                .text_color(theme::text())
                .child(text!(format!("{} 需要登录：", auth.endpoint))),
        );
        if let Some(hint) = &auth.hint {
            body = body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::muted())
                    .child(text!(hint.clone())),
            );
        }

        for (idx, (label, id, state)) in [
            ("用户名", "mo-connect-user", auth.user.as_ref()),
            ("密码", "mo-connect-pass", auth.pass.as_ref()),
        ]
        .into_iter()
        .enumerate()
        {
            body = body.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        // `text!` 没有定宽方法，要定宽就包一层。
                        // 显式 id：两个标签共享宏调用点哈希 → a11y NodeId 会撞
                        // （见 list_view::header_row 注释）。
                        div()
                            .w(px(48.0))
                            .text_size(px(12.0))
                            .text_color(theme::muted())
                            .child(text!(id = format!("auth-label-{idx}"), label.to_string())),
                    )
                    .child(self.auth_field(id, state)),
            );
        }

        body = body.child(self.remember_checkbox(auth.remember, entity));

        if let Some(err) = &auth.error {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(diff_del_fg())
                    .child(text!(err.clone())),
            );
        }

        let mut actions = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap(px(8.0))
            .pt(px(4.0));
        let ent_go = entity.clone();
        let mut go = Self::sync_button("connect-auth-go", "连接", true);
        go.interactivity().on_click(move |_, _window, cx| {
            ent_go.update(cx, |v, cx| v.connect_auth_submit(cx));
        });
        let ent_back = entity.clone();
        let mut back = Self::sync_button("connect-auth-cancel", "返回", false);
        back.interactivity().on_click(move |_, _window, cx| {
            ent_back.update(cx, |v, cx| v.connect_auth_cancel(cx));
        });
        actions = actions.child(back).child(go);
        body = body.child(actions);

        dialog_overlay(
            entity,
            "需要登录",
            "",
            body,
            "密码显示为星号 · 勾选「记住密码」则存入系统钥匙串 · 回车连接 · Esc 返回",
        )
        .into_any_element()
    }

    /// 认证弹窗里的一行输入框（与地址框同款外观，标识不同便于测试定位）。
    fn auth_field(&self, id: &'static str, state: Option<&Entity<InputState>>) -> Div {
        let mut field = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_1()
            .min_w(px(0.0))
            .h(px(28.0))
            .px(px(6.0))
            .rounded(px(6.0))
            .bg(theme::surface())
            .border_1()
            .border_color(theme::separator())
            // 测试用（release no-op）：定位用户名 / 密码框。
            .debug_selector(move || id.to_string());
        if let Some(state) = state {
            field = field.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .child(
                        Input::new(state)
                            .appearance(false)
                            .bordered(false)
                            .small()
                            .text_size(px(13.0))
                            .p(px(0.0)),
                    ),
            );
        }
        field
    }

    /// 认证弹窗里的「记住密码」勾选框（自绘：点击切换 + ✓）。
    ///
    /// 没有现成的 checkbox 组件，而这里只需要一个布尔——画个方框加个对勾比引
    /// 一整套控件轻。
    fn remember_checkbox(&self, on: bool, entity: &Entity<RootView>) -> impl IntoElement {
        let mark = if on { "✓" } else { "" }.to_string();
        let mut row = div()
            .id("mo-connect-remember")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .w_full()
            .child(
                div()
                    .w(px(14.0))
                    .h(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(if on {
                        theme::accent()
                    } else {
                        theme::separator()
                    })
                    .bg(if on {
                        theme::selected_bg()
                    } else {
                        theme::surface()
                    })
                    .text_size(px(10.0))
                    .text_color(theme::selected_text())
                    .child(text!(mark)),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text())
                    .child(text!("记住密码（存入系统钥匙串）".to_string())),
            )
            // 测试用（release no-op）：断言勾选框在。
            .debug_selector(|| "mo-connect-remember".to_string());
        let ent = entity.clone();
        row.interactivity().on_click(move |_, _window, cx| {
            ent.update(cx, |v, cx| {
                if let Some(auth) = v.connect_auth.as_mut() {
                    auth.remember = !auth.remember;
                }
                cx.notify();
            });
        });
        row
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

        // 地址栏导航同样要离开次级视图（回收站 / 全局搜索）：填了新地址 = 要去那边。
        self.leave_secondary_view(cx);

        // 形如 `scheme://...` 的地址 → 当成远程连接（协议是否支持由连接逻辑判定）。
        if text.contains("://") {
            cx.notify();
            cx.spawn(async move |weak, cx| {
                let outcome = app.connect_remote(&text).await;
                let _ = weak.update(cx, |v, cx| match outcome {
                    Ok(()) => v.remember_active_server(cx),
                    // 地址栏这条路没有对话框，但「需要登录」的处置一样——直接弹
                    // 认证框（`on_connect_result` 负责把用户名带进去）。
                    Err(failure @ mo_app::ConnectFailure::NeedsCredentials { .. }) => {
                        v.on_connect_result(Err(failure), cx);
                    }
                    // 其余失败走信息提示：地址栏那边没有可以留在原地显示错误的
                    // 对话框。
                    Err(mo_app::ConnectFailure::Message(msg)) => {
                        v.notice(format!("连接失败：{msg}"), None, cx);
                    }
                });
            })
            .detach();
            return;
        }

        // 正在看远程时，地址栏里填的是远程绝对路径（如 /pub/incoming）：
        // 跳过本地 is_dir 判定，直接交给当前后端导航。
        if app.browsing_remote() {
            let target = PathBuf::from(&text);
            cx.notify();
            cx.spawn(async move |weak, cx| {
                if let Err(e) = app.open_directory(&target).await {
                    let _ = weak.update(cx, |v, cx| {
                        v.notice(format!("打开「{}」失败：{e}", target.display()), None, cx);
                    });
                }
            })
            .detach();
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

    /// 命令面板当前列表的长度——借给「选择其他应用…」时是应用列表。
    fn palette_len(&self) -> usize {
        match &self.app_picker {
            Some(p) => filtered_apps(&self.cmd_query, &p.apps).len(),
            None => {
                filtered_commands_in(&self.cmd_query, &self.user_commands, &self.workflows).len()
            }
        }
    }

    /// 打开「选择其他应用…」的选择器（macOS）。
    ///
    /// 复用命令面板那层壳：输入即过滤、↑↓ 移动、Enter 用选中的应用打开。
    /// 列表是异步加载的（LaunchServices + 扫 `/Applications`，都是 IO），
    /// 所以先把空壳摆上、加载完再填——与「打开方式」二级菜单同一个套路。
    fn open_app_picker(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.modal = Modal::CommandPalette;
        self.cmd_query.clear();
        self.palette_index = 0;
        self.app_picker = Some(AppPicker {
            path: path.clone(),
            apps: Vec::new(),
        });
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            // 系统推荐的排前面，其余已装应用补在后面（同一个 App 不列两遍）。
            let mut apps = app.open_with_candidates(&path).await;
            let mut seen: std::collections::HashSet<String> =
                apps.iter().map(|a| a.progid.clone()).collect();
            for a in app.installed_apps().await {
                if seen.insert(a.progid.clone()) {
                    apps.push(a);
                }
            }
            this.update(cx, |v, cx| {
                // 一个都列不出来时别留个「无匹配」的空壳骗人——如实说。
                if apps.is_empty() {
                    v.app_picker = None;
                    v.modal = Modal::Info("没有找到可以打开它的应用".to_string());
                } else if let Some(p) = v.app_picker.as_mut() {
                    p.apps = apps;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 打开一个条目（双击 / 回车同语义）：目录进入，文件用系统默认应用打开。
    ///
    /// ⚠️ `is_dir` 必须由调用方从**列表模型**（`entry.kind`）传来，**不要**在这里查
    /// `path.is_dir()`（也不要退回那个写法）：那是本机磁盘判断，远程条目的路径
    /// （`/1`）在本机不存在，远程目录会被当成文件交给系统 `open`——日志里只有
    /// `The file /1 does not exist.`，界面一动不动（用户报的 bug，双击和回车都中）。
    /// 判据的守卫在 `mo-app`：`AppState::entry_is_dir` +
    /// `crates/mo-app/tests/remote_local.rs::whether_an_entry_is_a_directory_comes_from_the_listing`。
    pub(crate) fn open_entry(&mut self, path: PathBuf, is_dir: bool, cx: &mut Context<Self>) {
        if is_dir {
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

    /// 打开回收站条目的**实际落点**（双击语义）。
    ///
    /// 与 [`RootView::open_entry`] 的差别：落点必然是**本地**路径
    /// （`~/.Trash` / 卷上 `.Trashes`），所以目录走 `open_local`（本地入口
    /// 纪律），文件交给系统默认应用。目录还会先退出回收站面板——浏览替代
    /// 管理，面板任务已结束。
    pub(crate) fn open_trash_entry(
        &mut self,
        trashed: PathBuf,
        is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        if is_dir {
            self.modal = Modal::None;
            cx.notify();
            let app = self.app();
            cx.spawn(async move |_weak, _cx| {
                let _ = app.open_local(&trashed).await;
            })
            .detach();
        } else {
            let app = self.app();
            cx.spawn(async move |_weak, _cx| {
                if let Err(e) = app.open_with_system(&trashed).await {
                    tracing::warn!("打开回收站条目 {trashed:?} 失败：{e}");
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
        // 新的一代：后台还在为上一张准备的降采样副本据此作废（见 `preview_seq`）。
        self.preview_seq += 1;

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
        // ⚠️ **同步**开窗——原来包在 `cx.spawn` 里，要下一拍才真的建。两个理由：
        // ① 按空格到窗口出现之间不再白等一拍（`preview_step` 走的是复用分支，无影响）；
        // ② 建完就能拿到 `preview_window`：`open_quick_look` 的降采样任务可能比开窗
        //    还快（小图 0.3ms 就返回），异步开窗时那种情况会「贴图时窗口还不存在」，
        //    结果图永远贴不上、窗口停在占位。
        // 建窗闭包要把主视图交给 PreviewWindow（方向键回调）。
        let root_for_window = cx.entity();
        match cx.open_window(options, move |_, cx| {
            cx.new(|cx| crate::preview::PreviewWindow::new(pv, root_for_window, cx))
        }) {
            Ok(handle) => self.preview_window = Some(handle),
            Err(e) => {
                self.modal = Modal::Info(format!("打开预览窗口失败：{e}"));
                cx.notify();
            }
        }
    }

    /// 把已开着的预览窗里的图片换成刚准备好的降采样副本。
    ///
    /// `seq` 是发起准备时的预览代际：这期间用户可能已经翻页或关掉了窗口，
    /// 代际对不上就直接丢弃（否则会把上一张的图贴到当前预览上）。
    /// 窗口关闭走的是 `update` 报错——顺手把句柄清掉。
    pub(crate) fn set_preview_image(&mut self, seq: u64, image: PathBuf, cx: &mut Context<Self>) {
        if seq != self.preview_seq {
            return;
        }
        let Some(handle) = self.preview_window else {
            return;
        };
        if handle
            .update(cx, |v, _window, cx| v.set_image(image, cx))
            .is_err()
        {
            self.preview_window = None;
        }
    }

    /// 预览窗口里的文本替换（PDF 首页渲染不出来时给一句解释）。
    pub(crate) fn set_preview_text(&mut self, seq: u64, text: String, cx: &mut Context<Self>) {
        if seq != self.preview_seq {
            return;
        }
        let Some(handle) = self.preview_window else {
            return;
        };
        if handle
            .update(cx, |v, _window, cx| v.set_text(text, cx))
            .is_err()
        {
            self.preview_window = None;
        }
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
            // 翻页与首次打开共用一条**两拍**路径：先把窗口切到新文件（副本还没好时
            // 显示「载入预览…」），降采样在后台跑。以前是「先 await 降采样、再换内容」，
            // 于是按方向键后窗口里还挂着**上一张**的图，直到新图就绪才跳变。
            if let Ok(pv) = app.preview(&p) {
                show_preview_twopass(&app, &this, pv, cx);
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

    /// 打开主题选择器（光标停在当前主题）——统一设置窗口的「外观」页。
    pub(crate) fn open_theme_picker(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsTab::Theme, cx);
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

    /// 主题选择器：Enter 确认当前项并持久化（设置窗口保持打开）。
    fn theme_commit(&mut self, cx: &mut Context<Self>) {
        let app = self.app();
        let names = crate::theme::choices(&app.custom_themes());
        if let Some(name) = names.get(self.theme_index).cloned() {
            app.set_theme(&name);
            self.theme_name = name;
        }
        self.repaint_theme(cx);
    }

    /// 还原主题预览（不关窗）：外观页翻页 / 切走时用——预览只是全局调色板
    /// 换了个样子，没写配置，离开这一页就该回到配置里的主题。
    fn theme_revert_preview(&mut self, cx: &mut Context<Self>) {
        self.theme_name = self.app().theme_setting();
        self.repaint_theme(cx);
    }

    /// 主题选择器：Esc 放弃预览，还原到进入前配置里的主题并关窗。
    fn theme_cancel(&mut self, cx: &mut Context<Self>) {
        self.theme_revert_preview(cx);
        self.modal = Modal::None;
    }

    /// 外观页正文：主题列表（↑↓ 预览）+ 当前调色板色卡。外壳由 [`Self::render_settings`] 提供。
    fn theme_body(&self, entity: &Entity<RootView>) -> Div {
        let app = self.app();
        let names = crate::theme::choices(&app.custom_themes());
        let mut body = div().flex().flex_col().gap(px(2.0));
        for (i, name) in names.iter().enumerate() {
            let selected = i == self.theme_index;
            let on_click_theme = name.clone();
            let title = if crate::theme::label(name) != *name {
                // 自定义主题额外带上配置里的 key，方便对着 config.json 改。
                format!("{}（{}）", crate::theme::label(name), name)
            } else {
                crate::theme::label(name)
            };
            let mut row = picker_row(&format!("theme-row-{i}"), selected, &title);
            if !selected {
                row = row.hover(|s| s.bg(theme::hover_bg()));
            }
            if *name == self.theme_name {
                row = row.child(tag_pill("当前", selected));
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
        body
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
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(2.0))
            .p(px(8.0));
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
        central_view(
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
                .window_entries()
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
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(4.0))
            .p(px(8.0));
        if self.wf_running {
            body = body.child(
                div()
                    .text_color(theme::muted())
                    .child(text!("正在执行…（Esc：不再继续下一步）".to_string())),
            );
            return central_view("自动化工作流", "", body, "执行在后台进行，可以继续浏览");
        }
        let Some(r) = self.wf_report.clone() else {
            body = body.child(
                div()
                    .text_color(theme::muted())
                    .child(text!("（还没有执行记录）".to_string())),
            );
            return central_view("自动化工作流", "", body, "Esc 关闭");
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
        central_view(
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
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(4.0))
            .p(px(8.0));

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
        central_view(
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
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(4.0))
            .p(px(8.0));
        if self.dedup_running {
            body = body.child(
                div()
                    .text_color(theme::muted())
                    .child(text!("正在扫描当前目录…（Esc 取消）".to_string())),
            );
            return central_view("重复文件", "", body, "扫描在后台进行，可以继续浏览");
        }
        let Some(report) = self.dedup.clone() else {
            return central_view("重复文件", "", body, "还没有结果");
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
                        // 显式 id：同组多条路径共享 `text!` 调用点哈希（a11y NodeId 会撞）。
                        .child(text!(
                            id = format!("dedup-path-{i}-{j}"),
                            format!("{} {}", if j == 0 { "◆" } else { "  " }, p.display())
                        )),
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
        central_view(
            "重复文件",
            "",
            body,
            "删副本＝移入回收站（⌘Z 可撤销）· ◆ 为建议保留 · Esc 关闭",
        )
    }

    /// 切换「显示隐藏文件」（⌘⇧.，访达同款键位）。
    ///
    /// 两件事必须一起做，**顺序也不能反**：
    ///
    /// * **所有标签页都要切**：开关在 `AppState` 里是进程内镜像（列目录是热路径，
    ///   不能每次读盘），而每个标签页各持一份 `AppState`——只改当前那份会出现
    ///   「切到另一个标签页又变回去」；
    /// * **切完立刻重读**：过滤发生在建视图**之前**（`load_path`），不重读的话界面
    ///   上什么都没变，用户会以为按键没生效。
    fn toggle_hidden(&mut self, _entity: &Entity<RootView>, cx: &mut Context<Self>) {
        let apps = self.all_apps();
        // 判据取「当前值取反」：所有标签页同值，用谁读都一样；`apps` 为空时是关。
        let next = !apps.first().is_some_and(|a| a.show_hidden());
        for a in &apps {
            a.set_show_hidden(next);
        }
        cx.spawn(async move |_weak, _cx| {
            for a in apps {
                let _ = a.refresh().await;
            }
        })
        .detach();
        cx.notify();
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
            // macOS 惯例的 ⌘, ——直开设置窗口，省去「命令面板 → 翻到主题/布局」两步。
            "settings.open" => self.open_settings(SettingsTab::Layout, cx),
            "staging.collect" => self.stage_selection(cx),
            "staging.toggle" => self.toggle_staging(cx),
            "compare.toggle" => self.toggle_compare(cx),
            "compare.jump_next" => self.jump_diff(cx, 1),
            "compare.jump_prev" => self.jump_diff(cx, -1),
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
            "search.content" => {
                self.open_content_search(cx);
            }
            "server.connect" => {
                self.open_connect_dialog(cx);
                cx.notify();
            }
            "server.disconnect" => {
                let app = self.app();
                let entity = cx.entity().clone();
                self.connect_error = None;
                cx.spawn(async move |_weak, cx| {
                    let _ = app.disconnect_remote().await;
                    // 断开后回写 UI：侧边栏入口切回「连接到服务器…」、地址栏回落本地。
                    entity.update(cx, |_, cx| cx.notify());
                })
                .detach();
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
            "select.invert" => {
                let app = self.app();
                cx.spawn(async move |_weak, cx| {
                    app.select_invert_visible().await;
                    // app 侧选择是唯一事实来源：反选后回灌 UI 高亮。
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
            "view.hidden" => {
                self.toggle_hidden(&entity, cx);
            }
            "view.zoom_in" => self.zoom_by(1, cx),
            "view.zoom_out" => self.zoom_by(-1, cx),
            "view.zoom_reset" => self.reset_zoom(cx),
            key @ ("view.list" | "view.grid" | "view.gallery" | "view.columns") => {
                let mode = match key {
                    "view.grid" => ViewMode::Grid,
                    "view.gallery" => ViewMode::Gallery,
                    "view.columns" => ViewMode::Columns,
                    _ => ViewMode::List,
                };
                // 收口在 [`RootView::set_view_mode_for_active_surface`]：回收站面板
                // 开着切**回收站**的视图（用户报：工具栏切了没反应——按钮写的是
                // 浏览面板的状态，回收站渲染不读），否则切浏览面板。
                self.set_view_mode_for_active_surface(mode);
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
                        .window_entries()
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
                        .window_entries()
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
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    // 远程删除可能真失败（连接断了 / 没权限）：必须回显，
                    // 否则用户只看到「条目还在」，以为没删掉是因为没点上。
                    if let Err(e) = app.delete_selection().await {
                        this.update(cx, |v, cx| {
                            v.notice(format!("删除失败：{e}"), None, cx);
                        });
                    }
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

    /// 打开快捷键设置器——统一设置窗口的「快捷键」页。
    pub(crate) fn open_keys_picker(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsTab::Keys, cx);
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

    /// 快捷键页正文：绑定列表 + 恢复默认。外壳由 [`Self::render_settings`] 提供。
    fn keys_body(&self, entity: &Entity<RootView>) -> Div {
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(1.0))
            .p(px(6.0));
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
                .gap(px(10.0))
                .px(px(10.0))
                .h(px(30.0))
                .rounded(px(8.0))
                .text_size(px(13.0))
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
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(text!(b.label.to_string())),
                )
                .child(if capturing {
                    tag_pill("按下新键位…", true)
                } else {
                    tag_pill(&combo, false)
                });
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
        body
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
            ("新标签页默认视图".to_string(), mode.to_string()),
            // 图标缩放的三个入口（网格 / 画廊生效）。值那一列直接显示当前倍率，
            // 这样「按了没反应」一眼能看出是到了档位边界还是根本没生效。
            // f32 的 Display 会砍掉多余的 0（1.0 → "1"、1.25 → "1.25"）。
            (
                "放大图标（网格 / 画廊）".to_string(),
                format!("{}×", self.ui.icon_scale),
            ),
            ("缩小图标（网格 / 画廊）".to_string(), String::new()),
            ("图标大小还原 1×".to_string(), String::new()),
            // 列表分组：值那一列显示当前档位（不分组 / 按类型 / 按日期）。
            (
                "列表分组".to_string(),
                grouping_label(self.panel().grouping).to_string(),
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
                .window_entries()
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

    /// 打开统一设置窗口并停在某个标签页（各标签页光标复位语义与原选择器一致）。
    pub(crate) fn open_settings(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        match tab {
            SettingsTab::Layout => self.layout_index = 0,
            SettingsTab::Theme => {
                // 光标停在当前主题上。
                let app = self.app();
                let names = crate::theme::choices(&app.custom_themes());
                self.theme_index = names
                    .iter()
                    .position(|n| *n == self.theme_name)
                    .unwrap_or(0);
            }
            SettingsTab::Keys => {
                self.keys_index = 0;
                self.keys_capturing = None;
            }
        }
        self.settings_tab = tab;
        self.modal = Modal::Settings;
        cx.notify();
    }

    /// ← / → 循环切换设置标签页；离开外观页时还原未提交的预览。
    fn settings_step(&mut self, dir: isize, cx: &mut Context<Self>) {
        let all = SettingsTab::ALL;
        let n = all.len();
        let i = all
            .iter()
            .position(|t| *t == self.settings_tab)
            .unwrap_or(0) as isize;
        let next = all[(i + dir).rem_euclid(n as isize) as usize];
        if self.settings_tab == SettingsTab::Theme && next != SettingsTab::Theme {
            // 预览没提交（Enter）就翻页：还原到配置里的主题，别把预览色留在界面上。
            self.theme_revert_preview(cx);
        }
        self.open_settings(next, cx);
    }

    /// 打开布局设置器——统一设置窗口的「界面」页。
    pub(crate) fn open_layout_picker(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsTab::Layout, cx);
    }

    /// 立刻把当前 `ui` 写回配置。
    fn persist_ui(&self) {
        let ui = self.ui.clone();
        self.app().set_ui_prefs(ui);
    }

    /// 按一档调整图标缩放（`dir` = +1 放大 / -1 缩小），只作用于网格 / 画廊。
    pub(crate) fn zoom_by(&mut self, dir: i32, cx: &mut Context<Self>) {
        let step = mo_app::ICON_SCALE_STEP * if dir < 0 { -1.0 } else { 1.0 };
        self.set_icon_scale(self.ui.icon_scale + step, cx);
    }

    /// 图标大小还原 1×。
    pub(crate) fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.set_icon_scale(1.0, cx);
    }

    /// 列表分组循环切换（无 → 按类型 → 按日期 → 无）。
    ///
    /// 改的是当前面板 app 的视图分组（异步），同时把它落成**新标签页的默认值**
    /// （`ui.group`）——与「新标签页默认视图」同一条约定：设置器里改的是默认，
    /// 不是只影响当前标签页的一次性开关。
    pub(crate) fn cycle_grouping(&mut self, cx: &mut Context<Self>) {
        let next = self.panel().grouping.next();
        self.ui.group = next.key().to_string();
        self.persist_ui();
        let app = self.panel().app.clone();
        cx.spawn(async move |_weak, _cx| {
            app.set_grouping(next).await;
        })
        .detach();
        cx.notify();
    }

    /// 收口一次图标缩放：夹档位 → 落盘 → 重绘。
    ///
    /// 两件事值得在这一个入口里做掉，而不是让调用方各写一遍：
    ///
    /// * **夹档位只在这里**：边界与量化归 `mo_app::clamp_icon_scale`（唯一定义处），
    ///   界面上再夹一次就等于多一份会漂的规则；
    /// * **值没变就不落盘**：档位到头后继续按 ⌘+，不该每次重写一遍 config.json
    ///   （用户按着不放会连着写）。没变也就不 `notify`——那一帧本来就没东西变。
    fn set_icon_scale(&mut self, v: f32, cx: &mut Context<Self>) {
        let v = mo_app::clamp_icon_scale(v);
        if self.ui.icon_scale == v {
            return;
        }
        self.ui.icon_scale = v;
        self.persist_ui();
        cx.notify();
    }

    /// 触发布局设置器里的一行（开关取反 / 视图循环 / 图标缩放 / 恢复默认）。
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
            // 图标缩放三档：`zoom_*` 自己落盘 + 重绘，所以这里直接 return，
            // 免得再走一遍下面的 `persist_ui` 写第二次配置。
            4 => {
                self.zoom_by(1, cx);
                return;
            }
            5 => {
                self.zoom_by(-1, cx);
                return;
            }
            6 => {
                self.reset_zoom(cx);
                return;
            }
            // 列表分组：循环到下一档（cycle_grouping 自己落盘 + 重绘）。
            7 => {
                self.cycle_grouping(cx);
                return;
            }
            8 => {
                // 恢复默认：配置落盘 + 内存里的列布局也一起复位；
                // 当前面板的分组也回到「不分组」（默认值的一部分）。
                self.app().reset_layout();
                self.ui = mo_app::UiPrefs::default();
                self.cols = crate::list_columns::ColumnLayout::new();
                self.persist_ui();
                let app = self.panel().app.clone();
                cx.spawn(async move |_weak, _cx| {
                    app.set_grouping(mo_app::Grouping::None).await;
                })
                .detach();
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

    /// 界面页正文：布局开关与默认值行。外壳由 [`Self::render_settings`] 提供。
    fn layout_body(&self, entity: &Entity<RootView>) -> Div {
        let mut body = div().flex().flex_col().gap(px(2.0));
        for (i, (label, value)) in self.layout_rows().into_iter().enumerate() {
            let selected = i == self.layout_index;
            let mut row = picker_row(&format!("layout-row-{i}"), selected, &label);
            if !selected {
                row = row.hover(|s| s.bg(theme::hover_bg()));
            }
            if !value.is_empty() {
                row = row.child(tag_pill(&value, selected));
            }
            let ent = entity.clone();
            row.interactivity().on_click(move |_ev, _window, cx| {
                ent.update(cx, |v, cx| v.layout_activate(i, cx));
            });
            body = body.child(row);
        }
        body
    }

    /// 统一设置窗口：左侧标签栏（界面 / 外观 / 快捷键）+ 右侧当前页正文。
    ///
    /// 三个标签页的正文（[`Self::layout_body`] / [`Self::theme_body`] /
    /// [`Self::keys_body`]）就是原布局 / 主题 / 快捷键三个独立选择器的列表本体，
    /// 外壳（标题栏 / 提示行 / Esc 与点遮罩语义）由 [`dialog_overlay`] 统一提供。
    /// 正文统一限高滚动：卡片不再随标签页内容多少改变高度，翻页不跳。
    fn render_settings(&self, entity: &Entity<RootView>) -> impl IntoElement {
        let mut rail = div().flex().flex_col().gap(px(4.0)).w(px(92.0)).pt(px(2.0));
        for (i, tab) in SettingsTab::ALL.iter().enumerate() {
            let active = *tab == self.settings_tab;
            let ent = entity.clone();
            let mut row = div()
                .id(format!("settings-tab-{i}"))
                .flex()
                .flex_row()
                .items_center()
                .px(px(10.0))
                .h(px(30.0))
                .rounded(px(8.0))
                .text_size(px(13.0))
                .bg(if active {
                    theme::container()
                } else {
                    theme::surface()
                })
                .text_color(if active {
                    theme::text()
                } else {
                    theme::muted()
                })
                .hover(|s| s.bg(theme::hover_bg()))
                .child(text!(tab.label().to_string()));
            row.interactivity().on_click(move |_ev, _window, cx| {
                ent.update(cx, |v, cx| {
                    if v.settings_tab != *tab {
                        v.settings_step(
                            SettingsTab::ALL.iter().position(|t| t == tab).unwrap_or(0) as isize
                                - SettingsTab::ALL
                                    .iter()
                                    .position(|t| *t == v.settings_tab)
                                    .unwrap_or(0) as isize,
                            cx,
                        );
                    }
                });
            });
            rail = rail.child(row);
        }
        let inner: Div = match self.settings_tab {
            SettingsTab::Layout => self.layout_body(entity),
            SettingsTab::Theme => self.theme_body(entity),
            SettingsTab::Keys => self.keys_body(entity),
        };
        let body = div()
            .flex()
            .flex_row()
            .items_stretch()
            .gap(px(12.0))
            .min_h_0()
            .child(rail)
            .child(div().flex_none().w(px(1.0)).bg(theme::separator()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h(px(340.0))
                    .overflow_y_scrollbar()
                    .child(inner),
            );
        let hint = match self.settings_tab {
            SettingsTab::Layout => {
                "↑↓ 选择 · Enter 切换 · 改动即时生效并写入 config.json · ←→ 换页 · Esc 关闭"
            }
            SettingsTab::Theme => {
                "↑↓ 预览 · Enter 应用 · Esc 还原并关闭 · 自定义主题写在 config.json 的 custom_themes"
            }
            SettingsTab::Keys => {
                "↑↓ 选择 · Enter 捕获 · Delete 解绑 · R 复位 · ←→ 换页 · Esc 关闭"
            }
        };
        dialog_overlay_sized(
            entity,
            "设置",
            self.settings_tab.label(),
            body,
            hint,
            px(620.0),
        )
    }

    // ------------------------------------------------------------ 列视图

    /// 所有处于列视图的标签页：根列缺失或目录已变化时触发加载。
    ///
    /// 每帧调用，但只在「当前目录 ≠ 根列」且没有在途任务时才真正读盘。
    /// 判据是**第 0 列**：列视图的第 0 列恒等于当前目录，下钻只往后追加列。
    /// 因此换目录时必须走「重建」（`load_column(.., None, ..)` 会清空整条列栈），
    /// 不能只往末尾追加——否则第 0 列永远对不上，每读完一列又判一次过期（见
    /// [`Self::load_column`] 的注释）。
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

    /// 加载一列并作为当前最深列。
    ///
    /// - `parent = Some(i)`：展开第 `i` 列里选中的子目录 —— 读完截掉 `i + 1`
    ///   之后的列再追加（在途期间保持旧列栈显示，避免闪一排空列）；
    /// - `parent = None`：**重建根列**（当前目录换了）—— 发起时就丢掉整条列栈。
    ///
    /// ⚠️ `None` 必须是「替换」而不是「追加」。`ensure_columns` 每帧按
    /// 「第 0 列 ≠ 当前目录」判断根列过期；若只往末尾追加，第 0 列永远等于旧
    /// 路径 → 每读完一列又判一次过期 → 无限加列。而列视图的列是**全量读盘**
    /// （不虚拟化，只 `take(MAX_PER_COLUMN)` 渲染），一个上万条的目录每列就是
    /// 一次全量读 + 排序，几秒内就把内存和主线程拖死——用户报的「一直无限加载
    /// 同一个目录导致应用卡死」正是这条路径（从 `47a5fec` 引入列视图起就在）。
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
            // 换根：旧列栈对新目录已经没有意义，先丢掉，别等到读完。
            if parent.is_none() {
                p.columns.clear();
            }
        }
        cx.spawn(async move |weak, cx| {
            let entries = app.list_dir(&path).await.unwrap_or_default();
            let _ = weak.update(cx, |v, cx| {
                let Some(p) = v.panel_at_mut(pane, tab) else {
                    return;
                };
                p.column_busy = false;
                match parent {
                    Some(i) => {
                        // 在途期间列栈被重建过（中途换了目录 / 切走又切回）：
                        // 这一列挂不上任何父列，丢掉不 push，让它自愈重来。
                        if i >= p.columns.len() {
                            return;
                        }
                        p.columns.truncate(i + 1);
                    }
                    // 重建根列：清空后只留这一列，`ensure_columns` 的判据才收敛。
                    None => p.columns.clear(),
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
                .and_then(|id| self.panel().window_entries().find(|e| e.id == *id))
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
        self.usage_tree = None;
        self.usage_root = Some(root.clone());
        self.form_index = 0;
        self.modal = Modal::DiskUsage;
        let this = cx.entity().clone();
        cx.spawn(async move |_, cx| {
            let usage = app.analyze_usage(root).await.unwrap_or_default();
            this.update(cx, |v, cx| {
                v.usage = usage;
                cx.notify();
                // 地图模式要层级，条形图不需要：懒加载一次，切过去才付这份代价。
                if v.usage_map {
                    v.refresh_usage_tree(cx);
                }
            });
        })
        .detach();
    }

    /// 重建磁盘地图的树（`usage_root` 为根）。
    fn refresh_usage_tree(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.usage_root.clone() else {
            return;
        };
        let app = self.app();
        self.usage_tree = None;
        let this = cx.entity().clone();
        cx.spawn(async move |_, cx| {
            let tree = app.usage_tree(root, USAGE_MAP_DEPTH).await.ok();
            this.update(cx, |v, cx| {
                v.usage_tree = tree;
                cx.notify();
            });
        })
        .detach();
    }

    /// 条形图 ↔ 磁盘地图：同一份统计的两种看法。
    pub(crate) fn toggle_usage_map(&mut self, cx: &mut Context<Self>) {
        self.usage_map = !self.usage_map;
        if self.usage_map && self.usage_tree.is_none() {
            self.refresh_usage_tree(cx);
        }
        cx.notify();
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
            .filter_map(|id| self.panel().window_entries().find(|e| e.id == *id))
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
            p.window_entries()
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

    /// prepaint 回写列表内容区 bounds（窗口坐标），供框选折算行下标与
    /// 「斑马纹铺满一屏」算行数。值变化时返回 `true`（调用方借此 notify，
    /// 让下一帧用上新高度；高度不变时通知会造成每帧重绘）。
    pub(crate) fn set_list_origin(
        &mut self,
        pane: usize,
        tab: usize,
        x: f32,
        y: f32,
        h: f32,
    ) -> bool {
        let changed = self.list_origin.get(&(pane, tab)) != Some(&(x, y, h));
        if changed {
            self.list_origin.insert((pane, tab), (x, y, h));
        }
        changed
    }

    /// 列表视图的斑马纹要**铺满一屏**：条目不足一屏时，把 `uniform_list` 的
    /// 行数补到「内容区装得下的行数」，多出来的行走占位斑马纹（file_list 渲染端
    /// 的 `mo-file-ph-*` 分支，数据行未就绪时画的同一种空行）。
    ///
    /// 行数取 `floor((h - 2*PAD) / ROW_H)` 保证内容高度不超出视口——超出会出现
    /// 一条能滚进空白区的假滚动量。高度来自上一帧 prepaint；首帧拿不到就先按
    /// `count` 渲染，下一帧自然补齐。
    pub(crate) fn list_fill_rows(&self, pane: usize, tab: usize, count: usize) -> usize {
        const ROW_H: f32 = 24.0;
        const PAD: f32 = 12.0;
        let Some(&(_x, _y, h)) = self.list_origin.get(&(pane, tab)) else {
            return count;
        };
        let fit = (((h - 2.0 * PAD) / ROW_H).floor().max(0.0)) as usize;
        count.max(fit)
    }

    /// prepaint 回写回收站面板内容区高度；值变化时返回 `true`（调用方借此 notify，
    /// 下一帧才能把斑马纹补满一屏——与 [`Self::set_list_origin`] 同一条规矩）。
    pub(crate) fn set_trash_body_h(&mut self, h: f32) -> bool {
        let changed = (self.trash_body_h - h).abs() > f32::EPSILON;
        if changed {
            self.trash_body_h = h;
        }
        changed
    }

    /// 「回收站斑马纹铺满一屏」要补的占位行数：内容区装得下多少 24px 行，减去
    /// 真实行数就是补足数。首帧高度还没回写时返回 0（prepaint 后下一帧自然铺满）。
    pub(crate) fn trash_fill_rows(&self) -> usize {
        const ROW_H: f32 = 24.0;
        // body 的 py(6.0)：上下各 6px 的内边距不参与斑马纹。
        const BODY_PAD: f32 = 12.0;
        if self.trash_body_h <= BODY_PAD {
            return 0;
        }
        (((self.trash_body_h - BODY_PAD) / ROW_H).floor().max(0.0)) as usize
    }

    /// 把每个标签页的网格 / 画廊**列数**记下来（键盘定位换算单元行要用）。
    ///
    /// 列数取决于容器宽度与图标缩放，只有渲染路径知道（`available` 是每窗格宽）；
    /// 列表 / 列视图恒记 1——它们不用这个换算（前者按行、后者横向分栏）。
    fn sync_grid_cols(&mut self, available: f32) {
        let zoom = listing::Zoom(self.ui.icon_scale);
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                let cols = match tab.view_mode {
                    ViewMode::Grid | ViewMode::Gallery => {
                        zoom.columns_for(tab.view_mode, available)
                    }
                    _ => 1,
                };
                tab.grid_cols = cols;
            }
        }
    }

    // ------------------------------------------------------------ 框选（列表视图）

    /// 列表视图里按下左键：只有点在**空白区**（顶部留白 / 末行以下）才启动框选，
    /// 点到某个条目就交给那行的单选 / 拖拽逻辑，不在这里抢。
    pub(crate) fn start_box_selection_if_empty(
        &mut self,
        pane: usize,
        tab: usize,
        x: f32,
        y: f32,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        let (count, scroll_y) = {
            let Some(panel) = self.panel_at(pane, tab) else {
                return;
            };
            // 框选只在列表视图，行数用**列表行数**（分组开启时含组头行）。
            let count = panel.list_count;
            let scroll_y = f32::from(panel.scroll.0.borrow().base_handle.offset().y);
            (count, scroll_y)
        };
        let Some(&(_lx, ly, _lh)) = self.list_origin.get(&(pane, tab)) else {
            return;
        };
        const ROW_H: f32 = 24.0;
        const PAD: f32 = 12.0;
        // ⚠️ 符号：gpui 的滚动偏移约定「向下滚为负」（`scroll_top = -offset.y`），
        // 视口 y 折算内容 y 要**减去**它（负负得正 = 加回滚过的距离）。曾经写成
        // `+ scroll_y`，列表一滚就整段折成负下标——每一下点击都被判成「空白区」
        // 启动框选，抬起时清空选择：表现正是「翻到后面的数据鼠标点不中文件」。
        let rel = y - ly - PAD - scroll_y;
        // 行带按**顶边**对齐（floor）：行 i 占 [i*24, (i+1)*24)。原先用 round，
        // 行带整体错开半行——最后一行的下半截被折到行数之外（按下去当空白起橡皮筋）、
        // 列表下方 12px 内反被折成最后一行（点空白选中了它），两个都是用户可感的怪。
        let idx = (rel / ROW_H).floor() as isize;
        // 落在已存在行带内 = 点到某个条目，走单选 / 拖拽，不在这里框选。
        if idx >= 0 && idx < count as isize {
            return;
        }
        self.box_selection = Some(BoxSelection {
            pane,
            tab,
            extend,
            origin: (x, y),
            cursor: (x, y),
        });
        if !extend {
            if let Some(p) = self.panel_at_mut(pane, tab) {
                p.selection.clear();
            }
        }
        cx.notify();
    }

    /// 把当前框选矩形折算成可见行下标区间 `(pane, tab, extend, min, max)`。
    ///
    /// 几何：行 `i` 顶边在窗口坐标 = `list_origin.y + PAD + i*ROW_H - scroll_y`，
    /// 反解 y 得下标（行带按顶边对齐，floor，见 `start_box_selection_if_empty`）。
    /// 矩形与行带**不相交**（整段在最后一行之下 / 第一行之上）时返回 `None`——
    /// 不能钳到边界，否则在空白处单击一下就变成「选中最后一行」。
    /// 拿不到列表几何时同样返回 `None`。
    fn box_selection_range(&self) -> Option<(usize, usize, bool, usize, usize)> {
        let box_sel = self.box_selection.as_ref()?;
        let (pane, tab) = (box_sel.pane, box_sel.tab);
        let (_lx, ly, _lh) = *self.list_origin.get(&(pane, tab))?;
        let panel = self.panel_at(pane, tab)?;
        let count = panel.list_count;
        let scroll_y = f32::from(panel.scroll.0.borrow().base_handle.offset().y);
        let (min, max) = box_row_range(box_sel.origin.1, box_sel.cursor.1, ly, scroll_y, count)?;
        Some((pane, tab, box_sel.extend, min, max))
    }

    /// 框选拖动中：实时更新橡皮筋 + 本地选择（抬起时再回灌 app）。
    pub(crate) fn update_box_selection(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        if let Some(box_sel) = self.box_selection.as_mut() {
            box_sel.cursor = (x, y);
        }
        let Some((pane, tab, extend, min, max)) = self.box_selection_range() else {
            return;
        };
        if let Some(p) = self.panel_at_mut(pane, tab) {
            let wlen = p.window.len() as isize;
            if wlen <= 0 {
                return;
            }
            let ws = p.window_start as isize;
            // 行区间 [min, max] ∩ 窗口里的条目（跳过组头）。行流保持条目序，
            // 所以这些条目在「窗口条目序」里仍是连续一段 [first, last]，
            // 本地先按它给即时反馈；抬起时 app 侧按行区间重算（select_rows_range）。
            let ordered: Vec<mo_core::FileId> = p.window_entries().map(|e| e.id).collect();
            let mut first = None;
            let mut last = None;
            let mut k = 0usize;
            for (ri, row) in p.window.iter().enumerate() {
                let global = ws + ri as isize;
                if row.entry().is_some() {
                    if global >= min as isize && global <= max as isize {
                        if first.is_none() {
                            first = Some(k);
                        }
                        last = Some(k);
                    }
                    k += 1;
                }
            }
            if let (Some(f), Some(l)) = (first, last) {
                if !extend {
                    p.selection.clear();
                }
                p.selection.select_range(&ordered, f, l);
            }
        }
        cx.notify();
    }

    /// 框选抬起：回灌 app 侧选择（清旧 + 选区间），并收掉橡皮筋。
    ///
    /// 橡皮筋没碰到任何行 = 在空白处**单击**：只清选择、什么都不选——
    /// 曾经这里把界外下标钳到最后一行，「点空白」变成了「选最后一条」。
    pub(crate) fn finish_box_selection(&mut self, cx: &mut Context<Self>) {
        if self.box_selection.is_none() {
            // 根级的左键抬起都在这里过一遍；没在框选就什么都不做。
            return;
        }
        let range = self.box_selection_range();
        let extend = self.box_selection.as_ref().is_some_and(|b| b.extend);
        self.box_selection = None;
        let Some((_pane, _tab, _extend, min, max)) = range else {
            let app = self.app();
            let this = cx.entity().clone();
            cx.spawn(async move |_weak, cx| {
                app.clear_selection().await;
                // app 侧选择是唯一事实来源：清完回灌 UI 高亮。
                pull_selection(&app, &this, cx).await;
            })
            .detach();
            cx.notify();
            return;
        };
        let app = self.app();
        let this = cx.entity().clone();
        cx.spawn(async move |_weak, cx| {
            if !extend {
                app.clear_selection().await;
            }
            // 行区间回灌：app 侧把区间内的条目（跳过组头）折成条目位再选。
            app.select_rows_range(min, max).await;
            // app 侧选择是唯一事实来源：框选结果回灌 UI 高亮。
            pull_selection(&app, &this, cx).await;
        })
        .detach();
        cx.notify();
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
            self.run_transfer(d, pane, path, alt, cx);
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
        let refresh_app = self.pane_app(pane_idx);
        self.run_transfer(d, pane_idx, dest, alt, cx);
        // 目标窗格可能没开监听：主动刷一次让新文件立刻出现。
        if let Some(app) = refresh_app {
            cx.spawn(async move |_weak, _cx| {
                let _ = app.refresh().await;
            })
            .detach();
        }
    }

    /// 某个窗格**当前标签页**的 `AppState`（分栏拖拽要问目标那一头的后端）。
    fn pane_app(&self, pane: usize) -> Option<AppState> {
        self.panes
            .get(pane)
            .and_then(|p| p.tabs.get(p.active))
            .map(|t| t.app.clone())
    }

    /// 真正提交复制 / 移动：按住 ⌥ 是移动，否则复制。
    ///
    /// 源端取**拖拽来源窗格**的 `AppState`，目标端取**落点窗格**的——分栏时两个窗格
    /// 可能各连着不同的后端（一个本地一个远程，甚至两条不同会话），端点必须各问各的
    /// 窗格；照源窗格去猜目标那一头，会把「拖到另一个窗格的远程目录」当成本地路径交给
    /// `std::fs`（写进本机同名位置，或直接报不存在）。
    fn run_transfer(
        &mut self,
        d: DragState,
        dest_pane: usize,
        dest: PathBuf,
        alt: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(src_app) = self.panel_at(d.pane, d.tab).map(|p| p.app.clone()) else {
            return;
        };
        // 落点窗格一时取不到（正好被关掉）时退回源端：同窗格拖拽这一路本来两端相同。
        let dest_app = self.pane_app(dest_pane).unwrap_or_else(|| src_app.clone());
        let src_ep = src_app.endpoint();
        let dest_ep = dest_app.endpoint();
        let paths = d.paths.clone();
        cx.spawn(async move |_weak, _cx| {
            let _ = src_app
                .transfer_between(paths, src_ep, &dest, dest_ep, alt)
                .await;
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
                .and_then(|p| p.window_entries().find(|e| e.path == *path).map(|e| e.id));
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
                p.window_entries()
                    .filter(|e| p.selection.is_selected(&e.id))
                    .map(|e| e.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        let (target_path, is_dir) = match target {
            Some((p, d)) => (Some(p), d),
            None => (None, true),
        };
        // 本机专属动作（在访达中显示）要不要出现，就看这一刻
        // 是不是在看远程——菜单里的可见性必须在**打开时**定下来（之后列表可能变）。
        let remote = self
            .panel_at(pane, tab)
            .map(|p| p.app.browsing_remote())
            .unwrap_or(false);
        self.context_menu = Some(crate::context_menu::ContextMenu {
            x,
            y,
            target: target_path.clone(),
            is_dir,
            selected: paths.len(),
            paths,
            remote,
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
                    // `is_dir` 取自右键时那个条目在列表里的 `kind`（见 `open_entry`）。
                    self.open_entry(p, is_dir, cx);
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
                // 选一个不在候选里的应用：macOS 没有系统对话框，走 Mo 自己的选择器；
                // Windows 有 `openas` 动词，用系统的。
                let Some(p) = target else { return };
                if mo_app::AppState::has_app_picker() {
                    self.open_app_picker(p, cx);
                    return;
                }
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
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    if let Err(e) = app.delete_selection().await {
                        this.update(cx, |v, cx| {
                            v.notice(format!("删除失败：{e}"), None, cx);
                        });
                    }
                })
                .detach();
            }
            A::RevealInFileManager => {
                let app = self.app();
                let this = cx.entity().clone();
                let paths = menu.paths.clone();
                cx.spawn(async move |_weak, cx| {
                    if let Err(e) = app.reveal_in_file_manager(paths).await {
                        this.update(cx, |v, cx| {
                            v.notice(format!("显示失败：{e}"), None, cx);
                        });
                    }
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
            A::InvertSelection => {
                let app = self.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    app.select_invert_visible().await;
                    pull_selection(&app, &this, cx).await;
                })
                .detach();
            }
            A::Stage => self.stage_selection(cx),
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
    // 正在读取的目标（可能还没读回来）：UI 靠它立刻给反馈。
    let opening = app.opening_path();
    let count = app.visible_count().await;
    let grouping = app.grouping().await;
    let list_count = app.list_row_count().await;
    let sort = app.sort().await;
    let can_back = app.can_go_back().await;
    let can_forward = app.can_go_forward().await;
    let ops = app.operations_snapshot().await;
    let indexed = app.index_count();
    let trash_entries = app.trash_list();
    // app 侧选择是唯一事实来源；⌘A / 键盘移动等改动都从这里回灌 UI。
    let selection_ids = app.selection_ids().await;

    let outcome = this.update(cx, |v, cx| {
        let ui_path = match v.panel_at(pane_idx, tab_idx) {
            Some(p) => p.path.clone(),
            None => return None,
        };
        v.indexed = indexed;
        // 回收站多选挂在行下标上：内容**真的变了**（还原 / 删除 / 清空，长度
        // 必变）才作废多选与游标——任何总线事件都会重跑 sync，见事件就清会把
        // 刚点出来的选择立刻吹掉。
        if v.trash_entries != trash_entries {
            v.trash_selected.clear();
            v.trash_anchor = None;
            v.palette_index = 0;
        }
        v.trash_entries = trash_entries;
        // 回收站条目变了（还原 / 清空 / 新删除）就补缺的大小缓存：
        // fire-and-forget（无缺键即空转），绝不挂在 sync 的 await 链上。
        v.ensure_trash_sizes(cx);
        let p = v.panel_at_mut(pane_idx, tab_idx)?;
        // 列表视图当前应该用哪种窗口空间：分组开启才按「行」（含组头）取。
        let want_grouped =
            p.view_mode == crate::panel::ViewMode::List && grouping != mo_app::Grouping::None;
        // 切换目录 / 改过滤词 / 行数变化 / **窗口空间切换**后，旧窗口的下标已失效，
        // 直接作废。空间切换不改变条目数（count 不变），必须单独比标记。
        if ui_path != path
            || p.visible_count != count
            || p.list_count != list_count
            || p.window_is_grouped != want_grouped
        {
            tracing::debug!(
                target: "mo_ui::window",
                pane = pane_idx, tab = tab_idx,
                ui_path = ?ui_path, app_path = ?path,
                ui_count = p.visible_count, app_count = count,
                ui_rows = p.list_count, app_rows = list_count,
                was_grouped = p.window_is_grouped, want_grouped,
                "sync invalidated window"
            );
            p.window.clear();
            p.window_start = 0;
            p.pending = None;
        }
        p.path = path;
        p.opening = opening;
        p.visible_count = count;
        p.list_count = list_count;
        p.grouping = grouping;
        p.window_is_grouped = want_grouped;
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
        Some((p.window_start..p.window_start + len, want_grouped))
    });
    let Ok(Some((range, want_grouped))) = outcome else {
        // 视图已销毁 / 标签页已关闭 / 补窗任务在途（本轮跳过）。
        return;
    };

    let (dir_path, start, rows) = app.list_window(range, want_grouped).await;
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
        p.window = rows;
        p.window_is_grouped = want_grouped;
        cx.notify();
    });
    // 对比着色跟着导航走：换了一侧目录就重比（内部判根，同目录不重扫）。
    let _ = this.update(cx, |v, cx| v.refresh_compare_if_stale(cx));
    // 缩略图不在这里派发：这里的窗口最大到 INITIAL_WINDOW(=200) 条，按它派发就是
    // 进一个图片目录瞬间排 200 个解码任务（单张 80ms 级）——四核被占住，界面跟着卡。
    // 派发点在渲染那一帧，只给**看得见**的行排队（见 `file_list` 的 `want_thumbs`）。
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

        // 「连接到服务器」的地址输入框：开着就确保存在并握着焦点，关了就回收。
        // 放在这里（而不是打开 / 关闭两处）见 `sync_connect_input` 的说明。
        self.sync_connect_inputs(window, cx);

        let visible_panes = if self.split && self.panes.len() > 1 {
            2
        } else {
            1
        };
        // 每个窗格可用的横向宽度：网格 / 画廊按它算列数（uniform_list 必须先知道行数）。
        let viewport_w = window.viewport_size().width.to_f64() as f32;
        let sidebar_w = if self.ui.sidebar { SIDEBAR_WIDTH } else { 0.0 };
        let per_pane_w = ((viewport_w - sidebar_w) / visible_panes as f32 - 24.0).max(160.0);

        // 模态打开时，工具栏 / 状态栏保留，中央区按模态性质二选一：
        // * **对话框**（B 类）：带遮罩的浮层，下层照常渲染、透过遮罩可见，
        //   所以这里与 `None` 一样渲染正常浏览区，而不是顶掉整个中央区；
        // * **次级视图**（A 类）：内容多、要停留，正当占满中央区，
        //   由 `central_view` 提供标题栏（见 `dialogs.rs` / 各 `render_*`）。
        // 例外：`ConfirmTrash` 虽是 B 类浮层，但它是**从回收站面板里**弹出来的
        // 确认卡——中央区要渲染的是回收站面板（见下一档的 ConfirmTrash 分支），
        // 而不是确认前的浏览区。
        let body: Div = match &self.modal {
            // 对话框（B 类）：走带遮罩的浮层，中央区照常渲染浏览区。
            Modal::None
            | Modal::Info(_)
            | Modal::ConnectServer
            | Modal::ConnectAuth
            | Modal::Properties
            | Modal::Archive
            | Modal::Tags
            | Modal::Settings
            | Modal::CommandPalette => {
                let mut row = div().flex().flex_row().flex_1().min_w_0().min_h_0();
                // 侧边栏可关（配置 `ui.sidebar`）；关掉时不参与宽度计算。
                if self.ui.sidebar {
                    // 浏览远程时**不给「当前位置」**：远程绝对路径和本地路径在字符串上
                    // 分不开（FTP 的 home 往往就是本机 home），拿去比对本地快捷访问，
                    // 会让「图片」「文档」这些本地项跟着一起高亮——用户报的「关闭弹窗
                    // 后左侧选中了 2 个项目」有一半来自这里。
                    let current = (!self.panel().app.browsing_remote())
                        .then(|| {
                            // 正在读取时先高亮**目标**：刚点下去那 100–300ms 里选中态就得动，
                            // 否则观感就是「点了没反应」。
                            self.panel()
                                .opening
                                .clone()
                                .or_else(|| self.panel().path.clone())
                        })
                        .flatten();
                    row = row.child(sidebar::render(&self.panel().app, &current, false, &entity));
                }
                self.ensure_columns(cx);
                // 键盘定位要把条目位换算成网格 / 画廊的**单元行**，而列数只有在
                // 这里（拿到容器宽度 `per_pane_w`）才算得出来——渲染前写回 panel，
                // 定位路径读它（幂等缓存，不触发重绘，同 `list_origin` 那条约定）。
                self.sync_grid_cols(per_pane_w);
                for i in 0..visible_panes {
                    row = row.child(render_pane(self, i, &entity, per_pane_w));
                }
                row
            }
            // 次级视图（A 类）：占满中央区，由 `central_view` 提供标题栏。
            // 其中「浏览型」的两个（全局搜索 / 回收站）**保留侧栏**——它们本质上
            // 还是在挑文件，左侧导航得一直在（用户报：进回收站左侧整个没了）。
            // 其余（扩展 / 比较…）是工具页，全宽无妨。
            // 回收站确认卡（ConfirmTrash）也按回收站画：面板保留在遮罩后面。
            Modal::GlobalSearch
            | Modal::Trash
            | Modal::ContentSearch
            | Modal::ConfirmTrash(_)
            | Modal::TrashRename(_) => {
                let in_trash = matches!(
                    self.modal,
                    Modal::Trash | Modal::ConfirmTrash(_) | Modal::TrashRename(_)
                );
                let mut row = div().flex().flex_row().flex_1().min_w_0().min_h_0();
                if self.ui.sidebar {
                    // 回收站里没有「当前位置」可言：别把进面板前的目录高亮留着，
                    // 改高亮侧栏的「回收站」入口本身（见 sidebar::render 的 trash_active）。
                    let current = if in_trash {
                        None
                    } else {
                        (!self.panel().app.browsing_remote())
                            .then(|| {
                                self.panel()
                                    .opening
                                    .clone()
                                    .or_else(|| self.panel().path.clone())
                            })
                            .flatten()
                    };
                    row = row.child(sidebar::render(
                        &self.panel().app,
                        &current,
                        in_trash,
                        &entity,
                    ));
                }
                if in_trash {
                    row.child(self.render_trash(&entity))
                } else if self.modal == Modal::ContentSearch {
                    row.child(self.render_content_search(&entity))
                } else {
                    row.child(self.render_global_search(&entity))
                }
            }
            Modal::Diff => self.render_diff(),
            Modal::BatchRename => dialogs::batch_rename(self, &entity),
            Modal::DiskUsage => dialogs::disk_usage(self, &entity),
            Modal::Extensions => self.render_extensions(&entity),
            Modal::Duplicates => self.render_dedup(&entity),
            Modal::Workflow => self.render_workflow(),
            Modal::Sync => self.render_sync(&entity),
        };

        let ops = self.all_ops();
        // 任务全部移除后**自动收起**任务浮层：空浮层没有存在意义，且下次有新任务
        // 时应当以常显卡片出现、而不是凭空开着一张旧浮层。
        if ops.is_empty() && self.ops_open {
            self.ops_open = false;
        }
        let op_speeds = self.op_speeds(&ops);
        let panel = self.panel();
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
                // 回收站面板开着时，视图按钮组读写的是回收站自己的视图状态
                // （`trash_view_mode`），不是背后目录的。
                if self.is_in_trash() {
                    self.trash_view_mode
                } else {
                    panel.view_mode
                },
                // 回收站面板开着（含其上的确认卡）时，地址栏显示「回收站」，
                // 不再回显进面板前的目录。
                self.is_in_trash(),
            ))
            .child(body);

        // 分栏对比的图例条（只有对比开着时才占一行）。
        if self.compare {
            root = root.child(compare_legend(self, &entity));
        }

        // 暂存区抽屉：展开时贴在中央区下方、状态栏之上（收集 / 投递都在这一条里）。
        if self.staging_open {
            root = root.child(crate::staging::render_tray(&app, &entity));
        }

        // 状态栏可关（配置 `ui.status_bar`）。
        if self.ui.status_bar {
            root = root.child(status_bar::render(
                status_bar::Stats {
                    count: panel.visible_count,
                    selection_count,
                    indexed: self.indexed,
                    can_undo,
                    can_redo,
                    staged: self.app().staged_count(),
                },
                &panel.path,
                &panel.query,
                &entity,
            ));
        }

        // 传输指示：左下角小块 + 浮层（绝对定位，不占布局；没有任务时不画）。
        // 原先是整条横幅铺在窗口底部，任务会把状态栏顶上去一截，已废弃。
        root = root.child(progress_panel::render_overlay(
            &ops,
            &op_speeds,
            &app,
            self.ops_open,
            &entity,
        ));

        // 键盘路由：全局快捷键 + 模态内导航 + 输入即过滤。
        root.interactivity().on_key_down(move |ev, _window, cx| {
            let key = ev.keystroke.key.as_str();
            let m = &ev.keystroke.modifiers;
            let shift = m.shift;
            let plain = !m.control && !m.alt && !m.platform;

            // 右键菜单 / 传输浮层开着时 Esc 先关掉它们（其余按键继续走正常路由，
            // 菜单不该像模态那样吃掉方向键 / 输入即过滤）。
            if key == "escape" {
                let consumed = entity_key.update(cx, |v, cx| {
                    let mut consumed = false;
                    if v.context_menu.is_some() {
                        v.close_context_menu(cx);
                        consumed = true;
                    }
                    if v.ops_open {
                        v.ops_open = false;
                        consumed = true;
                    }
                    if consumed {
                        cx.notify();
                    }
                    consumed
                });
                if consumed {
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
                    // 模态 / 对话框打开时，作用于下层文件列表的动作**吞掉**：
                    // 那个列表在遮罩后面，选中项 / 剪贴板却在背后被改掉。最容易被
                    // 撞见的是 ⌘A——在「连接到服务器」的输入框里按 ⌘A，选中的是后面
                    // 的文件（用户报的就是这个）；⌘X / ⌘V / ⌘Z / Delete 更糟。
                    // 只拦这一类：导航 / 标签页 / 视图 / 切换模态照旧（Finder 的
                    // sheet 也是这个尺度），否则模态一开连 ⌘T 都没了。
                    if entity_key.read(cx).modal != Modal::None
                        && crate::keys::touches_the_browser(action)
                    {
                        // 但回收站这类模态自己要吃一部分组合键（⌘A 全选）——
                        // 先交给模态处理器；其余模态不认识这颗键，结果仍是吞掉
                        // （原行为）。只路由、不 return 掉语义：模态处理器对
                        // 不认识的键就是空操作。
                        handle_modal_key(key, plain, &combo, &entity_key, cx);
                        return;
                    }
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

            // 非模态：键盘导航 + 输入即定位（type-ahead）+ 快速预览 + 删除选中。
            match key {
                "up" | "down" if plain || shift => {
                    let app = entity_key.update(cx, |v, _cx| v.app());
                    let this = entity_key.clone();
                    let step = if key == "up" { -1 } else { 1 };
                    let extend = shift;
                    cx.spawn(async move |cx| {
                        // `locate_cursor` 已经移动并选中了新行，这里多拿它返回的
                        // 定位结果把目标滚进可视区（方向键之前没滚动，列表滚远了
                        // 选中项会跑到视口外看不见）。`Nearest` 只在不可见时才滚。
                        // 行号按当前视图换算：列表 / 列视图用列表行，网格 / 画廊
                        // 用条目位 ÷ 列数（它们的 `uniform_list` 按单元行计）。
                        if let Some(hit) = app.locate_cursor(step, extend).await {
                            this.update(cx, |v, cx| {
                                let row = located_row(v.panel(), hit);
                                v.panel_mut()
                                    .scroll
                                    .scroll_to_item(row, gpui_kit::ScrollStrategy::Nearest);
                                cx.notify();
                            });
                        }
                        // app 侧选择是唯一事实来源：移动后回灌 UI 高亮。
                        pull_selection(&app, &this, cx).await;
                    })
                    .detach();
                }
                "backspace" => {
                    // 定位缓冲非空：退格删掉最后一个字符并重定位（type-ahead 编辑），
                    // 缓冲空时回落「返回上级目录」（原有便利行为保留）。
                    let this = entity_key.clone();
                    let (to_parent, app, q, gen) = entity_key.update(cx, |v, cx| {
                        if v.panel().query.is_empty() {
                            (true, v.app(), String::new(), 0)
                        } else {
                            let app = v.app();
                            let p = v.panel_mut();
                            p.query.pop();
                            p.type_ahead_gen += 1;
                            cx.notify();
                            (false, app, p.query.clone(), p.type_ahead_gen)
                        }
                    });
                    if to_parent {
                        cx.spawn(async move |_cx| {
                            let _ = app.open_parent().await;
                        })
                        .detach();
                    } else {
                        // 退格后重新定位：不跳下一个（`skip_current = false`）。
                        run_type_ahead(app, this, q, false, gen, cx);
                    }
                }
                "escape" => {
                    entity_key.update(cx, |v, cx| {
                        v.panel_mut().query.clear();
                        // Esc 直接清空定位缓冲即可，不再触发过滤。
                        cx.notify();
                    });
                }
                k if plain && k.chars().count() == 1 => {
                    let ch = k.chars().next().unwrap();
                    // 输入即定位（type-ahead）：只逐字累积前缀缓冲、跳到匹配项
                    // 并滚进可视区，**不收窄列表**——跟 Finder / 资源管理器一致。
                    // 连按同一个字母（缓冲变成 "aa"/"aaa"）跳到**下一个**匹配项。
                    let this = entity_key.clone();
                    let (app, q, skip, gen) = entity_key.update(cx, |v, cx| {
                        // 先取走 `app`（克隆后即释放对 `v` 的借用），再拿
                        // `panel_mut()` 做可变修改，避免借用冲突。
                        let app = v.app();
                        let p = v.panel_mut();
                        p.query.push(ch);
                        // 刷新代数：让更早的自动重置定时器失效，避免误清空本次输入。
                        p.type_ahead_gen += 1;
                        cx.notify();
                        let skip = type_ahead_repeats_one_char(&p.query);
                        (app, p.query.clone(), skip, p.type_ahead_gen)
                    });
                    run_type_ahead(app, this, q, skip, gen, cx);
                }
                _ => {}
            }
        });

        // 框选（列表视图橡皮筋）：拖动中实时更新，抬起收尾并回灌 app 侧选择。
        // 鼠标事件冒泡到根容器，所以即便指针移到行 / 滚动条上也收得到。
        let box_entity = entity.clone();
        root.interactivity().on_mouse_move(move |ev, _window, cx| {
            if box_entity.read(cx).box_selection.is_none() {
                return;
            }
            let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
            box_entity.update(cx, |v, cx| v.update_box_selection(x, y, cx));
        });
        let box_entity_up = entity.clone();
        root.interactivity()
            .on_mouse_up(MouseButton::Left, move |_ev, _window, cx| {
                box_entity_up.update(cx, |v, cx| v.finish_box_selection(cx));
            });

        // 让焦点落在本视图上，否则按键不会派发到这里。
        // ⚠️ 三个例外——焦点属于真实输入组件时不能抢：
        // * 地址栏编辑态：抢回来会立刻给输入框一个 Blur → 触发 end_address_edit，
        //   表现为「点编辑闪一下又退回显示态」；
        // * 「连接到服务器」对话框：抢走刚给地址框的焦点，打字就没反应了；
        // * 「需要登录（认证）」弹窗：同上，而且那里有两个框要 Tab 切换。
        let input_owns_focus = self.panel().address_editing
            || matches!(self.modal, Modal::ConnectServer | Modal::ConnectAuth);
        if !self.focus.is_focused(window) && !input_owns_focus {
            cx.focus_self(window);
        }

        // 对话框（B 类）：一律是带遮罩的浮层——下层内容照常渲染、透过半透明遮罩
        // 可见，但被 `.occlude()` 挡住点不到；居中一张卡片。点遮罩空白 / Esc 关闭
        // （按键路由见 `handle_modal_key`）。这里在文末挂上，画在浏览区之上。
        match &self.modal {
            // 信息提示：与其它对话框共用外壳，只是无标题栏；按钮文字可经
            // `notice(text, Some(..))` 自定义，默认「知道了」。
            Modal::Info(msg) => {
                let label = self
                    .notice_ok
                    .clone()
                    .unwrap_or_else(|| "知道了".to_string());
                root = root.child(render_notice_overlay(msg, &label, &entity));
            }
            Modal::ConfirmTrash(action) => {
                root = root.child(render_trash_confirm(self, action, &entity));
            }
            Modal::TrashRename(entry) => {
                root = root.child(dialogs::trash_rename(self, entry, &entity));
            }
            Modal::ConnectServer => root = root.child(self.render_connect(&entity)),
            Modal::ConnectAuth => root = root.child(self.render_connect_auth(&entity)),
            Modal::Properties => root = root.child(dialogs::properties(self, &entity)),
            Modal::Archive => root = root.child(dialogs::archive(self, &entity)),
            Modal::Tags => root = root.child(dialogs::tags(&entity, self)),
            Modal::Settings => root = root.child(self.render_settings(&entity)),
            Modal::CommandPalette => root = root.child(self.render_command_palette(&entity)),
            _ => {}
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

        // 框选橡皮筋：窗口坐标直接当偏移量（与右键菜单同一套坐标系），画在最上层。
        if let Some(box_sel) = self.box_selection.as_ref() {
            let (ox, oy) = box_sel.origin;
            let (cx2, cy2) = box_sel.cursor;
            let left = ox.min(cx2);
            let top = oy.min(cy2);
            let w = (cx2 - ox).abs();
            let h = (cy2 - oy).abs();
            root = root.child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(w))
                    .h(px(h))
                    .bg(theme::selected_bg().opacity(0.22))
                    .border_1()
                    .border_color(rgb(0x0a84ff))
                    .debug_selector(|| "mo-box-selection".to_string()),
            );
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
    } else {
        // macOS：主窗口开了 `app_owns_titlebar_drag`，AppKit 已彻底退出标题栏
        // （换来「点标题栏不必先等一拍判断是不是双击」的手感），代价是整条顶栏
        // 48px 全高都得应用层自己拖。这里直接把处理器挂在行本身：按下会冒泡到它，
        // 于是标签缝隙、`pl(80)` 留给红绿灯的那块、以及「＋」右侧都被覆盖
        // —— 行为对齐并超过原来「只有上沿 ~28pt 归 AppKit」的那条带。
        // 标签 / ✕ / ＋ 的点击不受影响：click 是抬起时另算的事件，照旧触发。
        row = toolbar::attach_titlebar_drag(row);
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
            .child(match panel.view_mode {
                ViewMode::List => file_list::render(
                    entity,
                    pane_idx,
                    tab_idx,
                    // 列表用**行数**：分组开启时含组头行（与窗口快照同空间）。
                    panel.list_count,
                    &panel.scroll,
                    file_list::ListChrome {
                        cols: &view.cols,
                        sort: panel.sort,
                        dragging: view.dragging_column(),
                        resizing: view.resizing_divider(),
                        zebra: view.ui.zebra,
                        // 斑马纹铺满一屏：条目不足时补到视口装得下的行数。
                        fill_to: view.list_fill_rows(pane_idx, tab_idx, panel.list_count),
                    },
                )
                .into_any_element(),
                ViewMode::Columns => {
                    // 列视图的条目不走主目录模型，取不到「窗口」里那份 app，
                    // 所以把 panel 的 app 传进去（系统图标要走它那条链路）。
                    columns::render(
                        entity,
                        pane_idx,
                        tab_idx,
                        &panel.columns,
                        &panel.app,
                        panel.diff.as_deref(),
                    )
                    .into_any_element()
                }
                ViewMode::Grid | ViewMode::Gallery => {
                    // 图标缩放（配置 `ui.icon_scale`）：只作用于网格 / 画廊。
                    // 列数也要用同一个 `zoom` 算——方框放大了列数不变，相邻单元会互相压住。
                    let zoom = listing::Zoom(view.ui.icon_scale);
                    let cols = zoom.columns_for(panel.view_mode, available);
                    grid::render(
                        entity,
                        pane_idx,
                        tab_idx,
                        panel.visible_count,
                        cols,
                        panel.view_mode,
                        &panel.scroll,
                        zoom,
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
        // 远程标签：前面挂一枚地球图标 + 主机名，让「这个标签在看 FTP」一眼可见。
        // 用 `flex_shrink_0` 而不是并进标题串里：主机名不参与截断，被压缩的只该是
        // 后面的目录名（否则长主机名会把两边都挤没）。
        if let Some(host) = tab.remote_badge() {
            item = item.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(3.0))
                    .flex_shrink_0()
                    .text_size(px(11.5))
                    .text_color(theme::muted())
                    .child(crate::icons::icon(
                        crate::icons::GLOBE,
                        12.0,
                        theme::muted(),
                    ))
                    .child(text!(host)),
            );
        }
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
        .window_entries()
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

/// 分组方式的中文短标签（布局设置器的值列用）。
fn grouping_label(g: mo_app::Grouping) -> &'static str {
    match g {
        mo_app::Grouping::None => "不分组",
        mo_app::Grouping::Kind => "按类型",
        mo_app::Grouping::Date => "按日期",
    }
}

/// 新建面板：套用配置里的默认视图模式与默认分组（第四阶段·自定义布局）。
///
/// 认不出的 `ui.view_mode` / `ui.group` 回落默认——用户手改配置写错字符串
/// 不该让新标签页崩掉。分组是 app 侧的异步状态，这里只投递种子任务；
/// 首轮 `sync_panel`（120ms 泵）会把它同步进面板快照。
fn panel_with_prefs(app: AppState, ui: &mo_app::UiPrefs) -> Panel {
    let mode = ViewMode::from_key(&ui.view_mode).unwrap_or_default();
    let grouping = mo_app::Grouping::from_key(&ui.group).unwrap_or_default();
    app.spawn({
        let app = app.clone();
        async move {
            app.set_grouping(grouping).await;
        }
    });
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
                let n = v.palette_len();
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
        Modal::ContentSearch => {
            let hits_len = entity.update(cx, |v, _cx| v.content_hits().len());
            match key {
                "escape" => entity.update(cx, |v, cx| v.close_content_search(cx)),
                "up" | "arrowup" => entity.update(cx, |v, cx| {
                    if v.content_index > 0 {
                        v.content_index -= 1;
                    }
                    cx.notify();
                }),
                "down" | "arrowdown" => entity.update(cx, |v, cx| {
                    if hits_len > 0 {
                        v.content_index = (v.content_index + 1).min(hits_len - 1);
                    }
                    cx.notify();
                }),
                // 改过查询 / 开关 → Enter 重搜；没改 → Enter 跳到选中那条命中。
                // 少了 dirty 这层判断，「改完词按回车」会变成「打开上一条旧结果」。
                "enter" => entity.update(cx, |v, cx| {
                    if v.content_dirty {
                        v.run_content_search(cx);
                    } else {
                        v.open_content_hit(cx);
                    }
                }),
                k if plain && k.chars().count() == 1 => {
                    let ch = k.chars().next().unwrap();
                    entity.update(cx, |v, cx| {
                        v.content_query.push(ch);
                        v.content_dirty = true;
                        v.content_index = 0;
                        cx.notify();
                    });
                }
                "backspace" => entity.update(cx, |v, cx| {
                    v.content_query.pop();
                    v.content_dirty = true;
                    v.content_index = 0;
                    cx.notify();
                }),
                // ⌥ + 首字母切开关：四个开关各有专属键，不用记顺序。
                "a" if combo.alt => entity.update(cx, |v, cx| v.toggle_content_opt(0, cx)),
                "r" if combo.alt => entity.update(cx, |v, cx| v.toggle_content_opt(1, cx)),
                "w" if combo.alt => entity.update(cx, |v, cx| v.toggle_content_opt(2, cx)),
                "c" if combo.alt => entity.update(cx, |v, cx| v.toggle_content_opt(3, cx)),
                _ => {}
            }
        }
        // 回收站面板不绑 Esc（关闭）/ Delete（删除选中）/ E（清空）：这些是常规
        // 面板，快捷键反成误触源（用户要求去掉）；动作一律走面板上的按钮。
        Modal::Trash => match key {
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                v.trash_move_cursor(-1, combo.shift);
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                v.trash_move_cursor(1, combo.shift);
                cx.notify();
            }),
            // ⌘/Ctrl+A 全选（多选操作的动作都作用于**整个选中集**）。
            "a" if combo.cmd || combo.ctrl => entity.update(cx, |v, cx| {
                v.trash_select_all();
                cx.notify();
            }),
            // Enter 按平台惯例：macOS = 重命名（Finder 同款）；Windows / Linux =
            // 打开（资源管理器同款）。还原走面板上方的「还原」按钮。
            "enter" => {
                if crate::keys::has_command_key() {
                    on_trash_rename_start(entity, cx);
                } else {
                    on_trash_open(entity, cx);
                }
            }
            // 非 macOS 平台的重命名专属键（资源管理器同款 F2）：那边 Enter 归了
            // 「打开」，不接 F2 就等于整个面板没有重命名入口。
            "f2" if !crate::keys::has_command_key() => on_trash_rename_start(entity, cx),
            // 空格 = 快速预览**实际落点**文件（Finder 废纸篓同款 Quick Look）。
            // 预览的是 `trashed`（真在 ~/.Trash / 卷上 .Trashes 的本地文件），
            // 不是已不存在的原路径。
            "space" if plain => {
                let selected = entity.update(cx, |v, _cx| {
                    v.trash_entries
                        .get(v.palette_index)
                        .map(|e| e.trashed.clone())
                });
                if let Some(path) = selected {
                    let app = entity.update(cx, |v, _cx| v.app());
                    let this = entity.clone();
                    cx.spawn(async move |cx| match app.preview(&path) {
                        Ok(pv) => show_preview_twopass(&app, &this, pv, cx),
                        Err(e) => {
                            this.update(cx, |v, cx| {
                                v.modal = Modal::Info(format!("无法预览 {}：{e}", path.display()));
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                }
            }
            _ => {}
        },
        Modal::ConfirmTrash(_) => match key {
            "escape" => dismiss_trash_confirm(entity, cx),
            "enter" => confirm_trash_action(entity, cx),
            _ => {}
        },
        Modal::TrashRename(_) => match key {
            "escape" => dismiss_trash_rename(entity, cx),
            "enter" => commit_trash_rename(entity, cx),
            "backspace" => entity.update(cx, |v, cx| {
                v.trash_rename_name.pop();
                cx.notify();
            }),
            k if plain && k.chars().count() == 1 => {
                let ch = k.chars().next().unwrap();
                entity.update(cx, |v, cx| {
                    v.trash_rename_name.push(ch);
                    cx.notify();
                });
            }
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
                            // 磁盘分析扫的是**本地**路径：连远程时也得先切回本地，
                            // 否则拿本地路径去远程后端读必然失败（同侧边栏快捷访问）。
                            let _ = app.open_local(&u.path).await;
                        })
                        .detach();
                    }
                    close_modal(entity, cx);
                }
            }
            // M：条形图 ↔ 磁盘地图。地图模式下 ↑↓ / Enter 仍按条形图那一份走
            // （同一批子项，只是看法不同），所以这里只换视图。
            "m" => entity.update(cx, |v, cx| v.toggle_usage_map(cx)),
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
        Modal::Settings => {
            // 捕获态（快捷键页）：下一次按键就是新键位（Esc 取消，不绑）。
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
            // ← / → 在三个标签页之间循环切换（各页 ↑↓ 语义不同，先分流）。
            match key {
                "left" | "arrowleft" => entity.update(cx, |v, cx| v.settings_step(-1, cx)),
                "right" | "arrowright" => entity.update(cx, |v, cx| v.settings_step(1, cx)),
                "escape" => {
                    // 外观页的预览没落盘：Esc 与点遮罩一致——还原预览并关窗
                    //（theme_cancel 自己会把 modal 置回 None）。
                    if entity.read(cx).settings_tab == SettingsTab::Theme {
                        entity.update(cx, |v, cx| v.theme_cancel(cx));
                    } else {
                        close_modal(entity, cx);
                    }
                }
                "up" | "arrowup" | "down" | "arrowdown" | "enter" | "delete" | "backspace"
                | "r" => {
                    let tab = entity.read(cx).settings_tab;
                    match tab {
                        SettingsTab::Layout => match key {
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
                        SettingsTab::Theme => match key {
                            "up" | "arrowup" => entity.update(cx, |v, cx| v.theme_move(-1, cx)),
                            "down" | "arrowdown" => entity.update(cx, |v, cx| v.theme_move(1, cx)),
                            "enter" => entity.update(cx, |v, cx| v.theme_commit(cx)),
                            _ => {}
                        },
                        SettingsTab::Keys => match key {
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
                        },
                    }
                }
                _ => {}
            }
        }
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
        Modal::ConnectServer => match key {
            "escape" => entity.update(cx, |v, cx| {
                v.modal = Modal::None;
                v.connect_error = None;
                // 认证弹窗状态一并清掉（地址→认证中间态被 Esc 打断的情况）。
                v.connect_auth = None;
                // 输入框在下一帧渲染时回收（见 `sync_connect_inputs`）。
                cx.notify();
            }),
            // 回车正常由输入框消化（`InputEvent::PressEnter` → `connect_submit`）；
            // 这一条只在焦点不在输入框上时兜底。
            "enter" => entity.update(cx, |v, cx| v.connect_submit(cx)),
            // 字符 / 退格 / ⌘A / 剪贴板全归输入框，这里不再手搓字符串。
            _ => {}
        },
        Modal::ConnectAuth => match key {
            // Esc 退回地址对话框，而不是整个关掉：用户多半是想改地址
            // （端口敲错、主机名打错），把刚敲的地址一起丢掉太粗暴。
            "escape" => entity.update(cx, |v, cx| v.connect_auth_cancel(cx)),
            // 同上，回车正常由两个输入框的 `PressEnter` 订阅消化。
            "enter" => entity.update(cx, |v, cx| v.connect_auth_submit(cx)),
            _ => {}
        },
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

/// 回收站面板当前选中的条目（多选集；空则退化为游标那一条）。
fn trash_picked(v: &RootView) -> Vec<TrashEntry> {
    let picked: Vec<TrashEntry> = v
        .trash_selected
        .iter()
        .filter_map(|&i| v.trash_entries.get(i).cloned())
        .collect();
    if !picked.is_empty() {
        return picked;
    }
    v.trash_entries
        .get(v.palette_index)
        .cloned()
        .into_iter()
        .collect()
}

/// 回收站：还原**选中的**条目（多选集；普通单选时就是一条）。
fn on_trash_restore(entity: &Entity<RootView>, cx: &mut App) {
    let (entries, app) = entity.update(cx, |v, cx| {
        let entries = trash_picked(v);
        v.trash_selected.clear();
        v.trash_anchor = None;
        v.palette_index = 0;
        cx.notify();
        (entries, v.app())
    });
    if !entries.is_empty() {
        cx.spawn(async move |_cx| {
            for e in entries {
                app.restore_trash_entry(e.original).await;
            }
        })
        .detach();
    }
}

/// 回收站行 / 格的左键按下就地吃掉：别让「点条目」冒泡成「点空白」清选。
///
/// 用函数项而不是闭包：同一个闭包值注册到多个元素上会让 HRTB 推导翻车
/// （"implementation of `Fn` is not general enough"）。
fn swallow_trash_press(_ev: &MouseDownEvent, _window: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

/// 回收站：清空——**先弹确认卡**，不直接执行。
fn on_trash_empty(entity: &Entity<RootView>, cx: &mut App) {
    entity.update(cx, |v, cx| {
        v.modal = Modal::ConfirmTrash(TrashConfirm::Empty);
        cx.notify();
    });
}

/// 回收站：打开**选中的**条目（Windows / Linux 的 Enter 语义，与双击同款）：
/// 文件交系统默认应用，目录进当前窗口浏览。
fn on_trash_open(entity: &Entity<RootView>, cx: &mut App) {
    entity.update(cx, |v, cx| {
        let picked = trash_picked(v);
        for e in picked {
            v.open_trash_entry(e.trashed, e.is_dir, cx);
        }
        cx.notify();
    });
}

/// 回收站：开始重命名**游标条目**（macOS 的 Enter 语义）——弹重命名卡，
/// 输入缓冲预填当前名。
fn on_trash_rename_start(entity: &Entity<RootView>, cx: &mut App) {
    entity.update(cx, |v, cx| {
        let Some(e) = v.trash_entries.get(v.palette_index).cloned() else {
            return;
        };
        v.trash_rename_name = e
            .original
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        v.modal = Modal::TrashRename(Box::new(e));
        cx.notify();
    });
}

/// 取消重命名：回到回收站面板（面板本身没动过，多选保持原样）。
fn dismiss_trash_rename(entity: &Entity<RootView>, cx: &mut App) {
    entity.update(cx, |v, cx| {
        if matches!(v.modal, Modal::TrashRename(_)) {
            v.modal = Modal::Trash;
            v.trash_rename_name.clear();
            cx.notify();
        }
    });
}

/// 重命名提交：改实际落点 + 同步账本（[`AppState::rename_trash_entry`]），
/// 成功后面板列表经 `TrashChanged` 自动重拉，失败弹提示。
fn commit_trash_rename(entity: &Entity<RootView>, cx: &mut App) {
    let plan = entity.update(cx, |v, cx| {
        let entry = match &v.modal {
            Modal::TrashRename(e) => (**e).clone(),
            _ => return None,
        };
        let name = v.trash_rename_name.trim().to_string();
        if name.is_empty() {
            return None; // 空名不关卡：让用户继续输入或 Esc 取消。
        }
        v.modal = Modal::Trash;
        v.trash_rename_name.clear();
        cx.notify();
        Some((entry, name))
    });
    let Some((entry, name)) = plan else { return };
    let app = entity.read(cx).app();
    let this = entity.clone();
    cx.spawn(async move |cx| {
        if let Err(e) = app.rename_trash_entry(entry, name).await {
            this.update(cx, |v, cx| {
                v.notice(format!("重命名失败：{e}"), None, cx);
            });
        }
    })
    .detach();
}

/// 确认卡上点了「确认」（或按 Enter）：真正执行，然后回到回收站面板。
///
/// 取消（Esc / 取消按钮 / 点遮罩）走 [`dismiss_trash_confirm`]。
fn confirm_trash_action(entity: &Entity<RootView>, cx: &mut App) {
    let action = entity.update(cx, |v, cx| {
        let action = match std::mem::replace(&mut v.modal, Modal::Trash) {
            Modal::ConfirmTrash(a) => a,
            other => {
                // 不在确认卡上（竞态 / 重复触发）：原样放回，什么都不做。
                v.modal = other;
                return None;
            }
        };
        // 列表马上要变：选中索引归零（清空后列表空了，游标也一起归位）。
        v.palette_index = 0;
        cx.notify();
        Some(action)
    });
    let Some(action) = action else { return };
    let app = entity.read(cx).app();
    match action {
        TrashConfirm::Empty => app.empty_trash(),
    }
}

/// 取消确认：回到回收站面板（**不是** `close_modal`——那会把面板整个关掉）。
fn dismiss_trash_confirm(entity: &Entity<RootView>, cx: &mut App) {
    entity.update(cx, |v, cx| {
        if matches!(v.modal, Modal::ConfirmTrash(_)) {
            v.modal = Modal::Trash;
            cx.notify();
        }
    });
}

fn close_modal(entity: &Entity<RootView>, cx: &mut App) {
    entity.update(cx, |v, cx| {
        v.modal = Modal::None;
        v.notice_ok = None;
        v.connect_error = None;
        v.cmd_query.clear();
        v.search_query.clear();
        v.search_results.clear();
        v.diff_cache = None;
        v.palette_index = 0;
        v.trash_selected.clear();
        v.trash_anchor = None;
        // 选择器的壳借的是命令面板：一起收掉，否则下次打开面板还是「打开方式」。
        v.app_picker = None;
        cx.notify();
    });
}

/// 命令面板回车：执行选中的命令。
fn on_palette_enter(entity: &Entity<RootView>, cx: &mut App) {
    // 借这层壳的是「选择其他应用…」时，Enter = 用选中的应用打开那个文件。
    let picked = entity.update(cx, |v, _cx| {
        let picker = v.app_picker.as_ref()?;
        let chosen = filtered_apps(&v.cmd_query, &picker.apps)
            .get(v.palette_index)
            .cloned()?;
        Some((chosen.progid, picker.path.clone()))
    });
    if let Some((progid, path)) = picked {
        let app = entity.update(cx, |v, _cx| v.app());
        let this = entity.clone();
        cx.spawn(async move |cx| {
            if let Err(e) = app.open_with_app(&path, &progid).await {
                this.update(cx, |v, cx| {
                    v.modal = Modal::Info(format!("打开失败：{e}"));
                    cx.notify();
                });
            } else {
                this.update(cx, |v, cx| {
                    v.modal = Modal::None;
                    v.app_picker = None;
                    v.cmd_query.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
            }
        })
        .detach();
        return;
    }

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
        Some(CommandId::ContentSearch) => {
            entity.update(cx, |v, cx| v.open_content_search(cx));
        }
        Some(CommandId::ConnectServer) => {
            entity.update(cx, |v, cx| v.dispatch_action("server.connect", cx));
        }
        Some(CommandId::DisconnectServer) => {
            entity.update(cx, |v, cx| v.dispatch_action("server.disconnect", cx));
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
            entity.update(cx, |v, cx| v.open_trash_panel(cx));
        }
        Some(CommandId::StageSelection) => {
            entity.update(cx, |v, cx| v.stage_selection(cx));
        }
        Some(CommandId::ToggleStaging) => {
            entity.update(cx, |v, cx| v.toggle_staging(cx));
        }
        Some(CommandId::ClearStaging) => {
            entity.update(cx, |v, cx| v.clear_staged(cx));
        }
        Some(CommandId::StagedCopyHere) => {
            entity.update(cx, |v, cx| v.paste_staged(cx, false));
        }
        Some(CommandId::StagedMoveHere) => {
            entity.update(cx, |v, cx| v.paste_staged(cx, true));
        }
        Some(CommandId::ToggleCompare) => {
            entity.update(cx, |v, cx| v.toggle_compare(cx));
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
        Some(CommandId::RevealInFileManager) => {
            // 没选中就定位**当前目录**——命令面板不像右键那样一定有对象。
            entity.update(cx, |v, cx| {
                let app = v.app();
                let this = cx.entity().clone();
                cx.spawn(async move |_weak, cx| {
                    let mut paths = app.selection_paths().await;
                    if paths.is_empty() {
                        paths = app.current_path().await.into_iter().collect();
                    }
                    if let Err(e) = app.reveal_in_file_manager(paths).await {
                        this.update(cx, |v, cx| {
                            v.notice(
                                format!("{}失败：{e}", mo_platform::reveal_label()),
                                None,
                                cx,
                            );
                        });
                    }
                })
                .detach();
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
        Some(CommandId::ToggleHidden) => {
            entity.update(cx, |v, cx| {
                v.modal = Modal::None;
                v.cmd_query.clear();
                v.palette_index = 0;
                // 与 ⌘⇧. 同一条路径，避免「面板里点了」和「按了键」行为分叉。
                v.dispatch_action("view.hidden", cx);
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
        // 图标缩放三条走 UI 层：它改的是 `RootView.ui`（配置的镜像），与布局设置器
        // 同一份状态，不必再从 mo-app 绕一圈。
        Some(id @ (CommandId::ZoomIn | CommandId::ZoomOut | CommandId::ZoomReset)) => {
            entity.update(cx, |v, cx| {
                v.modal = Modal::None;
                match id {
                    CommandId::ZoomIn => v.zoom_by(1, cx),
                    CommandId::ZoomOut => v.zoom_by(-1, cx),
                    _ => v.reset_zoom(cx),
                }
                v.cmd_query.clear();
                v.palette_index = 0;
            });
        }
        Some(CommandId::GroupingCycle) => {
            entity.update(cx, |v, cx| {
                v.modal = Modal::None;
                v.cycle_grouping(cx);
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
                // 搜索索引是**本地**爬的，命中必然是本地路径：连远程时先切回本地，
                // 否则拿本地路径去远程后端读必然失败（同侧边栏快捷访问）。
                let _ = app.open_local(&h.path).await;
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
                            cx.notify();
                        });
                        show_preview_twopass(&app, &this, pv, cx);
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
        CommandId::InvertSelection => app.select_invert_visible().await,
        CommandId::ClearSelection => app.clear_selection().await,
        CommandId::DeleteSelection => {
            let _ = app.delete_selection().await;
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
        | CommandId::ContentSearch
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
        | CommandId::ToggleHidden
        // 图标缩放：改的是 UI 自己的 `ui.icon_scale`（见 `on_palette_enter`）。
        | CommandId::ZoomIn
        | CommandId::ZoomOut
        | CommandId::ZoomReset
        // 列表分组：同上，走 UI 层（`on_palette_enter` → `cycle_grouping`）。
        | CommandId::GroupingCycle
        // 暂存区五则：都需要 RootView 状态（抽屉开合 / 通知），走 `on_palette_enter`。
        | CommandId::StageSelection
        | CommandId::ToggleStaging
        | CommandId::ClearStaging
        | CommandId::StagedCopyHere
        | CommandId::StagedMoveHere
        // 对比着色同样要读两侧窗格 + 改面板状态，走 `on_palette_enter`。
        | CommandId::ToggleCompare
        | CommandId::ConnectServer
        | CommandId::DisconnectServer
        | CommandId::RevealInFileManager
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
        Ok(pv) => show_preview_twopass(app, this, pv, cx),
        Err(e) => {
            this.update(cx, |v, cx| {
                v.modal = Modal::Info(format!("无法预览 {p:?}：{e}"));
                cx.notify();
            });
        }
    }
}

/// 换预览内容：**窗口立刻切到新内容**，图片的降采样副本随后到。
///
/// 三个入口共用它：按空格（`open_quick_look`）、方向键翻页（`preview_step`）、
/// 搜索面板里回车预览。
///
/// ⚠️ 图片必须分两拍——先把 `image` 清掉，让 [`crate::preview::PreviewWindow`] 画
/// 「载入预览…」，副本生成好再 [`RootView::set_preview_image`] 换上。写成「先 await
/// 降采样、再换内容」会有两个后果：
///
/// * 首次打开时窗口要等副本生成完才出现（按空格后像没反应）；
/// * **翻页时窗口里一直挂着上一张的图**，直到新图就绪才跳变——按方向键看到的是旧图，
///   体感就是「切换有延迟」。
///
/// 预览本身不解码（`Preview.image` 只是路径），真正的解码在 UI 加载图片时发生。一张
/// 7680×4320 的 JPEG 全解码约 130MB RGBA，既慢又白占一张大纹理，所以这里先在 blocking
/// 池按长边上限生成一份副本并缓存（见 `mo_thumbnails::preview_scaled`）；之后同一张图
/// 直接命中磁盘，不再解码。降采样失败一律回退到原图路径——它是优化，不是预览能否
/// 打开的必要条件。
///
/// 快速连按时只有最后一张能贴上：`preview_seq` 代际校验在
/// [`RootView::set_preview_image`]。
/// 把一份预览拆成「可以立刻显示的那一半」与「还要后台补的图片源路径」。
///
/// ⚠️ 图片预览**必须**在这里把 `image` 摘掉：留着它，窗口就会继续显示**上一张**的图
/// （本轮修的体验问题），永远走不到占位分支。摘出来的路径拿去后台降采样，回来再由
/// [`crate::preview::PreviewWindow::set_image`] 补上。非图片预览一次给全，第二拍为空。
/// 两段式预览的「第二段要补什么」。
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreviewSecond {
    /// 图片：降采样后加载（`None` 表示直接用原图）。
    Image(PathBuf),
    /// PDF：把首页渲染成一张 PNG 再加载——渲染是平台能力，得等后台。
    Pdf(PathBuf),
}

impl PreviewSecond {
    /// 第二段要处理的那个文件（渲染 / 降采样的输入）。
    fn path(&self) -> &std::path::Path {
        match self {
            Self::Image(p) | Self::Pdf(p) => p.as_path(),
        }
    }
}

fn split_preview_for_two_pass(mut pv: Preview) -> (Preview, Option<PreviewSecond>) {
    let src = match pv.kind {
        PreviewKind::Image => pv.image.take().map(PreviewSecond::Image),
        PreviewKind::Pdf => pv.image.take().map(PreviewSecond::Pdf),
        _ => None,
    };
    (pv, src)
}

fn show_preview_twopass(app: &AppState, this: &Entity<RootView>, pv: Preview, cx: &mut AsyncApp) {
    let (head, src) = split_preview_for_two_pass(pv);
    // 立刻换内容（窗口已开着就复用，没开就现在开）——这一步不等任何 IO。
    let seq = this.update(cx, |v, cx| {
        v.show_preview(head, cx);
        v.preview_seq
    });
    let Some(src) = src else {
        return; // 文本 / 目录：本来就一次给全了。
    };
    let pool = app.clone();
    let this = this.clone();
    cx.spawn(async move |cx| {
        let probe = src.path().to_path_buf();
        match src {
            PreviewSecond::Image(src) => {
                let inner = pool.clone();
                let probe = probe.clone();
                let scaled = pool
                    .spawn_blocking(move || inner.preview_image_scaled(&probe))
                    .await
                    .ok()
                    .flatten();
                // `None` 是「用原图」（长边本来就没超上限 / 不是图片 / 降采样失败）——
                // 必须回落到原图路径，否则窗口会永远停在占位上。
                let show = scaled.unwrap_or(src);
                this.update(cx, |v, cx| v.set_preview_image(seq, show, cx));
            }
            PreviewSecond::Pdf(_) => {
                let inner = pool.clone();
                let rendered = pool
                    .spawn_blocking(move || inner.preview_pdf_page(&probe))
                    .await
                    .ok()
                    .flatten();
                this.update(cx, |v, cx| match rendered {
                    Some(png) => v.set_preview_image(seq, png, cx),
                    // 渲染不出来（平台不支持 / 加密 / 打不开）：把「正在渲染…」换成一句
                    // 说明，别让窗口永远停在占位上。
                    None => v.set_preview_text(
                        seq,
                        "PDF 文档\n\n（首页渲染不出来：平台不支持，或这个文件打不开 / 有加密）"
                            .to_string(),
                        cx,
                    ),
                });
            }
        }
    })
    .detach();
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

/// 分栏对比的图例条：统计 + 两种颜色的含义 + 关闭。
fn compare_legend(view: &RootView, entity: &Entity<RootView>) -> Stateful<Div> {
    let (lo, ro, diff) = view.compare_stats.unwrap_or((0, 0, 0));
    let swatch = |tint: Rgba, label: &'static str| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(
                div()
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded(px(2.0))
                    .bg(tint)
                    .border_1()
                    .border_color(theme::separator()),
            )
            .child(text!(label))
    };
    let row = div()
        .id("compare-legend")
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .px(px(10.0))
        .h(px(22.0))
        .flex_shrink_0()
        .bg(theme::container())
        .border_t_1()
        .border_color(theme::separator())
        .text_size(px(11.0))
        .text_color(theme::muted())
        .debug_selector(|| "mo-compare-legend".to_string())
        .child(text!(format!("对比：仅左 {lo} · 仅右 {ro} · 不同 {diff}")))
        .child(swatch(
            compare_tint(Some(mo_diff::TreeStatus::LeftOnly)).unwrap_or(theme::accent()),
            "仅此有",
        ))
        .child(swatch(
            compare_tint(Some(mo_diff::TreeStatus::Different)).unwrap_or(theme::accent()),
            "内容不同",
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(text!("⌘⇧↓ / ⌘⇧↑ 在差异间跳")),
        );

    let close = entity.clone();
    let mut btn = div()
        .id("compare-close")
        .px(px(7.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme::separator())
        .text_color(theme::text())
        .hover(|s| s.bg(theme::hover_bg()))
        .child(text!("关闭对比"))
        .debug_selector(|| "mo-compare-close".to_string());
    btn.interactivity().on_click(move |_, _window, cx| {
        close.update(cx, |v, cx| v.stop_compare(cx));
    });
    row.child(btn.test_support())
}

/// 一行搜索结果，命中区间单独上色。
///
/// 按 **字符** 切（`spans` 是字符下标）：按字节切会把一个汉字切成两半，
/// 显示出来是乱码。切成独立的 `text!` 段而不是给整行换色，是因为一行里
/// 可能有多处命中——只染其中几段才叫「高亮」。
fn highlighted_line(text: &str, spans: &[(usize, usize)], selected: bool) -> Div {
    let chars: Vec<char> = text.chars().collect();
    let mut row = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .text_size(px(12.0));
    let mut pos = 0usize;
    // 显式 id：`text!` 默认按宏调用点生成元素 id，一行里多处命中会把同一调用点
    // 画好几遍 → a11y NodeId 撞车（见 list_view::header_row 注释）。
    for (k, (a, b)) in spans.iter().enumerate() {
        let (a, b) = ((*a).min(chars.len()), (*b).min(chars.len()));
        if a > pos {
            row = row.child(text!(
                id = format!("hl-{k}-gap"),
                chars[pos..a].iter().collect::<String>()
            ));
        }
        if b > a {
            row = row.child(
                div()
                    .bg(hit_bg())
                    .rounded(px(2.0))
                    .text_color(if selected {
                        theme::selected_text()
                    } else {
                        theme::text()
                    })
                    .child(text!(
                        id = format!("hl-{k}-hit"),
                        chars[a..b].iter().collect::<String>()
                    )),
            );
        }
        pos = pos.max(b);
    }
    if pos < chars.len() {
        row = row.child(text!(
            id = "hl-tail",
            chars[pos..].iter().collect::<String>()
        ));
    }
    row
}

/// 命中高亮的底色（半透明黄：不盖掉行本身的选中 / 斑马纹底色）。
///
/// 写成函数不是常量：`rgba()` 不是 const fn，常量里调不了。
fn hit_bg() -> Rgba {
    rgba(0xf5d4424d)
}

/// 分栏对比的行底色：仅此有（蓝）/ 内容不同（橙）；**相同不染色**。
///
/// 用低透明度的一层而不是实心色：行本来还有斑马纹 / 悬停 / 选中三层底色，
/// 实心会把它们全盖掉，选中行就看不出选中了。
pub(crate) fn compare_tint(status: Option<mo_diff::TreeStatus>) -> Option<Rgba> {
    match status? {
        mo_diff::TreeStatus::Identical => None,
        mo_diff::TreeStatus::LeftOnly | mo_diff::TreeStatus::RightOnly => Some(rgba(0x2f6fd033)),
        mo_diff::TreeStatus::Different => Some(rgba(0xb26a1a3d)),
    }
}

/// 把一次树比较的结果拆成**两页各自的**行状态表（按绝对路径查）。
///
/// 关键在「只染有变化的那一侧」：仅左有的条目右栏根本没有这一行，染右栏就是
/// 凭空造一条不存在的记录；两侧都有但内容不同的，那才两侧都染。这正是「双栏
/// 对比」跟「出一份差异报告」的根本区别——报告是中立的一列，双栏是两侧各自的视角。
///
/// 只取**直接子项**：更深层的差异归到它所在的那个子目录上（`compare_trees` 对
/// 「内部有差异」的目录给的就是目录自己那一条）。
pub(crate) fn compare_maps(
    cmp: &mo_diff::TreeComparison,
    // ⚠️ 写全 `std::path::Path`：`use gpui_kit::*` 里也有个（带泛型的）`Path`，
    // 裸写会解析到它，报错是「missing generics」而不是「找不到类型」，很误导。
    left: &std::path::Path,
    right: &std::path::Path,
) -> (
    HashMap<PathBuf, mo_diff::TreeStatus>,
    HashMap<PathBuf, mo_diff::TreeStatus>,
) {
    let mut l = HashMap::new();
    let mut r = HashMap::new();
    for e in &cmp.entries {
        if e.rel.components().count() != 1 {
            continue;
        }
        match e.status {
            mo_diff::TreeStatus::Identical => {}
            mo_diff::TreeStatus::LeftOnly => {
                l.insert(left.join(&e.rel), e.status);
            }
            mo_diff::TreeStatus::RightOnly => {
                r.insert(right.join(&e.rel), e.status);
            }
            mo_diff::TreeStatus::Different => {
                l.insert(left.join(&e.rel), e.status);
                r.insert(right.join(&e.rel), e.status);
            }
        }
    }
    (l, r)
}

/// 定位结果 → 当前视图滚动要的行号。
///
/// 列表 / 列视图用 `hit.row`（**列表行**：分组开启时含组头行，与它们的
/// `uniform_list` 同空间）；网格 / 画廊的 `uniform_list` 一行放 `cols` 个单元，
/// 而 `hit.pos` 是条目位，除列数才是它的行（列数由渲染路径写回 `Panel.grid_cols`）。
fn located_row(panel: &Panel, hit: mo_app::LocateHit) -> usize {
    match panel.view_mode {
        ViewMode::Grid | ViewMode::Gallery => hit.pos / panel.grid_cols.max(1),
        _ => hit.row,
    }
}

/// 定位缓冲是不是「同一个字符的重复」（`"aa"` / `"aaa"`…）。
///
/// 用来实现 Finder / 资源管理器的习惯：**连按同一个字母跳到下一个匹配项**。
/// 只认长度为 2 及以上、且字符全同的缓冲——首字符（`"a"`）仍从第一个匹配项开始，
/// 输入 `"ab"` 这种正常前缀也不受影响。
fn type_ahead_repeats_one_char(buf: &str) -> bool {
    match buf.chars().next() {
        Some(c) => buf.chars().count() >= 2 && buf.chars().all(|x| x == c),
        None => false,
    }
}

/// 输入即定位（type-ahead）的「定位 + 自动重置」逻辑，字符输入与退格共用。
///
/// 给定当前前缀 `prefix`：
/// 1. 选中匹配项（`locate_by_prefix` 已做大小写不敏感匹配 + 前缀优先、子序列兜底），
///    并把它滚进可视区（`Nearest`：本来就看得见就不滚，避免无谓跳动）；
///    `skip_current` 为真时从**当前焦点之后**找（连按同字母跳下一个）；
/// 2. 之后若 `TYPE_AHEAD_RESET_MS` 内没有新输入，清空定位缓冲（选择保留——
///    定位只是临时跳选，不该因为缓冲过期就把文件取消选中）。
///
/// `gen` 是调用方在更新缓冲时自增的「代数」：每次新输入都会让更早的定时器
/// 失效，从而避免后一次输入被前一次定时器误清空。
fn run_type_ahead(
    app: AppState,
    this: Entity<RootView>,
    prefix: String,
    skip_current: bool,
    gen: u64,
    cx: &mut App,
) {
    // 定位与自动重置是两个独立任务，各持一份 `this` 克隆，互不影响。
    let this_reset = this.clone();
    cx.spawn(async move |cx| {
        if let Some(hit) = app.locate_by_prefix(&prefix, skip_current).await {
            this.update(cx, |v, cx| {
                let row = located_row(v.panel(), hit);
                v.panel_mut()
                    .scroll
                    .scroll_to_item(row, gpui_kit::ScrollStrategy::Nearest);
                cx.notify();
            });
            // app 侧选择是唯一事实来源：跳选后回灌 UI 高亮。
            pull_selection(&app, &this, cx).await;
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(TYPE_AHEAD_RESET_MS))
            .await;
        this_reset.update(cx, |v, cx| {
            let p = v.panel_mut();
            // 仅当期间没有新输入（代数未变）才清空，避免误清后一次输入。
            if p.type_ahead_gen == gen {
                p.query.clear();
                cx.notify();
            }
        });
    })
    .detach();
}

/// 输入即定位缓冲的自动重置时长（毫秒）：静默超过此时长即清空前缀缓冲。
/// 「几百毫秒」级别——足够连续敲完一个词，又不会让旧缓冲长时间滞留。
const TYPE_AHEAD_RESET_MS: u64 = 400;

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
    // 目录判据问**列表模型**（`EntryKind`），不问本地磁盘：远程条目（`/1`）在本机
    // 不存在，`Path::is_dir()` 会把远程目录判成文件、交给系统 `open`（用户报的
    // `The file /1 does not exist.`）。列表里查不到（路径不在当前目录）才回落本机判断。
    if app.entry_is_dir(&p).await.unwrap_or_else(|| p.is_dir()) {
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

// ---------- 模态卡片渲染 ----------

impl RootView {
    /// 命令面板。macOS 的「选择其他应用…」借这层壳（列表换成应用），
    /// 所以返回类型统一成 `AnyElement`。
    fn render_command_palette(&self, entity: &Entity<RootView>) -> AnyElement {
        if let Some(picker) = &self.app_picker {
            return self.render_app_picker(entity, picker);
        }
        let list = filtered_commands_in(&self.cmd_query, &self.user_commands, &self.workflows);
        let idx = self.palette_index;
        let mut body = div().flex().flex_col();
        body = body.child(dialog_search_row(&self.cmd_query, "输入以过滤命令…"));
        let mut rows = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(340.0));
        for (i, id) in list.iter().enumerate() {
            let def = commands_in(&self.user_commands, &self.workflows)
                .into_iter()
                .find(|c| c.id == *id)
                .unwrap();
            let selected = i == idx;
            // 分类放右侧胶囊：左缘是对齐的命令名，扫读时不必先跳过一串分类词。
            rows = rows.child(
                picker_row(&format!("cmd-row-{i}"), selected, &def.title)
                    .child(tag_pill(&def.category, selected)),
            );
        }
        if list.is_empty() {
            rows = rows.child(dialog_empty_row("无匹配命令"));
        }
        body = body.child(rows);
        dialog_overlay_sized(
            entity,
            "命令面板",
            "",
            body,
            "↑↓ 选择 · Enter 执行 · Esc 关闭（⌘⇧P 打开）",
            px(600.0),
        )
        .into_any_element()
    }

    /// 「选择其他应用…」的选择器：与命令面板同一层壳，只是列表是应用。
    ///
    /// 系统推荐的（LaunchServices 候选）排在已装应用前面，所以多数时候
    /// 想要的那个就在头几行——输入名字可以进一步过滤。
    fn render_app_picker(&self, entity: &Entity<RootView>, picker: &AppPicker) -> AnyElement {
        let list = filtered_apps(&self.cmd_query, &picker.apps);
        let idx = self.palette_index;
        let mut body = div().flex().flex_col();
        body = body.child(dialog_search_row(&self.cmd_query, "输入以过滤应用…"));
        let mut rows = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(340.0));
        for (i, a) in list.iter().enumerate() {
            let selected = i == idx;
            rows = rows.child(picker_row(&format!("app-row-{i}"), selected, &a.name));
        }
        if list.is_empty() {
            // 列表还没加载完（异步扫目录）与「真的没有」要分开说，否则看着像坏了。
            let msg = if picker.apps.is_empty() {
                "正在查找应用…"
            } else {
                "无匹配应用"
            };
            rows = rows.child(dialog_empty_row(msg));
        }
        body = body.child(rows);
        dialog_overlay_sized(
            entity,
            "打开方式",
            "",
            body,
            "↑↓ 选择 · Enter 打开 · Esc 取消",
            px(600.0),
        )
        .into_any_element()
    }

    fn render_global_search(&self, _entity: &Entity<RootView>) -> Div {
        let idx = self.palette_index;
        // 吃满中央区剩余高度（原先写死 360px 是为了配合 640px 卡片）。
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(2.0))
            .overflow_y_scrollbar();
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
                    theme::selected_bg()
                } else {
                    theme::surface()
                })
                .text_color(if selected {
                    theme::selected_text()
                } else {
                    theme::text()
                })
                .child(text!(hit.name.clone()))
                .child(text!(format!("{}", hit.path.display())));
            body = body.child(row);
        }
        if self.search_results.is_empty() {
            // 「索引还没建起来」与「搜了但没匹配」是两件事，提示必须分开：前者用户
            // 什么都没做错，只是得等一等，写成「输入关键词…」会让人以为搜索坏了。
            let hint = if self.indexed == 0 {
                "索引正在建立（首次启动要爬一遍主目录），稍后再试".to_string()
            } else {
                "输入关键词搜索整个文件系统".to_string()
            };
            body = body.child(text!(hint));
        }
        central_view(
            &format!("全局搜索（已索引 {} 项）", self.indexed),
            &format!("🔍 {}", self.search_query),
            body,
            "↑↓ 选择 · Enter 打开 · Esc 关闭",
        )
    }

    /// 内容搜索（grep）面板。
    ///
    /// 结构照 `render_global_search`：四枚开关 + 一行统计 + 扁平的命中行列表。
    /// 与那一头最大的区别是**不实时搜**（读字节太贵），只有 Enter 才跑。
    fn render_content_search(&self, entity: &Entity<RootView>) -> Div {
        // 四枚开关胶囊：点一下切一个，改了就置 dirty（Enter 变回「搜索」）。
        let mut opts_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .py(px(6.0));
        for i in 0..4 {
            let on = self.content_opts.get(i);
            let btn = div()
                .id(format!("content-opt-{i}"))
                .px(px(7.0))
                .py(px(1.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(if on {
                    theme::accent()
                } else {
                    theme::separator()
                })
                .bg(if on {
                    theme::selected_bg()
                } else {
                    theme::surface()
                })
                .text_size(px(11.0))
                .text_color(if on {
                    theme::selected_text()
                } else {
                    theme::muted()
                })
                .child(text!(format!(
                    "{} {}",
                    ContentOptions::label(i),
                    ContentOptions::hint(i)
                )))
                .debug_selector({
                    let s = format!("mo-content-opt-{i}");
                    move || s.clone()
                });
            let target = entity.clone();
            let mut btn = btn;
            btn.interactivity().on_click(move |_, _window, cx| {
                target.update(cx, |v, cx| v.toggle_content_opt(i, cx));
            });
            opts_row = opts_row.child(btn.test_support());
        }
        // 搜索按钮：跑着的时候置灰（点也不会再起一轮）。
        let go = div()
            .id("content-go")
            .px(px(9.0))
            .py(px(1.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme::separator())
            .text_size(px(11.0))
            .text_color(if self.content_busy {
                theme::muted()
            } else {
                theme::text()
            })
            .child(text!(if self.content_busy {
                "搜索中…".to_string()
            } else if self.content_dirty {
                "搜索".to_string()
            } else {
                "重新搜索".to_string()
            }))
            .debug_selector(|| "mo-content-go".to_string());
        let go_target = entity.clone();
        let mut go = go;
        go.interactivity().on_click(move |_, _window, cx| {
            go_target.update(cx, |v, cx| v.run_content_search(cx));
        });
        opts_row = opts_row.child(go.test_support());

        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(2.0))
            .child(opts_row)
            .overflow_y_scrollbar()
            .debug_selector(|| "mo-content-body".to_string());

        match (&self.content_report, self.content_busy) {
            // 第一轮的「正在搜索」：还没有任何旧结果可看。
            (&None, true) => {
                body = body.child(
                    div()
                        .px(px(8.0))
                        .text_color(theme::muted())
                        .child(text!("正在搜索…（Esc 可中断）".to_string())),
                );
            }
            (Some(r), _) => {
                body = body.child(
                    div()
                        .px(px(8.0))
                        .text_size(px(11.0))
                        .text_color(theme::muted())
                        .child(text!(format!(
                            "扫了 {} 个文件 · {} 个文件命中 · {} 处 · 跳过 {} 个二进制{}",
                            r.scanned,
                            r.files_matched,
                            r.hits,
                            r.skipped_binary,
                            if r.truncated { " · 已达上限" } else { "" }
                        ))),
                );
                if r.files.is_empty() {
                    body = body.child(
                        div()
                            .px(px(8.0))
                            .text_color(theme::muted())
                            .child(text!("没有找到匹配的内容".to_string())),
                    );
                }
                // 扁平命中行：序号即 `content_index`，与 ↑↓ 走的是同一条序列。
                let hits = self.content_hits();
                for (row_i, (fi, li)) in hits.iter().enumerate() {
                    let Some(file) = r.files.get(*fi) else {
                        continue;
                    };
                    let Some(line) = file.lines.get(*li) else {
                        continue;
                    };
                    let selected = row_i == self.content_index;
                    // 同一文件连续多行只在第一行标文件名，其余留白对齐——
                    // 每行都重复一次文件名，扫读时反而看不出行在变。
                    let first_of_file = row_i == 0 || hits[row_i - 1].0 != *fi;
                    let row = div()
                        .id(format!("content-row-{row_i}"))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(8.0))
                        .py(px(2.0))
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
                        .child(
                            div()
                                .w(px(150.0))
                                .flex_shrink_0()
                                .overflow_hidden()
                                .text_size(px(11.0))
                                .text_color(if selected {
                                    theme::selected_text()
                                } else {
                                    theme::muted()
                                })
                                .child(text!(if first_of_file {
                                    file.name.clone()
                                } else {
                                    String::new()
                                })),
                        )
                        .child(
                            div()
                                .w(px(38.0))
                                .flex_shrink_0()
                                .text_size(px(11.0))
                                .text_color(if selected {
                                    theme::selected_text()
                                } else {
                                    theme::muted()
                                })
                                .child(text!(format!("{}", line.line))),
                        )
                        .child(highlighted_line(&line.text, &line.spans, selected))
                        .debug_selector({
                            let s = format!("mo-content-row-{row_i}");
                            move || s.clone()
                        });
                    let target = entity.clone();
                    let idx = row_i;
                    let mut row = row;
                    row.interactivity().on_click(move |_, _window, cx| {
                        target.update(cx, |v, cx| {
                            v.content_index = idx;
                            v.open_content_hit(cx);
                        });
                    });
                    body = body.child(row.test_support());
                }
            }
            (None, false) => {
                body = body.child(
                    div()
                        .px(px(8.0))
                        .text_color(theme::muted())
                        .child(text!("输入要找的内容，回车搜索".to_string())),
                );
            }
        }

        let root = self
            .content_root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        central_view(
            "内容搜索",
            &format!("🔍 {} · 范围 {}", self.content_query, root),
            body,
            if self.content_dirty {
                "Enter 搜索 · ↑↓ 选择 · ⌥A/⌥R/⌥W/⌥C 切开关 · Esc 关闭"
            } else {
                "Enter 跳到命中 · ↑↓ 选择 · Esc 关闭"
            },
        )
    }

    fn render_trash(&self, entity: &Entity<RootView>) -> Div {
        // 表头列：与文件列表同一套列语言（宽度同 `ColId::default_width`，收口在
        // [`list_view::default_col_width`]）。时间列语义 = **删除时间**（账本
        // `at`；账本里没有文件修改时间，别把两者混称）。
        let cols = [
            list_view::Column::flex("name", "名称"),
            list_view::Column::fixed_right(
                "date",
                "删除时间",
                list_view::default_col_width(SortKey::Modified),
            ),
            list_view::Column::fixed_right(
                "size",
                "大小",
                list_view::default_col_width(SortKey::Size),
            ),
            list_view::Column::fixed_right(
                "kind",
                "种类",
                list_view::default_col_width(SortKey::Kind),
            ),
        ];
        let grid_mode = matches!(self.trash_view_mode, ViewMode::Grid | ViewMode::Gallery);
        let slot = crate::listing::icon_slot(self.trash_view_mode);
        // 网格 / 画廊格子的几何：wrap 流式布局，格宽定长、名字截断（与 grid.rs
        // 的 cell 同构，只是不虚拟化——回收站条目量级小，全量渲染足够）。
        let cell_w = if self.trash_view_mode == ViewMode::Gallery {
            152.0
        } else {
            112.0
        };
        let cell_h = if self.trash_view_mode == ViewMode::Gallery {
            140.0
        } else {
            100.0
        };

        // 滚动区。**空白点击清选**：行 / 格内按下会 `stop_propagation`，能落到
        // 容器这里的一定是空白（占位行 / 列表下方 / 空态）——与文件列表同语义
        // （用户报：回收站点空白不会取消选中）。
        let entity_blank = entity.clone();
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .px(px(12.0))
            // 表头与首行之间留 6px 呼吸（用户要求）；滚到底时末行下方同样有呼吸位。
            .py(px(6.0))
            .bg(theme::surface())
            .overflow_y_scrollbar();
        body.interactivity()
            .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                entity_blank.update(cx, |v, cx| {
                    if !v.trash_selected.is_empty() || v.trash_anchor.is_some() {
                        v.trash_selected.clear();
                        v.trash_anchor = None;
                        cx.notify();
                    }
                });
            });

        // 系统图标链路与文件列表**同一条**（`AppState::file_icon` 纯查表 + 后台泵
        // 补齐）。类型图标按扩展名向系统要（`iconForFileType:`）——回收站条目的
        // 原路径多半已不存在，按路径问只会得到一张通用白纸图标。headless 测试里
        // 平台层不可用，走内置 Lucide 字形兜底——两条路径都要能画。
        let app = self.app();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = now;

        // 网格 / 画廊：格子先进 wrap 容器，再整体挂进滚动区。
        let mut flow = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .content_start()
            .gap(px(8.0));

        for (i, e) in self.trash_entries.iter().enumerate() {
            // 选中态以**多选集合**为准：⌘A / ⌘点击 / ⇧连选都画在集合上。
            // ⚠️ 不能用游标 `i == idx` 代替——那是 ⌘A「没反应」的根因：
            // 状态集合全选了，渲染却只高亮游标一行。
            let selected = self.trash_selected.contains(&i);
            let name = e
                .original
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| e.original.to_string_lossy().to_string());
            // 系统图标拿不到才退回内置 Lucide 字形：目录 FOLDER，文件按扩展名挑
            // （与 file_item 的兜底同源）。
            let glyph = if e.is_dir {
                crate::icons::FOLDER
            } else {
                crate::icons::file_ext_icon(&name)
            };
            let fg = if selected {
                theme::selected_text()
            } else {
                theme::text()
            };
            let icon_el: AnyElement =
                match crate::file_item::system_icon(Some(&app), &e.original, e.is_dir, slot)
                    .as_ref()
                    .and_then(crate::bitmap::image_source)
                {
                    Some(source) => img(source).w(px(slot)).h(px(slot)).into_any_element(),
                    None => crate::icons::icon(glyph, slot, fg).into_any_element(),
                };

            // 点击 / 双击的选中与打开语义在两种视图下完全一致（提取成闭包对）。
            // ⚠️ 必须有元素 ID：无 ID 的裸 div 拿不到 element_state，on_click 永远不触发。
            let entity_click = entity.clone();
            let trashed = e.trashed.clone();
            let is_dir = e.is_dir;
            let on_row_click =
                move |ev: &gpui_kit::ClickEvent, _window: &mut Window, cx: &mut App| {
                    if ev.click_count() >= 2 {
                        entity_click
                            .update(cx, |v, cx| v.open_trash_entry(trashed.clone(), is_dir, cx));
                        return;
                    }
                    // 修饰键决定选择语义（与文件列表 / Finder 一致）：无修饰 = 单选替换；
                    // cmd/ctrl = 切换多选；shift = 从锚点连选。
                    let mods = ev.modifiers();
                    let multi = mods.platform || mods.control;
                    let shift = mods.shift;
                    entity_click.update(cx, |v, cx| {
                        v.palette_index = i;
                        if shift {
                            v.trash_select_range(i);
                        } else if multi {
                            if !v.trash_selected.remove(&i) {
                                v.trash_selected.insert(i);
                            }
                            v.trash_anchor = Some(i);
                        } else {
                            v.trash_selected.clear();
                            v.trash_selected.insert(i);
                            v.trash_anchor = Some(i);
                        }
                        cx.notify();
                    });
                };
            // 行内 / 格内按下就地吃掉：别让「点条目」冒泡成「点空白」清选
            // （见模块级 `swallow_trash_press`）。

            if grid_mode {
                let mut cell = div()
                    .id(format!("trash-cell-{i}"))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.0))
                    .w(px(cell_w))
                    .h(px(cell_h))
                    .p(px(4.0))
                    .rounded(px(6.0))
                    .bg(list_view::row_background(selected, i, false))
                    .text_size(px(11.0))
                    .debug_selector(move || format!("mo-trash-cell-{i}"));
                if !selected {
                    cell = cell.hover(|s| s.bg(theme::hover_bg()));
                }
                cell = cell.child(icon_el).child(
                    div()
                        .max_w_full()
                        .truncate()
                        .text_color(fg)
                        .child(text!(name.clone())),
                );
                cell.interactivity()
                    .on_mouse_down(MouseButton::Left, swallow_trash_press);
                cell.interactivity().on_click(on_row_click);
                // `.test_support()` 最后包（挂完事件再包，见行渲染的同一条注释）。
                flow = flow.child(cell.test_support());
            } else {
                let mut row = div()
                    .id(format!("trash-row-{i}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .h(px(list_view::ROW_H))
                    .px(px(4.0))
                    .text_size(px(13.0))
                    // 与文件列表一致：选中蓝底；未选中奇偶斑马纹 + 悬停。
                    .bg(list_view::row_background(selected, i, true))
                    .debug_selector(move || format!("mo-trash-row-{i}"));
                if !selected {
                    row = row.hover(|s| s.bg(theme::hover_bg()));
                }
                // 名称列（弹性）：图标 + 文件名。
                row = row.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .flex_1()
                        .min_w_0()
                        .child(icon_el)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(fg)
                                .child(text!(name.clone())),
                        ),
                );
                // 三个右对齐固定列：删除时间 / 大小 / 种类——宽度与表头同源
                // （同一 `cols`），cell 之间**没有 gap**（表头也没有，多了会错位）。
                let date = crate::file_item::format_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(e.at),
                );
                let size_text = if e.is_dir {
                    "—".to_string()
                } else {
                    self.trash_sizes
                        .get(&e.trashed)
                        .map(|s| crate::file_item::format_size(*s))
                        .unwrap_or_else(|| "…".to_string())
                };
                let kind = crate::file_item::trash_kind_label(e.is_dir, &name);
                let fg2 = if selected {
                    theme::selected_text()
                } else {
                    theme::muted()
                };
                for (ci, c) in cols.iter().skip(1).enumerate() {
                    let t = match c.key {
                        "date" => date.clone(),
                        "size" => size_text.clone(),
                        _ => kind.clone(),
                    };
                    row = row.child(
                        div()
                            .flex()
                            .justify_end()
                            .w(px(c.width))
                            .flex_shrink_0()
                            .min_w_0()
                            .text_color(fg2)
                            // 显式 id：`text!` 的默认 id 是宏调用点哈希，同一行
                            // 三列共享它 → a11y NodeId 撞车（见 list_view::header_row 注释）。
                            .child(text!(id = format!("trash-meta-{i}-{ci}"), t)),
                    );
                }
                row.interactivity()
                    .on_mouse_down(MouseButton::Left, swallow_trash_press);
                row.interactivity().on_click(on_row_click);
                // `.test_support()` 必须**最后**包（挂完 hover / 事件再包，与 sidebar /
                // status_bar 同款）：过早包会把后续的 hover / child 挂到 Observed 包装
                // 上，headless 的 click / double_click 通道点不中行（实测踩到）。
                body = body.child(row.test_support());
            }
        }
        if grid_mode {
            body = body.child(flow);
        }
        // 「斑马纹铺满一屏」（用户报：回收站只有两条记录时下面一片白，不像同一个
        // 应用）：真实行数不够一屏时，底下补**只有底色、没有任何内容**的占位行。
        // 底色规则收口在 [`list_view::row_background`]，与数据行逐字一致。行 ID
        // 不能省：多条占位行共享无 ID 路径会撞 a11y NodeId。网格 / 画廊是 wrap
        // 格子，空白就是表面色，不补占位。
        if !grid_mode {
            let fill = self.trash_fill_rows();
            for i in self.trash_entries.len()..fill {
                body = body.child(list_view::fill_row(
                    format!("trash-ph-{i}"),
                    format!("mo-trash-ph-{i}"),
                    i,
                ));
            }
        }
        // 空态**不**再画「回收站是空的」一行（用户报）：标题「回收站（0 项）」已
        // 说明状态，且这行字挂在补足行下面，看起来像又一处渲染错位。
        // 表头固定在滚动区上方，不随内容滚动（Finder 列表视图行为）；网格 / 画廊
        // 没有列的概念，不出表头。
        let mut content = div().flex().flex_col().flex_1().min_h_0().min_w_0();
        if !grid_mode {
            content = content.child(list_view::header_row("trash", &cols));
        }
        // prepaint 回写内容区高度，下一帧才能把斑马纹补满一屏（首帧先按实际
        // 行数渲染；高度变了才 notify，不会造成重绘循环）。
        // ⚠️ 量的是**视口**（这一层包裹），不能把 on_prepaint 挂在 `overflow_y_scrollbar`
        // 的 body 上：那个调用会把 body 重构进「叠加滚动条」包装，挂在它上面的监听
        // 实际量到的是**内容层**高度——补足行让它一帧比一帧高，下一帧又按这个高度
        // 补更多行，正反馈滚出几百行空白（用户报，Windows 实测 300 帧涨到 622 行）。
        // 与文件列表的 `list_origin` 同一套规矩：量视口，不量内容。
        let entity_h = entity.clone();
        let viewport = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .on_prepaint(move |bounds, _window, cx| {
                let h = f32::from(bounds.size.height);
                entity_h.update(cx, |v, cx| {
                    if v.set_trash_body_h(h) {
                        cx.notify();
                    }
                });
            })
            .child(body);
        content = content.child(viewport);

        // 「还原」按钮：只在**有选中**时出现（没有可还的就不占地方）。
        // Enter 不再还原——macOS 上它是重命名、Win/Linux 上是打开（平台惯例），
        // 还原这个高频动作给一枚看得见摸得着的按钮。
        // 「还原」按钮挂在**标题栏右侧的常驻槽位**里，有选中才画出按钮本体。
        // ⚠️ 槽位必须常驻（定高），按钮不能占列表的流内空间——首次点击选中会让
        // 按钮凭空出现、列表整体下移，双击的第二下就落在别处了（实测踩到）。
        let restore_n = self.trash_selected.len();
        let entity_empty = entity.clone();
        let mut action = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(24.0))
            .children((restore_n > 0).then(|| {
                let entity_restore = entity.clone();
                let label = if restore_n == 1 {
                    "还原".to_string()
                } else {
                    format!("还原 {restore_n} 项")
                };
                div()
                    .id("trash-restore-btn")
                    .test_support()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(12.0))
                    .h_full()
                    .rounded(px(6.0))
                    .bg(theme::selected_bg())
                    .text_size(px(12.0))
                    .text_color(theme::selected_text())
                    // 测试用（release no-op）：headless 点这颗按钮还原选中。
                    .debug_selector(|| "mo-trash-restore".to_string())
                    .on_click(move |_, _window, cx| on_trash_restore(&entity_restore, cx))
                    .child(text!(label))
            }));
        // 「清空回收站」：回收站还有东西才出现；点了先弹确认卡（二次确认），
        // 真正执行在确认卡上。
        if !self.trash_entries.is_empty() {
            action = action.child(
                div()
                    .id("trash-empty-btn")
                    .test_support()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(12.0))
                    .h_full()
                    .rounded(px(6.0))
                    .bg(theme::hover_bg())
                    .text_size(px(12.0))
                    .text_color(theme::text())
                    // 测试用（release no-op）：headless 点这颗按钮打开清空确认卡。
                    .debug_selector(|| "mo-trash-empty".to_string())
                    .on_click(move |_, _window, cx| on_trash_empty(&entity_empty, cx))
                    .child(text!("清空回收站")),
            );
        }
        // 紧凑版中央视图：标题栏与表头之间、表头与首行之间不留内衬（用户报的
        // 两段间距），观感对齐文件列表（表头顶格、行紧随表头）。
        central_view_with_action_compact(
            &format!("回收站（{} 项）", self.trash_entries.len()),
            "",
            content,
            "",
            action.into_any_element(),
        )
    }
    /// 比较 / diff 模态：文件 → 行级 diff；文件夹 → 树比较清单。
    fn render_diff(&self) -> Div {
        let Some(c) = &self.diff_cache else {
            return central_view(
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
            .flex_1()
            .min_h_0()
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
                    .flex_1()
                    .min_h_0()
                    .gap(px(1.0))
                    .overflow_y_scrollbar();
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
        central_view("文件比较", "", body, "Esc / Space 关闭")
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
            .flex_1()
            .min_h_0()
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
            .flex_1()
            .min_h_0()
            .gap(px(1.0))
            .overflow_y_scrollbar();
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
        central_view("文件夹比较", "", body, "Esc / Space 关闭")
    }
}

/// **中央区视图**：占据整个中央内容区的次级视图（**不是**对话框）。
///
/// 用于内容多、需要停留与滚动的场景——文件 / 文件夹比较、重复文件、磁盘占用、
/// 回收站、全局搜索、同步、工作流、批量重命名、快捷键、扩展。顶部一条标题栏
/// （标题 + 可选副标题 + 右侧快捷键提示），下方内容区铺满，由各视图自己滚动。
///
/// 反过来，短小的输入 / 确认类弹窗请用 [`dialog_overlay`]——它们不该把浏览区顶掉。
pub(crate) fn central_view(title: &str, input: &str, body: impl IntoElement, hint: &str) -> Div {
    central_view_with_action(title, input, body, hint, div())
}

/// [`central_view`] 的完整版：标题栏右侧多一个**常驻**动作槽位（回收站的
/// 「还原」按钮用）。槽位本身在不在、多高，直接决定标题栏高度——调用方要保证
/// 按钮出现 / 消失时标题栏高度不变（回收站的做法是槽位恒在、按钮可选）。
pub(crate) fn central_view_with_action(
    title: &str,
    input: &str,
    body: impl IntoElement,
    hint: &str,
    action: impl IntoElement,
) -> Div {
    central_view_inner(title, input, body, hint, action, true)
}

/// 紧凑版：body 外壳**零内衬**——回收站这类「表头 + 满幅列表」视图用：
/// 表头**紧贴**标题栏（用户明确要求这里不要缝），表头与首行之间的呼吸位由
/// body 自己的 padding 负责。文档式视图（diff / 搜索 / 重复文件…）
/// 仍走四周带内衬的 [`central_view_with_action`]。
pub(crate) fn central_view_with_action_compact(
    title: &str,
    input: &str,
    body: impl IntoElement,
    hint: &str,
    action: impl IntoElement,
) -> Div {
    central_view_inner(title, input, body, hint, action, false)
}

fn central_view_inner(
    title: &str,
    input: &str,
    body: impl IntoElement,
    hint: &str,
    action: impl IntoElement,
    pad_body: bool,
) -> Div {
    let action = action.into_any_element();
    let mut head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(div().text_size(px(14.0)).child(text!(title.to_string())));
    if !input.is_empty() {
        head = head.child(
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child(text!(input.to_string())),
        );
    }
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .bg(theme::surface())
        .text_color(theme::text())
        // 测试用（release no-op）：断言「回收站 / 全局搜索」这类次级视图确实占住了中央区。
        .debug_selector(|| "mo-central-view".to_string())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .px(px(14.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(theme::separator())
                .bg(theme::container())
                .child(head)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .justify_end()
                        .gap(px(10.0))
                        .child(action)
                        .text_size(px(12.0))
                        .text_color(theme::muted())
                        .child(text!(hint.to_string())),
                ),
        )
        .child({
            let mut shell = div().flex().flex_col().flex_1().min_h_0();
            if pad_body {
                // 文档式视图：四周 8px 内衬。
                shell = shell.p(px(8.0));
            }
            // 紧凑版（回收站）：零内衬——表头**紧贴**标题栏（用户明确要求这里
            // 不要缝），表头与首行之间的呼吸位由 trash body 自己的 padding 给。
            shell.child(body)
        })
}

/// 浮层卡片及其标题栏共用的圆角半径。
///
/// ⚠️ 两处必须一致：卡片是 `rounded(R)`，而标题栏自带底色（`theme::container()`），
/// 是**后画的矩形**——gpui 不会把子元素裁进父级圆角（这个 fork 的 `div` 只处理
/// `Overflow::Scroll`，`overflow_hidden()` 并不裁），所以标题栏必须自己
/// `rounded_t(R)`，否则会把卡片上面两个角盖成直角（下面两个角没有子元素覆盖，
/// 看着是圆的——就是「缺了 2 个圆角」那个现象）。
const DIALOG_RADIUS: f32 = 12.0;

/// **带遮罩的浮层对话框**：半透明遮罩铺满视口 + 居中卡片。
///
/// 所有短小的弹窗都走这里——属性、压缩、标签、统一设置、命令面板、
/// 连接到服务器，以及 [`Modal::Info`](Modal) 的信息提示。下层内容照常渲染、
/// 透过遮罩可见，但被 `.occlude()` 挡住点不到。点遮罩空白 / Esc 关闭
/// （按键路由见 `handle_modal_key`）。
///
/// `title` 为空 = 不画标题栏（纯正文提示，如信息提示），避免留一条空条。
/// 卡片留白由本函数统一负责，`body` 不要再自带 `p(...)`。
pub(crate) fn dialog_overlay(
    entity: &Entity<RootView>,
    title: &str,
    input: &str,
    body: impl IntoElement,
    hint: &str,
) -> impl IntoElement {
    dialog_overlay_sized(entity, title, input, body, hint, px(480.0))
}

/// [`dialog_overlay`] 的定宽版：命令面板 / 设置这类「列表 + 标签页」的浮层
/// 需要比 480px 更宽的卡片，窄卡片会把行里的标题与右侧胶囊挤成两行。
pub(crate) fn dialog_overlay_sized(
    entity: &Entity<RootView>,
    title: &str,
    input: &str,
    body: impl IntoElement,
    hint: &str,
    width: gpui_kit::Pixels,
) -> impl IntoElement {
    let mut card = div()
        .id("dialog-card")
        .w(width)
        .flex()
        .flex_col()
        .rounded(px(DIALOG_RADIUS))
        .border_1()
        // 深色底上阴影几乎不可见，靠一圈描边把卡片与遮罩分开。
        .border_color(theme::separator())
        .bg(theme::surface())
        .text_color(theme::text())
        .shadow_lg();
    if !title.is_empty() || !input.is_empty() {
        let mut head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(div().text_size(px(14.0)).child(text!(title.to_string())));
        if !input.is_empty() {
            head = head.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::muted())
                    .child(text!(input.to_string())),
            );
        }
        card = card.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .px(px(16.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(theme::separator())
                .bg(theme::container())
                // 顶部跟随卡片圆角（见 `DIALOG_RADIUS` 的说明）。
                .rounded_t(px(DIALOG_RADIUS))
                .child(head),
        );
    }
    let mut card = card.child(div().p(px(14.0)).child(body));
    if !hint.is_empty() {
        card = card.child(
            div()
                .px(px(16.0))
                .pb(px(12.0))
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child(text!(hint.to_string())),
        );
    }

    let card = card
        // 测试用（release no-op）：断言对话框卡片确实画了出来。
        .debug_selector(|| "mo-dialog-card".to_string())
        // 点卡片本身不冒泡到遮罩，否则点内容 / 按钮会误关。
        .on_click(|_, _window, cx| cx.stop_propagation());

    let backdrop = entity.clone();
    div()
        .id("dialog-backdrop")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_kit::rgb(0x000000).alpha(0.35))
        .occlude()
        // 测试用（release no-op）：断言遮罩铺满整个视口、且关闭后消失。
        .debug_selector(|| "mo-dialog-overlay".to_string())
        .on_click(move |_, _window, cx| dismiss_modal(&backdrop, cx))
        .child(card)
}

/// 小圆角胶囊：设置行的「当前值」与命令面板的「分类」右侧徽章共用一套视觉。
///
/// 选中行（实心强调色底）上换反色（底=正文白、字=强调蓝）——胶囊若仍是
/// `hover_bg` 会整块融进蓝底，值就看不见了。
fn tag_pill(label: &str, selected: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(18.0))
        .px(px(7.0))
        .rounded(px(5.0))
        .bg(if selected {
            theme::selected_text()
        } else {
            theme::hover_bg()
        })
        .text_size(px(11.0))
        .text_color(if selected {
            theme::selected_bg()
        } else {
            theme::muted()
        })
        .child(text!(label.to_string()))
}

/// 对话框里的整行可选项（设置三页的行、命令面板的行同一副骨架）：
/// 固定行高 + 圆角，选中=实心强调底，正文左标题。右侧内容调用方 `.child()` 追加。
/// 不带 hover——可点的行（设置）自己按 `!selected` 条件加，纯键盘的面板行加了是误导。
fn picker_row(id: &str, selected: bool, title: &str) -> Stateful<Div> {
    div()
        .id(id.to_string())
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .h(px(34.0))
        .rounded(px(8.0))
        .text_size(px(13.0))
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
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(text!(title.to_string())),
        )
}

/// 面板 / 选择器顶部的搜索行：放大镜图标 + 当前过滤词（空则给占位提示）。
///
/// 不用 emoji（🔍）：Windows 上 emoji 走系统彩色字体，字号、基线、配色都不跟主题。
/// 真正的输入仍走全局键位路径（这里只是回显）。
fn dialog_search_row(query: &str, placeholder: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .h(px(34.0))
        .mb(px(8.0))
        .rounded(px(8.0))
        .bg(theme::container())
        .child(crate::icons::icon(
            crate::icons::SEARCH,
            14.0,
            theme::muted(),
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(13.5))
                .text_color(if query.is_empty() {
                    theme::muted()
                } else {
                    theme::text()
                })
                .child(text!(if query.is_empty() {
                    placeholder.to_string()
                } else {
                    query.to_string()
                })),
        )
}

/// 面板空态：居中一行灰字（此前是贴边的裸文本，看着像渲染坏了）。
fn dialog_empty_row(msg: &str) -> Div {
    div()
        .flex()
        .justify_center()
        .py(px(28.0))
        .text_size(px(12.0))
        .text_color(theme::muted())
        .child(text!(msg.to_string()))
}

/// 关闭当前对话框，收尾与该模态的 **Esc 保持一致**。
///
/// 点遮罩空白与按 Esc 必须走同一套语义：设置窗口停在外观页时主题是
/// 「↑↓ 实时预览」的，Esc 是 `theme_cancel`（还原预览）；若遮罩走了通用关闭，
/// 会让没提交的预览留在配置里。
fn dismiss_modal(entity: &Entity<RootView>, cx: &mut App) {
    if matches!(entity.read(cx).modal, Modal::Settings)
        && entity.read(cx).settings_tab == SettingsTab::Theme
    {
        entity.update(cx, |v, cx| v.theme_cancel(cx));
        return;
    }
    // 回收站确认卡：点遮罩 = 取消（回面板），不是关掉整个面板。
    if matches!(entity.read(cx).modal, Modal::ConfirmTrash(_)) {
        dismiss_trash_confirm(entity, cx);
        return;
    }
    close_modal(entity, cx);
}

/// 信息提示（`Modal::Info`）的浮层。
///
/// 与其它对话框**共用同一套外壳**（[`dialog_overlay`]：遮罩 + 居中卡片 +
/// 点空白 / Esc 关闭），到这里已无第二份手写浮层。差异只有两点，都靠参数表达：
/// 无标题栏（纯提示没有标题）、底部一颗确认按钮。正文限高可滚动，长消息
/// （如哈希结果）不撑破窗口。按钮文字可经 `notice_ok` 自定义，默认「知道了」。
fn render_notice_overlay(msg: &str, ok_label: &str, entity: &Entity<RootView>) -> impl IntoElement {
    let ok_close = entity.clone();
    let body = div()
        .flex()
        .flex_col()
        .gap(px(18.0))
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
                    // 与点遮罩空白走同一套收尾（`close_modal` 会清掉 `notice_ok`）。
                    .on_click(move |_, _window, cx| close_modal(&ok_close, cx))
                    .child(text!(ok_label.to_string())),
            ),
        );
    dialog_overlay(entity, "", "", body, "")
}

/// 回收站危险操作确认卡（`Modal::ConfirmTrash`，目前只有「清空回收站」一档）。
///
/// 与 [`render_notice_overlay`] 共用 [`dialog_overlay`] 外壳；差别是两颗按钮：
/// 取消（中性）+ 确认（警示红 `rgba(0xd70015)`，项目没有危险色角色，用固定值）。
/// 回收站面板保留在遮罩后面，Esc / 取消 / 点遮罩都回到面板——**不能**走
/// [`close_modal`]，那会把面板整个关掉。
fn render_trash_confirm(
    v: &RootView,
    _action: &TrashConfirm,
    entity: &Entity<RootView>,
) -> impl IntoElement {
    let (title, message, ok_label) = (
        "清空回收站",
        format!(
            "将永久删除回收站里的全部 {} 项，此操作不可恢复。",
            v.trash_entries.len()
        ),
        "清空",
    );
    let cancel = entity.clone();
    let ok = entity.clone();
    let body = div()
        .flex()
        .flex_col()
        .gap(px(18.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(14.0))
                        .text_color(theme::text())
                        .child(text!(title.to_string())),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme::muted())
                        .child(text!(message)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(10.0))
                // 取消：中性按钮（描边），语义上把用户送回面板。
                .child(
                    div()
                        .id("trash-confirm-cancel")
                        .test_support()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(18.0))
                        .h(px(30.0))
                        .rounded(px(7.0))
                        .border_1()
                        .border_color(theme::muted())
                        .text_size(px(13.0))
                        .on_click(move |_, _window, cx| dismiss_trash_confirm(&cancel, cx))
                        .child(text!("取消")),
                )
                // 确认：警示红主按钮。
                .child(
                    div()
                        .id("trash-confirm-ok")
                        .test_support()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(18.0))
                        .h(px(30.0))
                        .rounded(px(7.0))
                        .bg(rgba(0xd70015))
                        .text_color(theme::selected_text())
                        .text_size(px(13.0))
                        .on_click(move |_, _window, cx| confirm_trash_action(&ok, cx))
                        .child(text!(ok_label.to_string())),
                ),
        );
    dialog_overlay(entity, "", "", body, "")
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
    let this = entity.clone();
    cx.spawn(async move |cx| {
        if !p.name.is_empty() && p.name != old_name {
            let to = p.path.with_file_name(&p.name);
            if let Err(e) = app.rename_many(vec![(p.path.clone(), to)]).await {
                this.update(cx, |v, cx| {
                    v.notice(format!("重命名失败：{e}"), None, cx);
                });
            }
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
    let this = entity.clone();
    // 闭包参数必须叫 `cx`：里面 `this.update(cx, ..)` 用的是**闭包自己的** cx，
    // 写成 `_cx` 会捕获到外层那个 `&mut App`，进而要求它活到 'static（E0521）。
    cx.spawn(async move |cx| {
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
        if let Err(e) = app.rename_many(pairs).await {
            this.update(cx, |v, cx| {
                v.notice(format!("重命名失败：{e}"), None, cx);
            });
        }
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
    use std::sync::Arc;

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{px, Context, TestAppContext};
    use mo_app::AppState;

    use super::{
        box_row_range, filtered_apps, located_row, type_ahead_repeats_one_char, ConnectAuthState,
        Modal, OperationHandle, RootView, SettingsTab,
    };
    use crate::panel::Panel;

    /// 橡皮筋 y → 行区间的折算：行带按顶边对齐（floor），与渲染出的行一一对应。
    ///
    /// 这组测试守的是「空白处单击选中最后一行」的根因：旧实现 round + clamp 双
    /// 叠加，最后一行的下半截折到行外、列表下方 12px 折成最后一行、界外硬钳边界。
    #[test]
    fn box_row_range_aligns_to_row_tops_and_never_clamps_out_of_band() {
        let top = 100.0;
        // 3 行：行带 = [112,136) [136,160) [160,184)（含 12px 顶部留白）。
        // 行内点击命中本行（上半截、下半截都要中——round 版下半截会脱靶）。
        assert_eq!(box_row_range(120.0, 120.0, top, 0.0, 3), Some((0, 0)));
        assert_eq!(box_row_range(133.0, 133.0, top, 0.0, 3), Some((0, 0)));
        assert_eq!(box_row_range(170.0, 170.0, top, 0.0, 3), Some((2, 2)));
        // 空白：首行上方与末行下方都**不命中任何行**，也不许钳到边界。
        assert_eq!(box_row_range(105.0, 105.0, top, 0.0, 3), None);
        assert_eq!(box_row_range(190.0, 190.0, top, 0.0, 3), None);
        // 拖拽跨界：从末行拖进下方空白 = 与行带求交，止于最后一行（Finder 同款）。
        assert_eq!(box_row_range(170.0, 300.0, top, 0.0, 3), Some((2, 2)));
        // 拖拽从空白起、扫过全部行 = 全选。
        assert_eq!(box_row_range(90.0, 300.0, top, 0.0, 3), Some((0, 2)));
        // 完全在行带之下拖出的框（起止都在界外）= 什么也不选。
        assert_eq!(box_row_range(200.0, 300.0, top, 0.0, 3), None);
        // 滚动参与折算：gpui 滚动偏移「向下滚为负」，滚过一行（offset.y = -24）
        // 后，同样的窗口 y 命中的行号 +1。
        assert_eq!(box_row_range(120.0, 120.0, top, -24.0, 3), Some((1, 1)));
        // 空目录：任何位置都取不到行。
        assert_eq!(box_row_range(120.0, 120.0, top, 0.0, 0), None);
    }

    /// 「选择其他应用…」的过滤：应用名大小写不敏感命中，空查询全给。
    ///
    /// 命令面板的过滤函数管不到这条——列表换成了应用，过滤逻辑是另一份
    /// （`filtered_apps`），得自己有人守。
    #[test]
    fn app_picker_filters_by_name_case_insensitively() {
        let apps = vec![
            mo_app::shell::OpenWithApp {
                name: "TextEdit".to_string(),
                progid: "/System/Applications/TextEdit.app".to_string(),
            },
            mo_app::shell::OpenWithApp {
                name: "预览".to_string(),
                progid: "/System/Applications/Preview.app".to_string(),
            },
        ];
        assert_eq!(filtered_apps("", &apps).len(), 2, "空查询应当全给");
        let hit = filtered_apps("text", &apps);
        assert_eq!(hit.len(), 1, "大小写不敏感地命中 TextEdit");
        assert_eq!(hit[0].name, "TextEdit");
        assert!(filtered_apps("vscode", &apps).is_empty());
    }

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

    /// 对话框（B 类）必须是**带遮罩的浮层**，而不是「替换中央区那种」：
    /// 打开后遮罩铺满整个视口、卡片画出来，下层浏览区照常渲染且位置不变；
    /// 关闭后遮罩消失。
    ///
    /// 覆盖「连接到服务器」与从替换中央区迁移过来的几个（布局 / 命令面板）——
    /// 它们原先会顶掉整个浏览区，与「连接到服务器」是同一类缺陷。
    #[test]
    fn dialogs_are_full_viewport_overlays() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|window, cx| window.render_frame(cx));
        let (vw, vh) = cx.update(|window, _cx| {
            let s = window.viewport_size();
            (s.width.to_f64() as f32, s.height.to_f64() as f32)
        });
        assert!(
            cx.debug_bounds("mo-dialog-overlay").is_none(),
            "没打开对话框时不该有遮罩"
        );
        let list_before = cx.debug_bounds("mo-file-list").expect("文件列表没有渲染");

        /// 打开某个对话框的动作。用普通函数指针而不是闭包 trait 对象——
        /// 这样 `cases` 是个朴素数组，不必为类型复杂度开 `allow`。
        type OpenDialog = fn(&mut RootView, &mut Context<RootView>);

        let cases: [(&str, OpenDialog); 4] = [
            ("连接到服务器", |v, cx| v.open_connect_dialog(cx)),
            ("信息提示", |v, cx| v.notice("测试消息", None, cx)),
            ("设置窗口", |v, cx| {
                v.modal = Modal::Settings;
                cx.notify();
            }),
            ("命令面板", |v, cx| {
                v.modal = Modal::CommandPalette;
                cx.notify();
            }),
        ];

        for (name, open) in cases {
            cx.update(|_window, cx| root.update(cx, open));
            cx.update(|window, cx| window.render_frame(cx));

            let overlay = cx.debug_bounds("mo-dialog-overlay").unwrap_or_else(|| {
                panic!("「{name}」没有渲染成浮层（遮罩缺失）——对话框应当是带遮罩的模态框")
            });
            assert_eq!(
                overlay.origin.x,
                px(0.0),
                "「{name}」遮罩没有从窗口左缘开始"
            );
            assert_eq!(
                overlay.origin.y,
                px(0.0),
                "「{name}」遮罩没有从窗口顶部开始"
            );
            assert!(
                (f32::from(overlay.size.width) - vw).abs() < 1.0
                    && (f32::from(overlay.size.height) - vh).abs() < 1.0,
                "「{name}」遮罩没有铺满整个视口：overlay={overlay:?} 视口={vw}x{vh}"
            );
            assert!(
                cx.debug_bounds("mo-dialog-card").is_some(),
                "「{name}」对话框卡片没有渲染（遮罩下没有内容）"
            );
            // 绝对定位的浮层不该挤动下层文件列表（与右键菜单同一条约束）。
            let list_after = cx.debug_bounds("mo-file-list").expect("文件列表没有渲染");
            assert_eq!(
                list_before, list_after,
                "「{name}」浮层挤动了文件列表：绝对定位模态不应当影响根容器的 flex 布局"
            );

            // 关闭后遮罩必须消失（否则「模态没真正关掉」）。
            cx.update(|_window, cx| {
                root.update(cx, |v, cx| {
                    v.modal = Modal::None;
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            assert!(
                cx.debug_bounds("mo-dialog-overlay").is_none(),
                "「{name}」关闭后遮罩仍在——模态没有真正关闭"
            );
        }
    }

    /// 对话框的**标题栏必须带上卡片顶部的那两个圆角**。
    ///
    /// 卡片是 `rounded(12)`，但标题栏自带底色（`theme::container()`）——它是
    /// **后画的矩形**。gpui 不会把子元素裁进父级圆角（这个 fork 的 `div` 只认
    /// `Overflow::Scroll`，`overflow_hidden()` 没有任何地方消费），于是矩形的
    /// 标题栏把卡片上面两个角盖成了直角，下面两个角因为没有子元素覆盖仍是圆的
    /// ——就是「连接到服务器」弹窗「缺了 2 个圆角」那个现象。
    ///
    /// headless 里拿不到栅格化截图，但 `painted_quads()` 给出了每个 quad 的
    /// `corner_radii`，足够直接断言绘制输出。
    #[test]
    fn dialog_header_carries_the_card_corner_radius() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|_window, cx| root.update(cx, |v, cx| v.open_connect_dialog(cx)));
        cx.update(|window, cx| window.render_frame(cx));

        let card = cx
            .debug_bounds("mo-dialog-card")
            .expect("对话框卡片没有渲染");
        // `debug_bounds` 是逻辑像素，`painted_quads` 是缩放后像素。
        let scale = cx.update(|window, _cx| window.scale_factor());
        let near = |a: f32, b: f32| (a - b).abs() < 0.5;
        let card_x = f32::from(card.origin.x) * scale;
        let card_y = f32::from(card.origin.y) * scale;
        let card_w = f32::from(card.size.width) * scale;

        let quads = cx.update(|window, _cx| window.painted_quads());
        let same_left_top_width = |q: &gpui_kit::Quad| {
            near(q.bounds.origin.x.as_f32(), card_x)
                && near(q.bounds.origin.y.as_f32(), card_y)
                && near(q.bounds.size.width.as_f32(), card_w)
        };

        // 卡片本身：四角同半径的圆角底色。
        let card_quad = quads
            .iter()
            .find(|q| {
                same_left_top_width(q)
                    && q.corner_radii.top_left.as_f32() > 0.0
                    && q.corner_radii.top_left == q.corner_radii.top_right
                    && q.corner_radii.top_left == q.corner_radii.bottom_left
            })
            .expect("没找到卡片那张四角圆角的底色 quad");
        let r = card_quad.corner_radii.top_left.as_f32();

        // 标题栏：卡片有 1px 边框，标题栏贴在边框**内侧**——与卡片同缩 1 逻辑
        // px、宽度少 2px，但明显更矮（卡片全高已被上一条排除）。
        // 位置认脸，圆角留给下面的断言。
        let inset = scale; // 1 逻辑 px 边框 = scale 物理 px
        let header = quads
            .iter()
            .find(|q| {
                near(q.bounds.origin.x.as_f32(), card_x + inset)
                    && near(q.bounds.origin.y.as_f32(), card_y + inset)
                    && near(q.bounds.size.width.as_f32(), card_w - 2.0 * inset)
                    && q.bounds.size.height.as_f32() < card_quad.bounds.size.height.as_f32() - 1.0
            })
            .expect("没找到对话框标题栏那一层的 quad");

        assert!(
            near(header.corner_radii.top_left.as_f32(), r)
                && near(header.corner_radii.top_right.as_f32(), r),
            "标题栏顶部圆角没有跟随卡片（卡片半径 {r}）——它会把卡片上面两个角\
             盖成直角，正是「缺了 2 个圆角」：标题栏圆角={:?}",
            header.corner_radii
        );
        assert_eq!(
            header.corner_radii.bottom_left.as_f32(),
            0.0,
            "标题栏底部接正文，不该有圆角"
        );
        assert_eq!(
            header.corner_radii.bottom_right.as_f32(),
            0.0,
            "标题栏底部接正文，不该有圆角"
        );
    }

    /// 次级视图（A 类）必须**接管中央区**：浏览区（文件列表）整个让位，而不是
    /// 像对话框那样叠一层、下层仍看得见。
    ///
    /// 与 `dialogs_are_full_viewport_overlays` 成对——那条守 B 类「浮层不挤动
    /// 浏览区」，这条守 A 类「正当占满中央区」。两条一起钉住这次拆分的边界：
    /// 短对话框与长视图不再共用同一张 640px 固定宽卡片。
    #[test]
    fn central_views_take_over_the_browsing_area() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|window, cx| window.render_frame(cx));
        assert!(
            cx.debug_bounds("mo-file-list").is_some(),
            "初始应当渲染浏览区（文件列表）"
        );

        // 快捷键页已并入统一设置窗口（B 类浮层），不再是次级视图。
        let cases: [(&str, Modal); 2] = [
            ("全局搜索", Modal::GlobalSearch),
            ("扩展", Modal::Extensions),
        ];
        for (name, modal) in cases {
            cx.update(|_window, cx| {
                root.update(cx, |v, cx| {
                    v.modal = modal;
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));

            assert!(
                cx.debug_bounds("mo-file-list").is_none(),
                "「{name}」没有接管中央区：文件列表还在渲染（它应当占满中央区）"
            );
            assert!(
                cx.debug_bounds("mo-dialog-overlay").is_none(),
                "「{name}」是次级视图，不该用带遮罩的浮层"
            );

            cx.update(|_window, cx| {
                root.update(cx, |v, cx| {
                    v.modal = Modal::None;
                    cx.notify();
                });
            });
            cx.update(|window, cx| window.render_frame(cx));
            assert!(
                cx.debug_bounds("mo-file-list").is_some(),
                "「{name}」关闭后浏览区没有回来"
            );
        }
    }

    /// 「连接到服务器」的地址框是**真实输入框**：⌘A 选中框里的内容，不是后面的文件列表。
    ///
    /// 自绘的「字符串 + 假光标 `▏`」那版没有选区，⌘A 直接穿透到根视图的键表，
    /// 变成 `select.all` → 全选文件列表（用户报的就是这个）。换成框架的真实
    /// `InputState` 后，⌘A / ⌘C / ⌘V / ⌘Z / 光标全归输入组件，它在**绑定阶段**
    /// 就把键吃掉了，根本到不了本视图的监听器。
    #[test]
    fn cmd_a_in_the_connect_dialog_selects_the_field_not_the_file_list() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();
        let main = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };

        cx.update(|_window, cx| root.update(cx, |v, cx| v.open_connect_dialog(cx)));
        cx.update(|window, cx| window.render_frame(cx));

        assert!(
            cx.debug_bounds("mo-connect-field").is_some(),
            "连接对话框的地址框没有渲染出来"
        );
        let state = cx.update(|_window, cx| {
            root.read(cx)
                .connect_input
                .clone()
                .expect("打开对话框后应当已经建好真实输入框")
        });

        // 敲地址进去（字符归输入框——`handle_modal_key` 里那套手搓字符串已删掉）。
        cx.simulate_input("sftp://example.com");
        cx.simulate_keystrokes(&format!("{main}-a"));

        // 读焦点的 `focus_handle` 是 `Focusable` 的方法：本模块刻意不用 glob
        // （会与 `#[test]` 属性撞名，见模块头的说明），这里单独引入。
        use gpui_kit::Focusable as _;
        let (value, selected, focused) = cx.update(|window, cx| {
            let s = state.read(cx);
            (
                s.value().to_string(),
                s.selected_value().to_string(),
                s.focus_handle(cx).is_focused(window),
            )
        });
        assert!(focused, "地址框没有握着键盘焦点——按键会漏给后面的浏览区");
        assert_eq!(value, "sftp://example.com", "地址框没有收到输入的字符");
        assert_eq!(
            selected, value,
            "⌘A 应当选中地址框里的内容：value={value:?} selected={selected:?}"
        );
        // 同一条按键的另一半：后面的文件列表不能被动过。
        let files_selected = cx.update(|_window, cx| root.read(cx).panel().selection.count());
        assert_eq!(files_selected, 0, "⌘A 不该选到后面的文件列表");
    }

    /// 认证弹窗的密码框是**掩码**的——「密码全部显示为星号」就落在这里。
    ///
    /// 掩码由输入组件的绘制层实现（每个字符画成圆点），`is_masked()` 正是那个
    /// 绘制分支的开关；同时断言 `value()` 仍是明文，否则拿去登录必然失败。
    #[test]
    fn connect_auth_masks_the_password() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        // 真去连一台「会拒绝匿名登录」的服务器不是单测能做的事，所以直接摆好
        // 「服务器要凭据」那一刻的状态（生产路径见 `on_connect_result`）。
        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.connect_auth = Some(ConnectAuthState::new(
                    "ftp://example.com:2121".to_string(),
                    Some("530 Login incorrect.".to_string()),
                    "alice",
                ));
                v.modal = Modal::ConnectAuth;
                cx.notify();
            })
        });
        cx.update(|window, cx| window.render_frame(cx));

        for selector in ["mo-connect-user", "mo-connect-pass", "mo-connect-remember"] {
            assert!(
                cx.debug_bounds(selector).is_some(),
                "认证弹窗没有渲染出 {selector}"
            );
        }

        // `focus_handle` 是 `Focusable` 的方法：本模块刻意不用 glob，单独引入。
        use gpui_kit::Focusable as _;
        let (user_value, masked, focus_on_pass) = cx.update(|window, cx| {
            let v = root.read(cx);
            let auth = v.connect_auth.as_ref().expect("认证态应当还在");
            let user = auth.user.clone().expect("用户名框应当已建好");
            let pass = auth.pass.clone().expect("密码框应当已建好");
            (
                user.read(cx).value().to_string(),
                // 掩码状态要经 `presentation()` 读——它是绘制层拿到的只读视图，
                // 也正是「画不画圆点」那个分支看得见的东西。
                pass.read(cx).presentation().is_masked(),
                pass.read(cx).focus_handle(cx).is_focused(window),
            )
        });
        assert_eq!(user_value, "alice", "用户名没有从地址里带进认证弹窗");
        assert!(masked, "密码框没开掩码——密码会明文显示出来");
        // 用户名已经有了 → 焦点直接落在密码上（改密码重试是这个弹窗的主路径）。
        assert!(focus_on_pass, "用户名非空时焦点应当落在密码框上");

        // 掩码只影响**显示**：值还是明文，`connect_auth_submit` 直接拿它去登录。
        cx.simulate_input("s3cr3t");
        let typed = cx.update(|_window, cx| {
            let v = root.read(cx);
            let pass = v
                .connect_auth
                .as_ref()
                .and_then(|a| a.pass.clone())
                .expect("密码框应当还在");
            pass.read(cx).value().to_string()
        });
        assert_eq!(typed, "s3cr3t", "掩码不该改变实际取值");
    }

    /// 「连接到服务器」对话框会列出记住过的服务器，每行可点（一键重连）。
    #[test]
    fn connect_dialog_lists_remembered_servers() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.connect_servers = vec![mo_app::SavedServer {
                    endpoint: "ftp://example.com:2121".to_string(),
                    user: "alice".to_string(),
                    last_used: 1,
                }];
                v.modal = Modal::ConnectServer;
                cx.notify();
            })
        });
        cx.update(|window, cx| window.render_frame(cx));

        assert!(
            cx.debug_bounds("mo-connect-servers").is_some(),
            "记住的服务器列表没有渲染出来"
        );
        assert!(
            cx.debug_bounds("mo-connect-server-0").is_some(),
            "列表里没有第一行——点不了一键重连"
        );
    }

    /// 「已记住的服务器」列表的观感：外框一圈描边 + 圆角，行底色奇偶交替（斑马纹），
    /// 行首一枚协议图标。
    ///
    /// 斑马纹沿用文件列表那组角色色（奇数行 `theme::zebra()`、偶数行 `theme::surface()`）。
    /// 贴边的首 / 末行还得自己收上 / 下两个角——gpui 不把子元素裁进父级圆角，矩形行底色
    /// 会把外框那两个角切方（同 `context_menu_item_hover_follows_the_panel_corner`）。
    /// 断言的是 `painted_quads()` 的真实绘制输出。
    ///
    /// ⚠️ 只断言**相对**关系（相邻两行不同色、隔一行同色；描边 alpha > 0），不拿
    /// `theme::zebra()` 这类绝对色值去比：调色板是进程级全局槽位，
    /// `theme::tests::set_switches_active_palette` 与主题选择器用例会在并行跑时临时
    /// 翻成深色，比色值就是随机闪断。
    #[test]
    fn remembered_servers_list_has_a_frame_and_zebra_rows() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.connect_servers = [
                    ("smb://172.25.48.48", ""),
                    ("ftp://example.com:2121", "alice"),
                    ("davs://cloud.example.com", "bob"),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, (endpoint, user))| mo_app::SavedServer {
                    endpoint: endpoint.to_string(),
                    user: user.to_string(),
                    last_used: i as i64,
                })
                .collect();
                v.modal = Modal::ConnectServer;
                cx.notify();
            })
        });
        cx.update(|window, cx| window.render_frame(cx));

        // `debug_bounds` 是逻辑像素，`painted_quads` 是缩放后像素：先统一成后者。
        let scale = cx.update(|window, _cx| window.scale_factor());
        let mut rects = [(0.0_f32, 0.0, 0.0, 0.0); 4];
        for (slot, &selector) in [
            "mo-connect-servers",
            "mo-connect-server-0",
            "mo-connect-server-1",
            "mo-connect-server-2",
        ]
        .iter()
        .enumerate()
        {
            let b = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} 没有渲染"));
            rects[slot] = (
                f32::from(b.origin.x) * scale,
                f32::from(b.origin.y) * scale,
                f32::from(b.size.width) * scale,
                f32::from(b.size.height) * scale,
            );
        }
        let frame_rect = rects[0];
        let row_rects = [rects[1], rects[2], rects[3]];

        let quads = cx.update(|window, _cx| window.painted_quads());
        let same_rect = |q: &gpui_kit::Quad, r: (f32, f32, f32, f32)| {
            let near = |a: f32, b: f32| (a - b).abs() < 1.0;
            near(q.bounds.origin.x.as_f32(), r.0)
                && near(q.bounds.origin.y.as_f32(), r.1)
                && near(q.bounds.size.width.as_f32(), r.2)
                && near(q.bounds.size.height.as_f32(), r.3)
        };
        let quad_at = |r: (f32, f32, f32, f32), what: &str| -> gpui_kit::Quad {
            *quads
                .iter()
                .find(|q| same_rect(q, r))
                .unwrap_or_else(|| panic!("{what} 没有画出同位同尺寸的 quad"))
        };

        // ---- 外框：四角同半径的圆角底色 + 一圈描边。
        // 描边不并进底色那张 quad——gpui 每条边单独画一张同尺寸的 quad，
        // 只在 `border_widths` 对应那条边上带宽度，所以要另找。
        let frame = quad_at(frame_rect, "服务器列表外框");
        let radius = frame.corner_radii.top_left.as_f32();
        assert!(
            radius > 0.0
                && frame.corner_radii.top_right.as_f32() == radius
                && frame.corner_radii.bottom_left.as_f32() == radius
                && frame.corner_radii.bottom_right.as_f32() == radius,
            "服务器列表外框不是四角同半径的圆角框：{:?}",
            frame.corner_radii
        );
        let edges: Vec<(f32, f32)> = quads
            .iter()
            .filter(|q| same_rect(q, frame_rect))
            .map(|q| {
                (
                    q.border_widths.top.as_f32()
                        + q.border_widths.right.as_f32()
                        + q.border_widths.bottom.as_f32()
                        + q.border_widths.left.as_f32(),
                    q.border_color.a,
                )
            })
            .filter(|(w, _)| *w > 0.0)
            .collect();
        assert!(
            edges.len() >= 4,
            "服务器列表外框的描边不全（四条边各一张 quad）：实测 {}/4 条，宽度={:?}",
            edges.len(),
            edges
        );
        assert!(
            edges.iter().all(|(_, a)| *a > 0.0),
            "外框描边是透明的（alpha=0 就看不见这圈框了）：{edges:?}"
        );

        // ---- 斑马纹：奇偶交替（色值取自 `theme::zebra()` / `theme::surface()`）。
        let row_quads = [
            quad_at(row_rects[0], "第 0 行"),
            quad_at(row_rects[1], "第 1 行"),
            quad_at(row_rects[2], "第 2 行"),
        ];
        assert_ne!(
            row_quads[0].background, row_quads[1].background,
            "相邻两行同色——斑马纹没生效"
        );
        assert_eq!(
            row_quads[0].background, row_quads[2].background,
            "第 0 行与第 2 行不同色——交替规律断了（奇偶反了或多算了一行？）"
        );

        // ---- 贴框角的行自己收角：首行收上面两个、末行收下面两个、中间行全直角。
        // 收的半径不大于外框即可（外框还含 1px 描边，行半径本就该小一圈）。
        let corners = |q: &gpui_kit::Quad| {
            (
                q.corner_radii.top_left.as_f32(),
                q.corner_radii.top_right.as_f32(),
                q.corner_radii.bottom_left.as_f32(),
                q.corner_radii.bottom_right.as_f32(),
            )
        };
        let first = corners(&row_quads[0]);
        assert!(
            first.0 > 0.0 && first.0 == first.1 && first.0 <= radius,
            "首行底色没跟随外框顶部圆角（外框半径 {radius}）：会把框上面两个角切方\
             —— {first:?}"
        );
        assert_eq!(
            (first.2, first.3),
            (0.0, 0.0),
            "首行下面还有行接着，不该有圆角"
        );
        assert_eq!(
            corners(&row_quads[1]),
            (0.0, 0.0, 0.0, 0.0),
            "中间行贴不到框角，四角都该是直角"
        );
        let last = corners(&row_quads[2]);
        assert!(
            last.2 > 0.0 && last.2 == last.3 && last.2 <= radius,
            "末行底色没跟随外框底部圆角（外框半径 {radius}）：会把框下面两个角切方\
             —— {last:?}"
        );
        assert_eq!(
            (last.0, last.1),
            (0.0, 0.0),
            "末行上面还有行接着，不该有圆角"
        );

        // ---- 行首的协议图标：每行一枚，落在地址文字之前。
        // 哪个协议配哪个形状由 `icons::protocol_icon_maps_scheme_aliases_and_falls_back`
        // 钉住，这里只守「图标位真的排出来了、没把行挤变形」。
        for (i, selector) in [
            "mo-connect-server-icon-0",
            "mo-connect-server-icon-1",
            "mo-connect-server-icon-2",
        ]
        .into_iter()
        .enumerate()
        {
            let b = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("第 {i} 行行首没有协议图标"));
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            assert!(
                (w - 14.0).abs() < 1.0 && (h - 14.0).abs() < 1.0,
                "第 {i} 行的协议图标不是 14×14：{w}×{h}"
            );
            let (row_x, row_w) = (row_rects[i].0, row_rects[i].2);
            let x = f32::from(b.origin.x) * scale;
            assert!(
                x > row_x && x < row_x + row_w / 2.0,
                "第 {i} 行的协议图标不在行的左半（行左缘 {row_x}，图标左缘 {x}）：\
                 排到地址后面了？"
            );
        }
    }

    /// 「使用说明」默认折叠，点一下展开各协议写法（再点一下收起）。
    #[test]
    fn connect_help_starts_collapsed_and_toggles() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|_window, cx| root.update(cx, |v, cx| v.open_connect_dialog(cx)));
        cx.update(|window, cx| window.render_frame(cx));

        assert!(
            cx.debug_bounds("mo-connect-help-toggle").is_some(),
            "「使用说明」开关没有渲染出来"
        );
        assert!(
            cx.debug_bounds("mo-connect-help-panel").is_none(),
            "使用说明默认应当折叠，不该占对话框高度"
        );

        // 点开关展开：面板出现、状态翻转。
        let toggle = cx
            .debug_bounds("mo-connect-help-toggle")
            .expect("开关应当有几何信息才点得到");
        cx.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        cx.update(|window, cx| window.render_frame(cx));
        let (panel_visible, expanded) = (
            cx.debug_bounds("mo-connect-help-panel").is_some(),
            cx.update(|_window, cx| root.read(cx).connect_help_open),
        );
        assert!(panel_visible, "点开「使用说明」后面板没有渲染出来");
        assert!(expanded, "点击后 connect_help_open 应当为 true");

        // 再点一下收起：面板消失。
        let toggle = cx
            .debug_bounds("mo-connect-help-toggle")
            .expect("展开后开关仍应在");
        cx.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        cx.update(|window, cx| window.render_frame(cx));
        let (panel_visible, expanded) = (
            cx.debug_bounds("mo-connect-help-panel").is_some(),
            cx.update(|_window, cx| root.read(cx).connect_help_open),
        );
        assert!(!panel_visible, "再次点击后面板应当收起");
        assert!(!expanded, "再次点击后 connect_help_open 应当为 false");
    }

    /// 认证弹窗按 Esc 退回地址对话框，而不是整个关掉。
    ///
    /// 用户按 Esc 多半是想改地址（端口敲错、主机名打错），把刚敲的地址一起丢掉
    /// 太粗暴。
    #[test]
    fn connect_auth_escape_returns_to_the_address_dialog() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.connect_auth = Some(ConnectAuthState::new(
                    "ftp://example.com:2121".to_string(),
                    None,
                    "alice",
                ));
                v.modal = Modal::ConnectAuth;
                cx.notify();
            })
        });
        cx.update(|window, cx| window.render_frame(cx));

        cx.simulate_keystrokes("escape");

        let (on_address_dialog, has_auth) = cx.update(|_window, cx| {
            let v = root.read(cx);
            // `Modal` 里有 `String`（`Info`），不是 `Copy`，所以用 `matches!` 判。
            (
                matches!(v.modal, Modal::ConnectServer),
                v.connect_auth.is_some(),
            )
        });
        assert!(
            on_address_dialog,
            "Esc 应当退回地址对话框（而不是整个关掉）"
        );
        assert!(!has_auth, "退回时认证态应当一并清掉");
    }

    /// 模态 / 对话框打开时，作用于下层文件列表的动作**不派发**。
    ///
    /// 见证点用 `file.properties`（⌘I）：它是浏览区动作，一按就会把当前模态换成
    /// 属性对话框——模态还是原来那个，说明它被吞掉了。
    ///
    /// 不用 ⌘A 当见证：这里没有文件可选，「全选」是空转，测不出真假。
    /// 反例也在同一条测试里：窗口级动作（⌘T）不该被一起吞掉——Finder 的 sheet
    /// 也照样响应菜单快捷键，一刀切地吞会让模态一开连标签页都建不了。
    #[test]
    fn browser_shortcuts_are_swallowed_while_a_dialog_is_open() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();
        let main = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };

        // 命令面板：自绘输入行 → 焦点在根视图上，⌘I 会一路走到全局键表。
        // `panel.path` 摆好，属性对话框才有得开（否则 `open_properties` 直接返回，
        // 见证点就成了空转）。
        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.panel_mut().path = Some(PathBuf::from("/tmp/mo-modal-key-test"));
                v.modal = Modal::CommandPalette;
                cx.notify();
            });
        });
        cx.update(|window, cx| window.render_frame(cx));

        cx.simulate_keystrokes(&format!("{main}-i"));
        let modal = cx.update(|_window, cx| root.read(cx).modal.clone());
        assert_eq!(
            modal,
            Modal::CommandPalette,
            "⌘I 把对话框顶掉了——模态打开时浏览区动作没被吞掉"
        );

        // 反例：窗口级动作照旧。
        let before = cx.update(|_window, cx| root.read(cx).pane().tabs.len());
        cx.simulate_keystrokes(&format!("{main}-t"));
        let after = cx.update(|_window, cx| root.read(cx).pane().tabs.len());
        assert_eq!(
            after,
            before + 1,
            "模态打开时窗口级快捷键（⌘T）不该被一起吞掉"
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

    /// 右键菜单的**首 / 末项 hover 底色必须跟着面板圆角**收一下。
    ///
    /// 面板是 `.rounded(PANEL_RADIUS)` + 上下内边距 `PAD`，而菜单项的 hover 底色是
    /// `w_full()` 的**矩形**——上下的边一路顶到面板边缘，把面板上下那两个圆角切方。
    /// gpui 不会把子元素裁进父级圆角（与 `dialog_header_carries_the_card_corner_radius`
    /// 是同一个病），所以只能让 hover 样式自己收角：首项 `.rounded_t`、末项 `.rounded_b`、
    /// 只有一项时四个角全收。它只在 hover 时才可见，肉眼很容易漏过去。
    ///
    /// 断言的是 `painted_quads()` 里的真实绘制输出（每个 quad 的 `corner_radii`），
    /// 不需要栅格化；鼠标用 `simulate_mouse_move` 真的停到那一行上。
    #[test]
    fn context_menu_item_hover_follows_the_panel_corner() {
        use crate::context_menu::{ITEM_H, ITEM_RADIUS, MENU_W, PAD, PANEL_RADIUS};

        /// 窗口逻辑坐标下的一点，找出它底下那条菜单项 hover 底色 quad
        /// （高 = 一项、宽 = 面板内容盒——只有 hover 中的那一行会画出这种 quad）。
        fn pick_hover_quad(quads: &[gpui_kit::Quad], x: f32, y: f32, scale: f32) -> gpui_kit::Quad {
            let (x, y) = (x * scale, y * scale);
            quads
                .iter()
                .find(|q| {
                    let (ox, oy) = (q.bounds.origin.x.as_f32(), q.bounds.origin.y.as_f32());
                    let (w, h) = (q.bounds.size.width.as_f32(), q.bounds.size.height.as_f32());
                    ox <= x
                        && x <= ox + w
                        && oy <= y
                        && y <= oy + h
                        && (h - ITEM_H * scale).abs() < 1.0
                        && w > (MENU_W - 8.0) * scale
                })
                .copied()
                .expect("鼠标底下没有菜单项 hover 底色的 quad（项没渲染？还是没 hover 上？）")
        }

        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        // 空白处菜单：首项「新建文件夹」、末项「显示简介」，全部可用（都有 hover）。
        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.open_context_menu(None, 120.0, 120.0, 0, 0, cx)
            })
        });
        cx.update(|window, cx| window.render_frame(cx));

        let menu = cx
            .debug_bounds("mo-context-menu")
            .expect("右键菜单没有渲染");
        let scale = cx.update(|window, _cx| window.scale_factor());
        let (menu_x, menu_y) = (f32::from(menu.origin.x), f32::from(menu.origin.y));
        let menu_w = f32::from(menu.size.width);
        let menu_h = f32::from(menu.size.height);
        // 项底色的左缘（面板内容盒内，避开左边框）。
        let x = menu_x + 2.0;
        let r = ITEM_RADIUS * scale;
        let near = |a: f32, b: f32| (a - b).abs() < 0.5;

        // ---- 第一项：上面两角跟随面板圆角，下面两角是直角
        let (fx, fy) = (x, menu_y + PAD + ITEM_H / 2.0);
        cx.update(|window, cx| {
            window.simulate_mouse_move(gpui_kit::Point::new(px(fx), px(fy)), cx)
        });
        cx.update(|window, cx| window.render_frame(cx));

        let quads = cx.update(|window, _cx| window.painted_quads());

        // 面板自己的圆角（同位置同尺寸、四角有半径的那张底色）。
        let panel_r = quads
            .iter()
            .find(|q| {
                near(q.bounds.origin.x.as_f32(), menu_x * scale)
                    && near(q.bounds.origin.y.as_f32(), menu_y * scale)
                    && near(q.bounds.size.width.as_f32(), menu_w * scale)
                    && near(q.bounds.size.height.as_f32(), menu_h * scale)
                    && q.corner_radii.top_left.as_f32() > 0.0
            })
            .map(|q| q.corner_radii.top_left.as_f32())
            .expect("没找到面板那层圆角底色 quad");
        assert!(
            near(panel_r, PANEL_RADIUS * scale),
            "面板圆角实测 {}pt，不是 {PANEL_RADIUS}pt",
            panel_r / scale
        );
        // 项底色圆角必须比面板的小，否则它会溢出面板圆角——不是同心就穿帮。
        assert!(
            r < panel_r,
            "项底色圆角 {ITEM_RADIUS}pt 不小于面板圆角 {}pt：会溢出面板的圆角",
            panel_r / scale
        );

        let q = pick_hover_quad(&quads, fx, fy, scale);
        assert!(
            (q.corner_radii.top_left.as_f32() - r).abs() < 0.5
                && (q.corner_radii.top_right.as_f32() - r).abs() < 0.5,
            "第一项的 hover 底色没有跟随面板顶部圆角（期望 {r}）——它会把面板上面两个角\
             切方：{:?}",
            q.corner_radii
        );
        assert_eq!(
            (
                q.corner_radii.bottom_left.as_f32(),
                q.corner_radii.bottom_right.as_f32()
            ),
            (0.0, 0.0),
            "第一项下面接的不是面板边缘，不该有圆角"
        );

        // ---- 最后一项：反过来，下面两角跟随面板圆角
        let (lx, ly) = (x, menu_y + menu_h - PAD - ITEM_H / 2.0);
        cx.update(|window, cx| {
            window.simulate_mouse_move(gpui_kit::Point::new(px(lx), px(ly)), cx)
        });
        cx.update(|window, cx| window.render_frame(cx));

        let quads = cx.update(|window, _cx| window.painted_quads());
        let q = pick_hover_quad(&quads, lx, ly, scale);
        assert!(
            (q.corner_radii.bottom_left.as_f32() - r).abs() < 0.5
                && (q.corner_radii.bottom_right.as_f32() - r).abs() < 0.5,
            "末项的 hover 底色没有跟随面板底部圆角（期望 {r}）：{:?}",
            q.corner_radii
        );
        assert_eq!(
            (
                q.corner_radii.top_left.as_f32(),
                q.corner_radii.top_right.as_f32()
            ),
            (0.0, 0.0),
            "末项上面接的不是面板边缘，不该有圆角"
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

    /// 统一设置窗口：三个旧入口（界面 / 外观 / 快捷键）都开进**同一个**模态、
    /// 各自落到对应标签页；← → 循环换页；带着未提交的主题预览翻页要还原预览。
    #[test]
    fn settings_window_unifies_the_three_pickers() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|window, cx| window.render_frame(cx));

        // 三个入口 → 同一个 Modal::Settings，各自落在自己的标签页。
        let (modal, tab) = cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.open_layout_picker(cx);
                (v.modal.clone(), v.settings_tab)
            })
        });
        assert!(matches!(modal, Modal::Settings), "布局入口应开设置窗口");
        assert_eq!(tab, SettingsTab::Layout, "布局入口应停在「界面」页");

        cx.update(|_window, cx| root.update(cx, |v, cx| v.open_keys_picker(cx)));
        let tab = cx.update(|_window, cx| root.update(cx, |v, _| v.settings_tab));
        assert_eq!(tab, SettingsTab::Keys, "快捷键入口应停在「快捷键」页");

        // ←→ 循环换页：Keys ←→ Theme；从 Theme 翻走要还原未提交的预览。
        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.keys_capturing = None;
                v.settings_step(-1, cx);
            })
        });
        let (tab, _theme) =
            cx.update(|_window, cx| root.update(cx, |v, _| (v.settings_tab, v.theme_name.clone())));
        assert_eq!(tab, SettingsTab::Theme, "← 应循环到「外观」页");

        // ↓ 预览下一个主题（不落盘），然后翻页离开：预览必须被还原。
        cx.update(|_window, cx| root.update(cx, |v, cx| v.theme_move(1, cx)));
        let previewed = cx.update(|_window, cx| root.update(cx, |v, _| v.theme_name.clone()));
        let (tab, restored) = cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                v.settings_step(1, cx);
                (v.settings_tab, v.theme_name.clone())
            })
        });
        assert_eq!(tab, SettingsTab::Keys, "→ 应循环回「快捷键」页");
        assert_ne!(previewed, restored, "翻页后预览主题应被还原");
        let want = crate::theme::resolve(&restored, &HashMap::new(), false);
        assert_eq!(crate::theme::current(), want, "调色板应回到配置主题");

        // 渲染一遍确认卡片画了出来（外壳走 dialog_overlay）。
        cx.update(|window, cx| window.render_frame(cx));
        assert!(
            cx.debug_bounds("mo-dialog-card").is_some(),
            "设置窗口应渲染成带遮罩的浮层卡片"
        );
        let _ = root;
    }

    /// 定位结果 → 滚动行号：列表 / 列视图用列表行，网格 / 画廊按列数换算条目位。
    ///
    /// 这条守的是「网格 / 画廊下方向键与 type-ahead 滚错位置」——两种视图的
    /// `uniform_list` 计数空间不同（列表按行含组头、网格按单元行），拿错一个
    /// 目标行就会滚偏（甚至滚到列表外）。
    #[test]
    fn located_row_converts_entry_pos_to_cell_row_in_grid_views() {
        let mut panel = Panel::new(AppState::new());
        // pos 7 / row 9：故意让两者不等，谁被用了一看便知。
        let hit = mo_app::LocateHit { pos: 7, row: 9 };

        panel.view_mode = crate::panel::ViewMode::List;
        assert_eq!(located_row(&panel, hit), 9, "列表用列表行（含组头）");
        panel.view_mode = crate::panel::ViewMode::Columns;
        assert_eq!(located_row(&panel, hit), 9, "列视图同样用列表行");

        // 网格 / 画廊：条目位 ÷ 列数（7 / 3 = 2）。
        panel.view_mode = crate::panel::ViewMode::Grid;
        panel.grid_cols = 3;
        assert_eq!(located_row(&panel, hit), 2);
        panel.view_mode = crate::panel::ViewMode::Gallery;
        assert_eq!(located_row(&panel, hit), 2, "画廊与网格同为单元行");

        // 单列退化：条目位即单元行；列数非法（0）按 1 列算，不 panic。
        panel.grid_cols = 1;
        assert_eq!(located_row(&panel, hit), 7);
        panel.grid_cols = 0;
        assert_eq!(located_row(&panel, hit), 7);
    }

    /// 连按同字母的判据：只有「长度 ≥ 2 且字符全同」才算重复输入。
    #[test]
    fn repeat_detection_covers_double_and_triple_press() {
        assert!(!type_ahead_repeats_one_char(""), "空缓冲不算重复");
        assert!(
            !type_ahead_repeats_one_char("a"),
            "首字符应从头找第一个匹配项"
        );
        assert!(type_ahead_repeats_one_char("aa"));
        assert!(type_ahead_repeats_one_char("aaa"));
        assert!(!type_ahead_repeats_one_char("ab"), "正常前缀不受影响");
    }

    /// 传输估速：首次快照只建样本（速度 0 不显示），间隔后差分出速度与剩余时间；
    /// 操作不再 Running（暂停 / 结束）时样本清空，恢复 Running 从零重新观测。
    #[test]
    fn op_speeds_averages_progress_deltas() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        let mk = |done: u64, status: mo_operations::OperationStatus| OperationHandle {
            id: 7,
            describe: "复制 /a → /b".to_string(),
            status,
            progress: (done, 1000),
            pausable: true,
        };

        cx.update(|_window, cx| {
            root.update(cx, |v, _| {
                crate::inject_ops_for_tests(v, vec![mk(0, mo_operations::OperationStatus::Running)])
            })
        });
        let first = cx.update(|_window, cx| {
            root.update(cx, |v, _| {
                let ops = v.all_ops();
                let mut speeds = v.op_speeds(&ops);
                speeds.remove(&7).unwrap()
            })
        });
        assert_eq!(first.0, 0.0, "首次观测没有上一次差分，速度应为 0");

        // 睡过最小采样间隔，再喂一段确定推进的进度：差分速度必须为正。
        std::thread::sleep(std::time::Duration::from_millis(150));
        let second = cx.update(|_window, cx| {
            root.update(cx, |v, _| {
                crate::inject_ops_for_tests(
                    v,
                    vec![mk(300, mo_operations::OperationStatus::Running)],
                );
                let ops = v.all_ops();
                let mut speeds = v.op_speeds(&ops);
                speeds.remove(&7).unwrap()
            })
        });
        assert!(
            second.0 > 0.0 && second.1 > 0.0,
            "推进 300/1000 字节后应有正速度与剩余时间：{second:?}"
        );

        // 暂停后样本清空：恢复 Running 时从零重新观测（速度回到 0）。
        let after_pause = cx.update(|_window, cx| {
            root.update(cx, |v, _| {
                crate::inject_ops_for_tests(
                    v,
                    vec![mk(300, mo_operations::OperationStatus::Paused)],
                );
                let ops = v.all_ops();
                let mut speeds = v.op_speeds(&ops);
                speeds.remove(&7)
            })
        });
        assert!(after_pause.is_none(), "暂停的操作不应出现在估速表里");
        let resumed = cx.update(|_window, cx| {
            root.update(cx, |v, _| {
                crate::inject_ops_for_tests(
                    v,
                    vec![mk(300, mo_operations::OperationStatus::Running)],
                );
                let ops = v.all_ops();
                let mut speeds = v.op_speeds(&ops);
                speeds.remove(&7).unwrap()
            })
        });
        assert_eq!(
            resumed.0, 0.0,
            "恢复后应从零重新观测，而不是沿用暂停前的速度"
        );
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

    /// 标签页的远程徽标只在「正在浏览远程」的标签上出现。
    ///
    /// 用户要求「访问 FTP 时标签页上要显示出来」。图标与主机名具体怎么画在
    /// `render_tab_bar` 里（headless 读不出文本，测不了），但**什么时候该挂**
    /// 这条判据在模型层，可以钉住——尤其是切回本地之后不该再挂：那时会话
    /// 仍活着（`open_local` 不断开），可这个标签看的已经是本地目录了。
    #[test]
    fn tab_badge_shows_only_while_browsing_a_remote() {
        crate::isolate_config_for_tests();
        // 独占会话表：库测试并行跑，共享进程级注册表会互相看见对方装的假服务器。
        let app = AppState::with_sessions(
            std::env::temp_dir().join("mo-ui-badge-trash"),
            Arc::new(mo_app::SessionRegistry::new()),
        );
        let panel = Panel::new(app.clone());
        assert_eq!(panel.remote_badge(), None, "本地标签不该有远程徽标");

        // 徽标只读地址，不必真连：拿本地 fs 当「已连接的后端」的替身。
        app.install_backend_for_test(Arc::new(mo_fs::LocalFileSystem), "ftp://example.com:2121");
        assert_eq!(
            panel.remote_badge().as_deref(),
            Some("example.com:2121"),
            "浏览远程时标签页要标出主机（省略默认端口，不带 ftp:// 前缀）"
        );

        // 切回本地：会话还活着，但标签页看的是本地 → 徽标应当消失。
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir();
        rt.block_on(app.open_local(&dir))
            .expect("本地目录应当能打开");
        assert_eq!(
            app.live_connections().len(),
            1,
            "前置条件：切到本地不该丢掉连接"
        );
        assert_eq!(
            panel.remote_badge(),
            None,
            "当前看的是本地，标签页不该还挂着远程徽标"
        );
    }

    /// 网格 / 画廊视图：内容四周要留白，单元名称要水平居中（长名字截断不溢出）。
    ///
    /// 两条都是肉眼验收时点出来的——原来 list 只给了左右 `px`，首行顶着工具栏、
    /// 滚到底最后一行贴着状态栏；名称贴着格的左缘而图标是居中的，看着就是歪。
    /// 画廊与网格共用同一个 `grid::cell`，两种模式各跑一遍（「画廊也没居中」是
    /// 用户第二次点出来的，别只测网格）。
    #[test]
    fn grid_and_gallery_inset_content_and_center_names() {
        for mode in [
            crate::panel::ViewMode::Grid,
            crate::panel::ViewMode::Gallery,
        ] {
            let tag = format!("{mode:?}");
            crate::isolate_config_for_tests();
            let mut cx = TestAppContext::single();
            cx.update(gpui_kit::init);
            let app = AppState::new();
            let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
            let root = root.clone();

            // 一条名字很长（验证截断不溢出），一条只有一个字符（验证真的收缩到内容宽）。
            cx.update(|_window, app| seed_window(&root, app, mode));
            cx.update(|window, cx| window.render_frame(cx));

            let grid = cx
                .debug_bounds("mo-grid")
                .unwrap_or_else(|| panic!("{tag} 视图没有渲染（view_mode 没切过去？）"));
            let cell0 = cx
                .debug_bounds("mo-grid-cell-0")
                .expect("第一个单元没有渲染");
            let cell1 = cx
                .debug_bounds("mo-grid-cell-1")
                .expect("第二个单元没有渲染");
            let name0 = cx
                .debug_bounds("mo-grid-name-0")
                .expect("第一个单元的名称没有渲染");
            let name1 = cx
                .debug_bounds("mo-grid-name-1")
                .expect("第二个单元的名称没有渲染");

            // ---- 四周留白（挂在 list 的 padding 上）。
            let left = f32::from(cell0.origin.x - grid.origin.x);
            let top = f32::from(cell0.origin.y - grid.origin.y);
            assert!(
                (left - 12.0).abs() < 0.51,
                "{tag}：左侧留白 {left}，应为 12"
            );
            assert!(
                (top - 12.0).abs() < 0.51,
                "{tag}：顶部留白 {top}，应为 12（首行顶着工具栏了）"
            );

            // ---- 名称居中。
            let center = |b: &gpui_kit::Bounds<gpui_kit::Pixels>| {
                f32::from(b.origin.x) + f32::from(b.size.width) / 2.0
            };
            for (name, cell, what) in [(name0, cell0, "长名字"), (name1, cell1, "短名字")] {
                assert!(
                    (center(&name) - center(&cell)).abs() < 0.51,
                    "{tag}：{what}没有水平居中：name={name:?} cell={cell:?}"
                );
            }
            // 长名字不许溢出到隔壁格（cell 自身有 4pt 内边距）。
            assert!(
                f32::from(name0.size.width) <= f32::from(cell0.size.width) - 8.0 + 0.51,
                "{tag}：长名字没被截断，宽度 {} 超出单元内容宽 {}",
                f32::from(name0.size.width),
                f32::from(cell0.size.width) - 8.0
            );
            // 短名字必须真的收缩到内容宽：否则「居中」是拿一个铺满的盒子比中点，
            // 文字照样贴左也测不出来（`text_center` 那版就是这么假绿的）。
            assert!(
                f32::from(name1.size.width) < f32::from(cell1.size.width) - 8.0 - 0.5,
                "{tag}：短名字那一层的宽度 {} 几乎铺满单元 {}——没收缩，居中断言无意义",
                f32::from(name1.size.width),
                f32::from(cell1.size.width)
            );
        }
    }

    /// 在面板里放几个假条目（网格 / 画廊 / 列表三条布局断言共用）。
    ///
    /// 不读真实 Home：headless 里那趟异步加载落不落地是不确定的（gpui 的非确定性
    /// 检测还会因此偶发 panic），而且换台机器目录内容就变了。直接写窗口快照——
    /// 范围已被 `Panel::covered` 命中，`ensure_window` 不会派生取窗任务把它冲掉。
    fn seed_window(
        root: &gpui_kit::Entity<RootView>,
        cx: &mut gpui_kit::App,
        mode: crate::panel::ViewMode,
    ) {
        root.update(cx, |v, cx| {
            let p = v.panel_mut();
            p.view_mode = mode;
            p.path = Some(PathBuf::from("/mo-layout-test"));
            p.window_start = 0;
            p.window = ["一个很长名字的文件夹，用来同时验证截断与居中", "b"]
                .into_iter()
                .enumerate()
                .map(|(i, name)| {
                    mo_app::WindowRow::Entry(mo_core::Entry::new(
                        mo_core::FileId::new(0, i as u128),
                        name.to_string(),
                        mo_core::EntryKind::Directory,
                        PathBuf::from(format!("/mo-layout-test/{i}")),
                    ))
                })
                .collect();
            p.visible_count = p.window.len();
            p.list_count = p.visible_count;
            cx.notify();
        })
    }

    /// 造一个 columns 列视图的面板（列数据全靠手填，不读盘）。
    fn seed_column_panel(
        root: &gpui_kit::Entity<RootView>,
        cx: &mut gpui_kit::App,
        path: &str,
        columns: &[&str],
    ) {
        root.update(cx, |v, _cx| {
            let p = v.panel_mut();
            p.view_mode = crate::panel::ViewMode::Columns;
            p.path = Some(PathBuf::from(path));
            p.columns = columns
                .iter()
                .map(|c| crate::panel::ColumnData {
                    path: PathBuf::from(c),
                    entries: Vec::new(),
                    cursor: 0,
                })
                .collect();
            p.column_busy = false;
        });
    }

    /// 列视图换目录必须**重建**根列，不能往末尾继续追加。
    ///
    /// 回归（从 `47a5fec` 列视图引入起就在，用户报的「一直无限加载同一个目录
    /// 导致应用卡死」）：`ensure_columns` 每帧按「第 0 列 ≠ 当前目录」判根列过期，
    /// 而 `load_column(.., None, ..)` 只把新列 push 到末尾 —— 第 0 列永远对不上，
    /// 于是每读完一列又判一次过期，列数无限增长。列视图的列是全量读盘（只对
    /// 渲染条数 `take` 封顶，`Vec<LightEntry>` 是整个目录），一个上万条的目录
    /// 每列一次全量读 + 排序，几秒就把内存和主线程拖死。
    #[test]
    fn switching_directory_rebuilds_column_stack_instead_of_appending() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let state = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(state, cx));
        let root = root.clone();

        // 已经在列视图里下钻过一级：第 0 列 home、第 1 列 sub。
        cx.update(|_window, app| {
            seed_column_panel(
                &root,
                app,
                "/mo-columns-test/home",
                &["/mo-columns-test/home", "/mo-columns-test/home/sub"],
            );
        });
        // 换目录（例如侧栏点「下载」）：当前目录变了，列栈还是旧的。
        cx.update(|_window, app| {
            root.update(app, |v, cx| {
                v.panel_mut().path = Some(PathBuf::from("/mo-columns-test/downloads"));
                v.ensure_columns(cx);
                {
                    let p = v.panel();
                    assert!(
                        p.columns.is_empty(),
                        "换根那一刻就该丢掉旧列栈，实际还剩 {} 列（第 0 列 {:?}）",
                        p.columns.len(),
                        p.columns.first().map(|c| c.path.clone()),
                    );
                    assert!(p.column_busy, "根列重建应当已经发起读盘");
                }
                // 下一帧（`ensure_columns` 在 render 里每帧都跑）不能重复追加。
                v.ensure_columns(cx);
                assert!(
                    v.panel().columns.is_empty(),
                    "在途期间不该再追加列（老实现就是这么无限涨的）",
                );
            });
        });
    }

    /// 根列已经对得上当前目录时，不该每帧重新读一次盘。
    #[test]
    fn matching_root_column_is_not_reloaded() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let state = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(state, cx));
        let root = root.clone();

        cx.update(|_window, app| {
            seed_column_panel(
                &root,
                app,
                "/mo-columns-test/home",
                &["/mo-columns-test/home"],
            );
            root.update(app, |v, cx| {
                v.ensure_columns(cx);
                let p = v.panel();
                assert!(!p.column_busy, "根列没变就不该再读一次盘");
                assert_eq!(p.columns.len(), 1, "列栈不该被动过");
            });
        });
    }

    /// 点击下钻（`parent = Some(i)`）在发起时**不动**列栈：旧列先留着，读完再一次性
    /// 替换。否则点击那 100–300ms 里会先闪掉几列，滚动位置也跟着跳。
    #[test]
    fn drilling_down_keeps_old_columns_until_loaded() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let state = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(state, cx));
        let root = root.clone();

        cx.update(|_window, app| {
            seed_column_panel(
                &root,
                app,
                "/mo-columns-test/home",
                &["/mo-columns-test/home", "/mo-columns-test/home/sub"],
            );
            root.update(app, |v, cx| {
                v.load_column(
                    cx,
                    PathBuf::from("/mo-columns-test/home/sub/bin"),
                    Some(1),
                    0,
                    0,
                );
                let p = v.panel();
                assert!(p.column_busy, "下钻应当发起读盘");
                assert_eq!(p.columns.len(), 2, "读回来之前不该动列栈");
            });
        });
    }

    /// 列头是**固定单行**的短标签：不管路径多长，各列头部高度都一致。
    ///
    /// 回归：列头原来直接渲染完整绝对路径（`/Users/11048490/.bluecode-desktop`），
    /// 在 210px 列宽 + 11px 字号下折成 3–4 行 —— 相邻列的前缀本来就是重复内容，
    /// 折行还让各列头部高度不一、列内容的起始线参差。现在只显示目录最后一段。
    ///
    /// ⚠️ 「显示的是最后一段而不是完整路径」这件事**测不到**：gpui 的测试通道只给
    /// `painted_quads()`，文本走 glyph sprite 不在 `Quad` 里。语义由 `path_label.rs`
    /// 的 `last_segment` 单测守（`keeps_only_the_last_segment` /
    /// `falls_back_to_root_path`），这里只钉几何（固定高度 = 不折行）。
    #[test]
    fn column_headers_are_single_line_regardless_of_path_depth() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let state = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(state, cx));
        let root = root.clone();

        // 三列，路径长度差很大：短 / 中 / 长（深）。第 0 列即当前目录，
        // 免得 `ensure_columns` 判根列过期把列栈清掉。
        cx.update(|_window, app| {
            seed_column_panel(
                &root,
                app,
                "/a",
                &[
                    "/a",
                    "/Users/11048490/.bluecode-desktop",
                    "/Users/11048490/.bluecode-desktop/change-sessions",
                ],
            );
        });
        cx.update(|window, cx| window.render_frame(cx));

        let heads = [
            "mo-col-head-0-0-0",
            "mo-col-head-0-0-1",
            "mo-col-head-0-0-2",
        ]
        .iter()
        .map(|id| cx.debug_bounds(id).unwrap_or_else(|| panic!("{id} 没渲染")))
        .collect::<Vec<_>>();
        for (i, b) in heads.iter().enumerate() {
            assert!(
                (f32::from(b.size.height) - crate::columns::HEAD_HEIGHT).abs() < 0.51,
                "第 {i} 列的列头高 {}，应为固定单行 {}（折行就是回归）",
                f32::from(b.size.height),
                crate::columns::HEAD_HEIGHT,
            );
        }
    }

    /// 视图模式按钮是**平铺**的一排（Finder 工具栏那组）：四枚等宽同高、等距，
    /// 并且**恰好只有当前模式那一枚画了底色** —— 只有一个高亮才看得出「现在在哪」。
    ///
    /// 老实现是单枚按钮显示模式名、点击循环到下一个：用户既看不出还有哪些模式，
    /// 也没法一步点到想去的那个（回归现场是用户拿 Finder 截图来对）。
    #[test]
    fn view_mode_buttons_are_tiled_with_exactly_one_active() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        // 切视图会重画中央区（可能派生补窗 / 缩略图任务），开一次官方豁免。
        cx.dispatcher.allow_parking();
        cx.update(gpui_kit::init);
        let state = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(state, cx));
        let root = root.clone();

        // ⚠️ `debug_bounds` 只收 `&'static str`，所以这里用字面量表，不能 `format!`。
        const IDS: [(&str, &str); 4] = [
            ("list", "mo-view-mode-list"),
            ("grid", "mo-view-mode-grid"),
            ("gallery", "mo-view-mode-gallery"),
            ("columns", "mo-view-mode-columns"),
        ];

        /// bounds（逻辑像素）→ 物理像素四元组，用来在 `painted_quads` 里找同位的 quad。
        fn quad_of(b: &gpui_kit::Bounds<gpui_kit::Pixels>, scale: f32) -> (f32, f32, f32, f32) {
            // ⚠️ `painted_quads` 是物理像素、`debug_bounds` 是逻辑像素。
            (
                f32::from(b.origin.x) * scale,
                f32::from(b.origin.y) * scale,
                f32::from(b.size.width) * scale,
                f32::from(b.size.height) * scale,
            )
        }

        /// 两个 bounds 是否「同位同尺寸」（容差 1px）。
        fn aligned(
            a: &gpui_kit::Bounds<gpui_kit::Pixels>,
            b: &gpui_kit::Bounds<gpui_kit::Pixels>,
        ) -> bool {
            let near = |x: f32, y: f32| (x - y).abs() < 1.0;
            near(f32::from(a.origin.x), f32::from(b.origin.x))
                && near(f32::from(a.origin.y), f32::from(b.origin.y))
                && near(f32::from(a.size.width), f32::from(b.size.width))
                && near(f32::from(a.size.height), f32::from(b.size.height))
        }

        fn set_mode(
            cx: &mut gpui_kit::VisualTestContext,
            root: &gpui_kit::Entity<RootView>,
            mode: crate::panel::ViewMode,
        ) {
            cx.update(|_window, app| {
                root.update(app, |v, cx| {
                    v.panel_mut().view_mode = mode;
                    cx.notify();
                });
            });
        }

        /// 渲染一帧并读出：四枚按钮的 bounds、有底色的那几枚、指示块的 bounds。
        fn measure(
            cx: &mut gpui_kit::VisualTestContext,
        ) -> (
            Vec<gpui_kit::Bounds<gpui_kit::Pixels>>,
            Vec<&'static str>,
            gpui_kit::Bounds<gpui_kit::Pixels>,
        ) {
            cx.update(|window, cx| window.render_frame(cx));

            let bounds = IDS
                .iter()
                .map(|(k, id)| {
                    cx.debug_bounds(id)
                        .unwrap_or_else(|| panic!("视图按钮 {k} 没渲染"))
                })
                .collect::<Vec<_>>();
            let indicator = cx
                .debug_bounds("mo-view-indicator")
                .expect("指示块没渲染 —— 高亮就是它画的");

            let scale = cx.update(|window, _cx| window.scale_factor());
            let quads = cx.update(|window, _cx| window.painted_quads());
            let near = |a: f32, b: f32| (a - b).abs() < 1.0;
            let filled = IDS
                .iter()
                .zip(&bounds)
                .filter(|(_, b)| {
                    let (x, y, w, h) = quad_of(b, scale);
                    quads.iter().any(|q| {
                        near(q.bounds.origin.x.as_f32(), x)
                            && near(q.bounds.origin.y.as_f32(), y)
                            && near(q.bounds.size.width.as_f32(), w)
                            && near(q.bounds.size.height.as_f32(), h)
                    })
                })
                .map(|((k, _), _)| *k)
                .collect::<Vec<_>>();
            (bounds, filled, indicator)
        }

        /// 把指示块的弹簧推到落定。
        ///
        /// 弹簧按**真实时钟**步进（`Instant::now()`，不是测试时钟），headless 里
        /// 也没有自动帧驱动 —— 所以只能真的等一会儿、再手动渲染几帧。按
        /// `SLIDE_SPRING` 的参数，最远一段（84px）约 250ms 落定，这里给 360ms 余量。
        fn settle_slide(cx: &mut gpui_kit::VisualTestContext) {
            for _ in 0..6 {
                std::thread::sleep(std::time::Duration::from_millis(60));
                cx.update(|window, cx| window.render_frame(cx));
            }
        }

        // 首帧：初始态**直接**停在「列表」那枚上（新挂载的弹簧 = target），没有
        // 入场滑动 —— 刚打开应用时不该看到一块灰底从别处滑过来。
        let (bounds, filled, indicator) = measure(cx);
        assert!(
            aligned(&indicator, &bounds[0]),
            "首帧指示块 {indicator:?} 没停在列表那枚 {:?} 上",
            bounds[0]
        );

        // 平铺：同高、同宽、同一行、等距。
        let (h0, w0) = (
            f32::from(bounds[0].size.height),
            f32::from(bounds[0].size.width),
        );
        assert!(w0 > 0.0, "按钮宽度是 0（被压扁了？）");
        for (i, b) in bounds.iter().enumerate() {
            assert!(
                (f32::from(b.size.height) - h0).abs() < 0.51
                    && (f32::from(b.size.width) - w0).abs() < 0.51,
                "第 {i} 枚按钮尺寸与第 0 枚不同：{}×{} vs {}×{w0}",
                f32::from(b.size.width),
                f32::from(b.size.height),
                f32::from(bounds[0].size.width),
            );
            assert!(
                (f32::from(b.origin.y) - f32::from(bounds[0].origin.y)).abs() < 0.51,
                "第 {i} 枚按钮没和第 0 枚排在同一行"
            );
            if i > 0 {
                assert!(
                    f32::from(b.origin.x) > f32::from(bounds[i - 1].origin.x),
                    "第 {i} 枚按钮没排在第 {} 枚右边",
                    i - 1
                );
            }
        }
        let gaps = (1..bounds.len())
            .map(|i| f32::from(bounds[i].origin.x) - f32::from(bounds[i - 1].origin.x) - w0)
            .collect::<Vec<_>>();
        for g in &gaps {
            assert!(
                (g - gaps[0]).abs() < 0.51,
                "按钮间距不均匀：{gaps:?}（看着就不像一组）"
            );
        }

        assert_eq!(
            filled,
            vec!["list"],
            "应恰好只有当前模式（列表）那枚有底色，实际有底色的是 {filled:?}"
        );

        // 「一个整体」：四枚按钮要被**同一个外框**包住（左右上下都有内衬）。
        //
        // 没有框时这四枚与右边的刷新 / 搜索按钮长得一模一样，读不出它们是**互斥**的
        // 一组（用户的回归现场就是「一排放着，各是各的」）。框画在 `view-mode-group`
        // 这个容器上，所以既断几何（包住 + 内衬 + 与地址栏胶囊等高），也断「它真的
        // 画了底色 / 描边」——只在容器上加 padding 而不画框是骗不过去的。
        let g = cx
            .debug_bounds("mo-view-mode-group")
            .expect("按钮组容器没渲染 —— 那个外框就是它画的");
        let (gx, gy) = (f32::from(g.origin.x), f32::from(g.origin.y));
        let (gw, gh) = (f32::from(g.size.width), f32::from(g.size.height));
        let (x0, y0) = (f32::from(bounds[0].origin.x), f32::from(bounds[0].origin.y));
        let (x1, y1) = (
            f32::from(bounds[3].origin.x) + f32::from(bounds[3].size.width),
            f32::from(bounds[3].origin.y) + f32::from(bounds[3].size.height),
        );
        let pad = crate::toolbar::GROUP_PAD - 0.51;
        assert!(x0 - gx >= pad, "外框左侧没内衬：按钮起点 {x0}、框起点 {gx}");
        assert!(
            gx + gw - x1 >= pad,
            "外框右侧没内衬：按钮终点 {x1}、框终点 {}",
            gx + gw
        );
        assert!(
            y0 - gy >= pad && gy + gh - y1 >= pad,
            "外框上下没内衬：按钮 y {y0}..{y1}、框 y {gy}..{}",
            gy + gh
        );
        assert!(
            (gh - crate::toolbar::GROUP_HEIGHT).abs() < 0.51,
            "按钮组高 {gh} ≠ {}（应与地址栏那枚胶囊等高）",
            crate::toolbar::GROUP_HEIGHT
        );

        let scale = cx.update(|window, _cx| window.scale_factor());
        let quads = cx.update(|window, _cx| window.painted_quads());
        let framed = quads.iter().any(|q| {
            let (qx, qy) = (q.bounds.origin.x.as_f32(), q.bounds.origin.y.as_f32());
            let (qw, qh) = (q.bounds.size.width.as_f32(), q.bounds.size.height.as_f32());
            // ⚠️ 别按色值找 quad（`background` 是 `pub(crate)`），按几何：这个 quad
            // 落在组容器的范围内、且几乎铺满它 —— 那就是外框的底色（描边同属它）。
            // 容差不取 0：背景绘制范围可能与布局 bounds 差 1px 边框。
            qx >= gx * scale - 1.0
                && qy >= gy * scale - 1.0
                && qx + qw <= (gx + gw) * scale + 1.0
                && qy + qh <= (gy + gh) * scale + 1.0
                && qw >= gw * scale * 0.8
                && qh >= gh * scale * 0.8
        });
        assert!(
            framed,
            "按钮组没画出外框（底色 / 描边），看起来还是四个裸图标"
        );

        // 切到网格：**第一帧不该已经到位** —— 到位就说明是瞬移，没有滑动过程。
        set_mode(cx, &root, crate::panel::ViewMode::Grid);
        let (_, in_flight, _) = measure(cx);
        assert!(
            !in_flight.contains(&"grid"),
            "切到网格的第一帧高亮就已经在新位置了（{in_flight:?}）—— 这是瞬移，不是滑动"
        );

        // 推时间让它落定：必须**精确**停在网格那枚上（同时钉住 `segment_offset`
        // 与按钮的实际排布一致）。
        settle_slide(cx);
        let (_, filled, indicator) = measure(cx);
        assert_eq!(
            filled,
            vec!["grid"],
            "滑动落定后高亮不在网格那枚上：{filled:?}"
        );
        assert!(
            aligned(&indicator, &bounds[1]),
            "落定后指示块 {indicator:?} 与网格那枚 {:?} 不重合",
            bounds[1]
        );

        // 最远的一段（列视图，位移 84px）也要落得准。
        set_mode(cx, &root, crate::panel::ViewMode::Columns);
        settle_slide(cx);
        let (_, filled, indicator) = measure(cx);
        assert_eq!(
            filled,
            vec!["columns"],
            "滑动落定后高亮不在列视图那枚上：{filled:?}"
        );
        assert!(
            aligned(&indicator, &bounds[3]),
            "落定后指示块 {indicator:?} 与列视图那枚 {:?} 不重合",
            bounds[3]
        );
    }

    /// 列表视图的行也要留出四周的呼吸空间：首行不顶表头、左右不贴窗口边缘。
    ///
    /// 与 `grid_and_gallery_inset_content_and_center_names` 同源——`uniform_list`
    /// 的 padding 四个方向都吃（top 加到条目起点、上下算进滚动内容高度、左右扣
    /// 可用宽度），所以留白挂在 list 上就够，不必每行自己加。
    /// 左右 12 / 上下 10（用户反馈上下各收 2px）。
    #[test]
    fn file_list_rows_are_inset_from_the_edges() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        cx.update(|_window, app| seed_window(&root, app, crate::panel::ViewMode::List));
        cx.update(|window, cx| window.render_frame(cx));

        let list = cx
            .debug_bounds("mo-file-list")
            .expect("列表视图没有渲染（view_mode 没切过去？）");
        let row = cx
            .debug_bounds("mo-file-row-0")
            .expect("第一行没有渲染（条目没进窗口快照？）");

        let left = f32::from(row.origin.x - list.origin.x);
        let top = f32::from(row.origin.y - list.origin.y);
        assert!(
            (left - 12.0).abs() < 0.51,
            "列表左侧留白 {left}，应为 12：行的 hover 底色顶到窗口边缘了"
        );
        assert!(
            (top - 10.0).abs() < 0.51,
            "列表顶部留白 {top}，应为 10：首行顶着表头了"
        );
    }

    /// 窗口快照还没落地时，列表画的是**斑马纹空行**，不是省略号。
    ///
    /// 用户报的现象：切到内容少的目录（或快速滚动）会整屏 `…` 闪一下。占位行改成
    /// 只有底色之后，真数据到位是文字直接浮现在同一块底色上，连底色都不跳。
    /// 这里钉三件事：占位行与数据行同高、同左留白（不会抖），相邻行底色交替
    /// （斑马纹占位生效），且这一帧里确实没有数据行（整片都是占位）。
    #[test]
    fn unfilled_rows_are_empty_zebra_placeholders() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        // 占位来自「快照还没回来」，渲染闭包会派生一次补窗任务（`ensure_window`）；
        // 真 IO 与 gpui 的确定性调度器混搭天生会偶发误报，开一次官方豁免（同
        // `tests/layout.rs::open_app`）。
        cx.dispatcher.allow_parking();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        // 快照一条都没有 → 可见区整片占位。path 指向不存在的目录：补窗任务取回
        // 空快照后窗口仍是空的（`visible_window` 对未加载目录直接返回空），
        // 所以这一帧是确定性的，不掺真实目录内容。
        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                let p = v.panel_mut();
                p.view_mode = crate::panel::ViewMode::List;
                p.path = Some(PathBuf::from("/mo-placeholder-test"));
                p.window_start = 0;
                p.window.clear();
                p.visible_count = 4;
                // 列表渲染的 item_count 用**行数**（分组开启时含组头）；
                // 这里无分组，行数 = 条目数。
                p.list_count = 4;
                cx.notify();
            })
        });
        cx.update(|window, cx| window.render_frame(cx));

        assert!(
            cx.debug_bounds("mo-file-row-0").is_none(),
            "窗口是空的却渲染出了数据行——这个用例的前提没了"
        );
        let list = cx.debug_bounds("mo-file-list").expect("列表视图没有渲染");
        let ph0 = cx
            .debug_bounds("mo-file-ph-0")
            .expect("第 0 条占位行没渲染");
        let ph1 = cx
            .debug_bounds("mo-file-ph-1")
            .expect("第 1 条占位行没渲染");
        let ph2 = cx
            .debug_bounds("mo-file-ph-2")
            .expect("第 2 条占位行没渲染");

        // 几何必须与数据行一致：同 24px 行高、同左留白（list 的 12pt padding）。
        // 占位期与加载完的几何差一点，看起来就是整块列表抖一下。
        let row_h = crate::listing::row_height(crate::panel::ViewMode::List);
        for (i, ph) in [ph0, ph1, ph2].iter().enumerate() {
            assert!(
                (f32::from(ph.size.height) - row_h).abs() < 0.51,
                "第 {i} 条占位行高 {}，应与数据行 {row_h} 一致（占位不能塌）",
                f32::from(ph.size.height)
            );
            assert!(
                (f32::from(ph.origin.x - list.origin.x) - 12.0).abs() < 0.51,
                "第 {i} 条占位行左缘距列表 {}，应为 12（与数据行同留白）",
                f32::from(ph.origin.x - list.origin.x)
            );
        }

        // 斑马纹：相邻两行不同色、隔一行同色。
        // ⚠️ 只比**相对**关系，不拿 `theme::zebra()` 的绝对色值比：调色板是进程级
        // 全局槽位，并行的主题用例会临时翻深色，比色值就是随机闪断。
        let scale = cx.update(|window, _cx| window.scale_factor());
        let rect = |b: gpui_kit::Bounds<gpui_kit::Pixels>| {
            (
                f32::from(b.origin.x) * scale,
                f32::from(b.origin.y) * scale,
                f32::from(b.size.width) * scale,
                f32::from(b.size.height) * scale,
            )
        };
        let quads = cx.update(|window, _cx| window.painted_quads());
        let bg_at = |r: (f32, f32, f32, f32)| {
            let near = |a: f32, b: f32| (a - b).abs() < 1.0;
            quads
                .iter()
                .find(|q| {
                    near(q.bounds.origin.x.as_f32(), r.0)
                        && near(q.bounds.origin.y.as_f32(), r.1)
                        && near(q.bounds.size.width.as_f32(), r.2)
                        && near(q.bounds.size.height.as_f32(), r.3)
                })
                .map(|q| q.background)
                .unwrap_or_else(|| panic!("占位行没有画出同位同尺寸的底色 quad：{r:?}"))
        };
        let (b0, b1, b2) = (bg_at(rect(ph0)), bg_at(rect(ph1)), bg_at(rect(ph2)));
        assert_ne!(
            b0, b1,
            "相邻两条占位行同色——斑马纹占位没生效（又退回一片空白了？）"
        );
        assert_eq!(
            b0, b2,
            "第 0 与第 2 条占位行不同色——交替规律断了（奇偶反了或多算了一行？）"
        );
    }

    /// 网格视图缺格时画的是**同尺寸的空骨架格**（不是 `…`）。
    #[test]
    fn unfilled_grid_cells_match_cell_geometry() {
        crate::isolate_config_for_tests();
        let mut cx = TestAppContext::single();
        cx.dispatcher.allow_parking();
        cx.update(gpui_kit::init);
        let app = AppState::new();
        let (root, cx) = cx.add_window_view(|_, cx| RootView::new(app, cx));
        let root = root.clone();

        // 只填第一格，其余缺格 → 第一格是数据单元、后面的都是骨架格。
        cx.update(|_window, cx| {
            root.update(cx, |v, cx| {
                let p = v.panel_mut();
                p.view_mode = crate::panel::ViewMode::Grid;
                p.path = Some(PathBuf::from("/mo-placeholder-grid-test"));
                p.window_start = 0;
                p.window = vec![mo_app::WindowRow::Entry(mo_core::Entry::new(
                    mo_core::FileId::new(0, 0),
                    "a".to_string(),
                    mo_core::EntryKind::Directory,
                    PathBuf::from("/mo-placeholder-grid-test/a"),
                ))];
                p.visible_count = 4;
                p.list_count = 4;
                cx.notify();
            })
        });
        cx.update(|window, cx| window.render_frame(cx));

        let cell = cx
            .debug_bounds("mo-grid-cell-0")
            .expect("第一格数据单元没渲染");
        let ph = cx
            .debug_bounds("mo-grid-ph-1")
            .expect("第 1 格骨架没渲染（缺格又退回不画了？）");
        assert!(
            cx.debug_bounds("mo-grid-ph-0").is_none(),
            "第 0 格有数据，不该画骨架格"
        );
        assert!(
            (f32::from(ph.size.height) - f32::from(cell.size.height)).abs() < 0.51,
            "骨架格高 {} 与真单元 {} 不一致：数据到达前后网格会跳",
            f32::from(ph.size.height),
            f32::from(cell.size.height)
        );
        assert!(
            (f32::from(ph.size.width) - f32::from(cell.size.width)).abs() < 0.51,
            "骨架格宽 {} 与真单元 {} 不一致：数据到达前后网格会跳",
            f32::from(ph.size.width),
            f32::from(cell.size.width)
        );
    }

    /// 图片预览在换上降采样副本之前，**必须先摘掉 `image`**。
    ///
    /// 不摘的后果正是本轮修的体验问题：翻页时窗口里还挂着上一张的图，用户按了方向键
    /// 看到的还是旧图、隔一拍才跳变。摘掉之后 `PreviewWindow` 才会走到占位分支画
    /// 「载入预览…」。非图片预览一次给全，第二拍必须为空——否则会白跑一次降采样
    /// （图片之外的类型没有可降采样的东西）。
    #[test]
    fn only_images_are_split_into_two_passes() {
        let photo = std::path::Path::new("/tmp/mo/photo.jpg");

        let image = mo_preview::Preview {
            kind: mo_preview::PreviewKind::Image,
            title: "photo.jpg".to_string(),
            text: None,
            image: Some(photo.to_path_buf()),
            size: 1024,
        };
        let (head, src) = super::split_preview_for_two_pass(image);
        assert!(
            matches!(&src, Some(super::PreviewSecond::Image(p)) if p == photo),
            "图片的源路径要交给第二拍去降采样：{src:?}"
        );
        assert!(
            head.image.is_none(),
            "第一拍必须不带图片路径，否则窗口会继续显示上一张图、永远走不到占位分支"
        );
        assert_eq!(
            head.kind,
            mo_preview::PreviewKind::Image,
            "类型不能变——占位分支就在图片这一支里"
        );
        assert_eq!(
            head.title, "photo.jpg",
            "标题照旧：占位期间标题不该闪成空白"
        );

        let text = mo_preview::Preview {
            kind: mo_preview::PreviewKind::Text,
            title: "notes.txt".to_string(),
            text: Some("hello".to_string()),
            image: None,
            size: 5,
        };
        let (head, src) = super::split_preview_for_two_pass(text);
        assert!(src.is_none(), "文本预览没有第二拍");
        assert_eq!(head.text.as_deref(), Some("hello"), "文本要原样留着");
    }

    /// 双栏对比的着色表：**只染有变化的那一侧**。
    ///
    /// 这条守的是双栏对比与「出一份差异报告」的分野——报告是中立的一列，
    /// 双栏是两侧各自的视角：左栏根本不存在右独有的那一行，染上去就是凭空
    /// 造记录；两侧都有但内容不同的才两边都染。深层差异不展开，只取直接子项。
    #[test]
    fn compare_maps_tint_each_side_from_its_own_view() {
        let (left, right) = (PathBuf::from("/L"), PathBuf::from("/R"));
        let cmp = mo_diff::TreeComparison {
            left: left.clone(),
            right: right.clone(),
            entries: vec![
                mo_diff::TreeEntry {
                    rel: PathBuf::from("only_left.txt"),
                    status: mo_diff::TreeStatus::LeftOnly,
                    is_dir: false,
                },
                mo_diff::TreeEntry {
                    rel: PathBuf::from("only_right.txt"),
                    status: mo_diff::TreeStatus::RightOnly,
                    is_dir: false,
                },
                mo_diff::TreeEntry {
                    rel: PathBuf::from("changed.txt"),
                    status: mo_diff::TreeStatus::Different,
                    is_dir: false,
                },
                // 深层差异：不进着色表（归到它所在的子目录上）。
                mo_diff::TreeEntry {
                    rel: PathBuf::from("sub/deep.txt"),
                    status: mo_diff::TreeStatus::LeftOnly,
                    is_dir: false,
                },
            ],
            identical_files: 0,
            identical_dirs: 0,
            different: 1,
            left_only: 2,
            right_only: 1,
        };
        let (lm, rm) = super::compare_maps(&cmp, &left, &right);

        assert_eq!(
            lm.get(&left.join("only_left.txt")),
            Some(&mo_diff::TreeStatus::LeftOnly)
        );
        assert!(
            !rm.contains_key(&right.join("only_left.txt")),
            "右栏没有这一行，不能染——染了就是凭空造一条不存在的记录"
        );
        assert_eq!(
            rm.get(&right.join("only_right.txt")),
            Some(&mo_diff::TreeStatus::RightOnly)
        );
        assert!(
            !lm.contains_key(&left.join("only_right.txt")),
            "同理，左栏也不该出现右独有的条目"
        );
        // 两侧都有但内容不同 → 两边都染。
        assert_eq!(
            lm.get(&left.join("changed.txt")),
            Some(&mo_diff::TreeStatus::Different)
        );
        assert_eq!(
            rm.get(&right.join("changed.txt")),
            Some(&mo_diff::TreeStatus::Different)
        );
        // 深层不上色：着色表只管直接子项。
        assert_eq!(lm.len(), 2, "左栏只有仅左有 + 内容不同两条");
        assert_eq!(rm.len(), 2, "右栏只有仅右有 + 内容不同两条");
        assert!(!lm.contains_key(&left.join("sub/deep.txt")));
    }

    /// 相同不染色：染了就等于把整屏都涂成「有差异」，差异反而看不见了。
    #[test]
    fn compare_tint_leaves_identical_rows_alone() {
        assert!(super::compare_tint(None).is_none(), "没在对比 → 不染");
        assert!(
            super::compare_tint(Some(mo_diff::TreeStatus::Identical)).is_none(),
            "相同不染：整屏都染就没差异可言了"
        );
        assert!(super::compare_tint(Some(mo_diff::TreeStatus::LeftOnly)).is_some());
        assert!(super::compare_tint(Some(mo_diff::TreeStatus::RightOnly)).is_some());
        assert!(super::compare_tint(Some(mo_diff::TreeStatus::Different)).is_some());
    }
}
