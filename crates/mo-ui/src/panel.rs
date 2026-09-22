//! panel：一个**浏览面板**（标签页）的全部状态。
//!
//! 多标签页 / 分栏之所以要先把这部分状态抽出来：每个标签页必须拥有独立的
//! 目录、导航栈、选择、过滤词与滚动位置。这里让每个标签页持有**自己的
//! `AppState`**——`AppState` 内部全是 `Arc`、且共用进程级 tokio runtime
//! （见 mo-app 的 `runtime()`），因此多开标签页不会额外堆线程，也**不必**
//! 把 `mo-app` 改造成「多会话」模型：独立性由构造保证，读写路径与单目录
//! 时完全一致。
//!
//! UI 侧同样不持有整份目录：每个面板只有可见区的窗口快照。

use std::ops::Range;
use std::path::PathBuf;

use gpui_kit::component::input::InputState;
use gpui_kit::{Entity, Subscription, UniformListScrollHandle};
use mo_app::AppState;
use mo_core::{Entry, LightEntry, SelectionModel, SortDir, SortKey};
use mo_operations::OperationHandle;

/// 面板的呈现方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// 表格列表（ 名称 / 大小 / 修改时间）。
    #[default]
    List,
    /// 网格：中等图标 + 文件名横向排列。
    Grid,
    /// 画廊：大缩略图 + 文件名。
    Gallery,
    /// 列视图：Miller 列，逐级展开当前选中目录。
    Columns,
}

impl ViewMode {
    pub fn label(&self) -> &'static str {
        match self {
            ViewMode::List => "列表",
            ViewMode::Grid => "网格",
            ViewMode::Gallery => "画廊",
            ViewMode::Columns => "列视图",
        }
    }

    /// 切换顺序（⌘1..4 或工具栏循环切换时使用）。
    pub fn next(&self) -> Self {
        match self {
            ViewMode::List => ViewMode::Grid,
            ViewMode::Grid => ViewMode::Gallery,
            ViewMode::Gallery => ViewMode::Columns,
            ViewMode::Columns => ViewMode::List,
        }
    }

    /// 配置文件里用的稳定键名（不要用中文标签，改文案会让用户配置失效）。
    pub fn key(&self) -> &'static str {
        match self {
            ViewMode::List => "list",
            ViewMode::Grid => "grid",
            ViewMode::Gallery => "gallery",
            ViewMode::Columns => "columns",
        }
    }

    /// 键名 → 视图模式；不认识返回 `None`（调用方回落默认，不让布局崩掉）。
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "list" => Some(ViewMode::List),
            "grid" => Some(ViewMode::Grid),
            "gallery" => Some(ViewMode::Gallery),
            "columns" => Some(ViewMode::Columns),
            _ => None,
        }
    }
}

/// 一个窗格：竖栏，内含若干标签页。
pub(crate) struct Pane {
    pub tabs: Vec<Panel>,
    pub active: usize,
}

impl Pane {
    pub fn new(first: Panel) -> Self {
        Self {
            tabs: vec![first],
            active: 0,
        }
    }

    /// 当前标签页（窗格至少有一个标签页，这里永不 panic）。
    pub fn panel(&self) -> &Panel {
        &self.tabs[self.active.min(self.tabs.len() - 1)]
    }

    /// 当前标签页的可变引用。
    pub fn panel_mut(&mut self) -> &mut Panel {
        let i = self.active.min(self.tabs.len() - 1);
        &mut self.tabs[i]
    }
}

/// 一个浏览面板 = 独立的 `AppState` + 它自己的 UI 快照。
pub(crate) struct Panel {
    /// 本面板的 app 状态（目录 / 导航 / 选择 / 操作的唯一事实来源）。
    pub app: AppState,
    /// 当前目录（窗口快照归属校验用：跨目录的取回结果不能互相覆盖）。
    pub path: Option<PathBuf>,
    /// 正在读取的目录（`AppState::opening_path` 的最近一次同步）。
    ///
    /// 与 `path` 的区别：`path` 是「已经显示出来的目录」，这个是「正在换过去的目标」。
    /// 读大目录要 100–300ms，这段窗口里界面还停在上一处——侧栏高亮与「正在读取 …」
    /// 提示都按它走，用户点了才有反应。
    pub opening: Option<PathBuf>,
    /// 可见条目总数（虚拟化列表的 `item_count`）。
    pub visible_count: usize,
    /// 窗口快照的起始下标。
    pub window_start: usize,
    /// 窗口快照：只覆盖可见区 + 少量缓冲。
    pub window: Vec<Entry>,
    /// 已发起但还没回填的窗口请求，避免重复派生任务。
    pub pending: Option<Range<usize>>,
    pub selection: SelectionModel,
    /// 本面板发起的后台操作快照（进度面板数据源）。
    pub ops: Vec<OperationHandle>,
    /// 输入即过滤的当前关键词。
    pub query: String,
    /// 文件列表滚动状态（跨帧存活才能记住滚动位置）。
    pub scroll: UniformListScrollHandle,
    pub can_back: bool,
    pub can_forward: bool,
    /// 呈现方式（列表 / 网格 / 画廊 / 列视图）。
    pub view_mode: ViewMode,
    /// 地址栏是否处于编辑态（Win11 式：点击空白 / 铅笔进入，Esc / 失焦退出）。
    pub address_editing: bool,
    /// 地址栏的**真实**文本输入状态（首次进入编辑时懒创建，之后复用）。
    ///
    /// 用框架的 `InputState` 而不是自绘一个「字符串 + 假光标」：选区、光标定位、
    /// 双击选词、⌘A、剪切 / 复制 / 粘贴、撤销、中文输入法全部由它提供，
    /// 自绘方案（只支持追加字符 + 退格）连「选中一段路径」都做不到。
    pub address: Option<Entity<InputState>>,
    /// `address` 的事件订阅句柄。
    ///
    /// ⚠️ 必须持有：gpui 的 `Subscription` 一旦 drop 就退订，
    /// 那样回车 / 失焦事件就再也收不到了。
    pub address_sub: Option<Subscription>,
    /// 列视图的各列数据（仅 `ViewMode::Columns` 下使用）。
    pub columns: Vec<ColumnData>,
    /// 列视图正在读盘：避免每帧重复发起加载任务。
    pub column_busy: bool,
    /// 当前排序方式（键 + 方向），来自 `AppState` 的最近一次同步。
    /// 表头据此画排序指示箭头；点击表头改的也是它（随后回灌 app）。
    pub sort: (SortKey, SortDir),
}

/// 列视图的一列。
#[derive(Debug, Clone)]
pub(crate) struct ColumnData {
    pub path: PathBuf,
    pub entries: Vec<LightEntry>,
    /// 本列当前高亮的行下标。
    pub cursor: usize,
}

impl Panel {
    pub fn new(app: AppState) -> Self {
        Self {
            app,
            path: None,
            opening: None,
            visible_count: 0,
            window_start: 0,
            window: Vec::new(),
            pending: None,
            selection: SelectionModel::new(),
            ops: Vec::new(),
            query: String::new(),
            scroll: UniformListScrollHandle::new(),
            can_back: false,
            can_forward: false,
            view_mode: ViewMode::default(),
            address_editing: false,
            address: None,
            address_sub: None,
            columns: Vec::new(),
            column_busy: false,
            sort: (SortKey::Name, SortDir::Asc),
        }
    }

    /// 套上默认视图模式（配置里的 `ui.view_mode`）。
    pub fn with_view_mode(mut self, mode: ViewMode) -> Self {
        self.view_mode = mode;
        self
    }

    /// 标签页标题：当前目录名；「此电脑」虚拟根显示固定名；尚未加载时为「新标签页」。
    pub fn title(&self) -> String {
        match &self.path {
            // 空路径 = 「此电脑」盘符列表（见 mo-fs 的 list_drives）。
            Some(p) if p.as_os_str().is_empty() => "此电脑".to_string(),
            // 盘符根（C:\）没有 file_name：剥掉尾部分隔符显示「C:」。
            // unix 根 `/` 剥完为空，回退显示原样。
            Some(p) if p.parent().is_none_or(|parent| parent == p) => {
                let trimmed = p
                    .to_string_lossy()
                    .trim_end_matches(['\\', '/'])
                    .to_string();
                if trimmed.is_empty() {
                    "/".to_string()
                } else {
                    trimmed
                }
            }
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.to_string_lossy().to_string()),
            None => "新标签页".to_string(),
        }
    }

    /// 标签页上的远程徽标：正在浏览远程时给出 `host[:port]`，本地为 `None`。
    ///
    /// 用户要求「访问 FTP 时标签页上要显示出来」。判据抽在这里（徽标怎么画——
    /// 地球图标 + 这个串——在 `render_tab_bar`），是因为这一半能单测，
    /// 而「标签上有没有画出那个图标」headless 下读不出文本、测不了。
    ///
    /// 只看**当前是否在浏览远程**：切回本地后会话可能还活着（`open_local` 不断开），
    /// 但那时标签页显示的是本地目录，不该再挂远程徽标。
    pub fn remote_badge(&self) -> Option<String> {
        self.app.remote_url().map(|u| u.authority())
    }

    /// 窗口快照是否完整覆盖 `[start, end)`。
    pub fn covered(&self, start: usize, end: usize) -> bool {
        !self.window.is_empty()
            && start >= self.window_start
            && end <= self.window_start + self.window.len()
    }
}
