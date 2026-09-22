//! mo-app：应用层。
//!
//! 把 UI / 导航 / 状态 / 事件 / 用户交互 与 文件系统 / 操作 / 缓存 解耦。
//! UI 只通过 [`AppState`] 发出命令（打开目录、导航、选择、提交操作），
//! 并通过 [`mo_core::EventBus`] 订阅状态变化，绝不直接操作文件系统。
//!
//! ```text
//! UI → AppState → DirectoryController → DirectoryModel
//!                              │
//!                              ▼
//!                         FileSystemService → Platform
//! ```
//!
//! 关键原则：**mo-core 不依赖 GPUI**，核心文件系统逻辑可单独测试。

mod controller;
/// 远程服务器的凭据存取（系统钥匙串）。
mod credentials;
/// 扩展系统：声明式清单 + 外部程序。
pub mod extensions;
mod metadata;
/// 系统 shell 集成（默认打开 / 打开方式）。
pub mod shell;
mod thumbnail;
/// 用户自定义命令（占位符 / 清单 / 执行）。
pub mod usercmds;
/// 自动化工作流：多步命令顺序执行。
pub mod workflows;

pub use controller::DirectoryController;
pub use metadata::MetadataScheduler;
// 配置类型经应用层再导出：UI 只依赖 mo-app，不直接抓 mo-config。
pub use mo_config::{
    ColumnPrefs, Config, SavedServer, ThemeColors, UiPrefs, UserCommand, Workflow,
};
pub use thumbnail::ThumbnailScheduler;
pub use workflows::{run_workflow, StepResult, WorkflowReport};

use std::future::Future;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mo_cache::MetadataCache;
use mo_core::{
    AppEvent, Directory, Entry, EventBus, FileId, FileMetadata, LightEntry, MetadataState, MoError,
    NavigationState, SelectionModel, SortDir, SortKey, ThumbnailState,
};
use mo_fs::{entry_at, FileSystem, FileSystemWatcher, LocalFileSystem, WatcherEvent};
use mo_operations::{
    CopyOperation, LinkKind, LinkOperation, MoveOperation, OperationHandle, OperationManager,
    RenameOperation, RestoreOperation, SharedOperation, Trash, TrashEntry, TrashOperation,
};
use mo_preview::Preview;
use mo_remote::RemoteUrl;
use mo_search::{crawl, FileIndex, SearchHit};
use parking_lot::Mutex as PlMutex;
use tokio::sync::{Mutex, RwLock};

/// 应用可变状态（全部放在 RwLock 内，便于 UI 与后台任务并发访问）。
pub struct AppStateInner {
    pub navigation: NavigationState,
    pub selection: SelectionModel,
    pub directory: Option<Directory>,
    /// UI 当前可见的行范围：元数据加载与缩略图生成都按它排优先级。
    pub visible_range: Range<usize>,
}

/// Mo 应用状态。 cheap clone（内部均为 `Arc`）。
#[derive(Clone)]
pub struct AppState {
    inner: Arc<RwLock<AppStateInner>>,
    /// 本标签页在看哪一边：本地，还是注册表里的某一条远程会话。
    ///
    /// 这里**只有「哪一条」**，连接对象本身活在 [`SessionRegistry`] 里——那个比标签页
    /// 活得久（用户要的是「关标签页不断开，只有退出应用才断开」）。用一个
    /// `std::sync::Mutex` 而不是 tokio 的：只在切来源那一瞬间写，读也极短（取一个
    /// id），而且 `active_fs()` 会在非异步上下文被调用。与 mo-app 的共享 tokio runtime
    /// 解耦也靠它（FTP 连接自带一份 runtime，`block_on` 不能在共享 runtime 上做）。
    source: Arc<std::sync::Mutex<Source>>,
    /// 进程级会话注册表。
    ///
    /// 做成字段（而不是每次调全局函数）是为了让测试能塞一份独占的进来：并行跑的
    /// 用例共享进程级单例会互相串味（一个用例装上的假服务器，另一个也看得见）。
    sessions: Arc<SessionRegistry>,
    ops: Arc<Mutex<OperationManager>>,
    bus: EventBus,
    scheduler: MetadataScheduler,
    thumbs: ThumbnailScheduler,
    /// 元数据缓存（打开失败时降级为 `None`，功能不受影响）。
    cache: Option<Arc<MetadataCache>>,
    watcher: Arc<Mutex<Option<FileSystemWatcher>>>,
    /// 「模型已变化、UI 该刷新了」的合并标记。
    ///
    /// 元数据是逐条回填的，一万条目就是一万次变更；若每变一条就广播一次，
    /// UI 会被刷爆。这里只置位，由刷新泵按固定节拍合并成一次广播。
    dirty: Arc<AtomicBool>,
    /// 后台泵是否该收工（关标签页时置位）。见 [`AppState::stop_pumps`]。
    stopped: Arc<AtomicBool>,
    /// 全局搜索索引（内存 SQLite）。跨目录搜索的数据源。
    index: Arc<PlMutex<FileIndex>>,
    /// 「系统里挂了哪些网络盘」的缓存：`(上次查的时刻, 结果)`。
    ///
    /// 侧边栏**每帧**都会问一次，而发现是真要去读 `/proc/mounts` 或跑一次 `mount`
    /// 子进程——每帧一次的子进程会把 UI 拖死。挂载 / 卸载这种事几秒的延迟无所谓，
    /// 所以按 [`NET_SHARE_TTL`] 缓存（挂载 / 卸载后立刻作废）。
    net_shares: Arc<std::sync::Mutex<(std::time::Instant, Vec<mo_remote::mount::NetworkShare>)>>,
    /// 「本机挂了哪些卷宗」的缓存（`mountedVolumeURLs` / 读 `/Volumes`）。
    ///
    /// 与 [`AppState::net_shares`] 同一条纪律：侧边栏每帧都问，而卷宗列表要
    /// `statfs` 逐个查或调 AppKit，缓存 [`VOLUME_TTL`] 省得每帧抖一下。挂载 / 卸载
    /// 后立刻作废。
    volumes_cache: Arc<std::sync::Mutex<(std::time::Instant, Vec<mo_platform::Volume>)>>,
    /// 系统文件图标缓存：路径 → 写好的 PNG 缓存文件路径。
    ///
    /// 列表每行都要图标，而「问系统拿图标」（`NSWorkspace.iconForFile:` → 转 PNG）
    /// 是**重**操作——万条目目录每帧都问会卡死 UI。所以首见才真去问、之后命中。
    /// 缓存目录在 `temp_dir()/mo-icons`，PNG 文件让 GPUI 的 `img()` 直接加载
    /// （光栅图没法像内置 SVG 那样用文字色描边）。容量封顶，超出整清空重来。
    file_icon_cache: Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>>,
    /// 中断后台索引爬取的开关。
    index_stop: Arc<AtomicBool>,
    /// 操作历史（轻量环形日志，供「操作历史」面板展示）。
    history: Arc<PlMutex<Vec<HistoryEntry>>>,
    /// 回收站（删除走回收站，故删除可撤销）。
    trash: Arc<Trash>,
    /// 撤销栈：最近执行的「可撤销操作」在栈顶。
    undo_stack: Arc<PlMutex<Vec<Reversible>>>,
    /// 重做栈：被撤销的操作暂存于此，重做后回到撤销栈。
    redo_stack: Arc<PlMutex<Vec<Reversible>>>,
    /// 应用内剪贴板（⌘C / ⌘X / ⌘V 的文件复制与剪切）。
    clipboard: Arc<Mutex<Option<Clipboard>>>,
}

/// Mo 的后台 runtime：进程级共享，永不释放。
///
/// **不能**把 `Runtime` 的所有权交给 `AppState`：`AppState` 会被大量 clone 进后台任务，
/// 最后一个克隆一旦在异步上下文里被 drop，tokio 会直接 panic
/// （"Cannot drop a runtime in a context where blocking is not allowed"）。
/// 用静态引用既避免了这个陷阱，也省掉了多套 runtime 的线程开销。
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            // 4 个 worker：大目录回填会产生大量后台任务，
            // worker 太少时 UI 的窗口取回会排在积压后面（滚动闪烁的成因之一）。
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

/// 一台服务器最多记住多少条（更早的滚掉）。
const SAVED_SERVERS_MAX: usize = 20;

/// 当前 unix 时间戳（秒）。取不到（系统时钟早于 1970）就当 0。
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// 「连接到服务器」的失败结果。
///
/// 拆成两种而不是压成一句字符串，是因为 UI 的处置完全不同：要凭据就弹认证框，
/// 其余直接在对话框里显示错误。改造前一切都被压成 `String`，「密码不对」和
/// 「主机名打错」在 UI 上长得一模一样，没法分别处置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectFailure {
    /// 直接显示给用户的错误（地址写错 / 协议不支持 / 连不上 / 超时…）。
    Message(String),
    /// 服务器要账号密码：匿名登录被拒，或上次给的凭据不对。
    NeedsCredentials {
        /// 要连的服务器（`scheme://host:port`），认证框用来显示与重连。
        endpoint: String,
        /// 这次尝试用的用户名（地址里写的，或钥匙串里存过的）。
        ///
        /// 带出来是为了让认证框**初值**就是它——用户多半只需要补 / 改密码。
        user: String,
        /// 上次尝试的失败原因，作为认证框的初始提示。
        detail: String,
    },
}

impl std::fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectFailure::Message(msg) => write!(f, "{msg}"),
            // 这条通常由 UI 单独处置（弹认证框），但日志与兜底提示也得能打印它。
            ConnectFailure::NeedsCredentials {
                endpoint, detail, ..
            } => {
                write!(f, "{endpoint} 需要账号密码：{detail}")
            }
        }
    }
}

/// 一条活着的远程会话的编号。
///
/// 会话活在 [`SessionRegistry`] 里，界面只拿这个编号指代「哪一条连接」：侧边栏按它
/// 列出多条、点哪条切哪条、按哪个图标断开哪条。
pub type SessionId = u64;

/// 一条活着的远程会话。
///
/// 连接对象（FTP 的 socket、它自带的 runtime）就活在这里。**它不随标签页消失**：
/// 会话挂在注册表上，关标签页只是不再有人看它，连接照旧。终点只有一个——
/// 显式「断开」（[`AppState::disconnect_connection`]），或进程退出。
#[derive(Clone)]
struct RemoteSession {
    id: SessionId,
    /// 连接时的地址（含登录用户名；密码在 `display()` 里已被刻意丢掉）。
    url: RemoteUrl,
    /// 这条会话的后端，即上层的连接对象。
    fs: Arc<dyn FileSystem>,
    /// 这条会话最后待过的远程目录。
    ///
    /// 切回本地再切回来时回到这里，而不是每次都掉回根目录——「会话还活着」的
    /// 体感一半来自不重登，另一半来自位置还在。
    path: String,
    /// 最近一次**确认这条连接还能用**的时刻（登入算一次，读目录成功 / 探活通过
    /// 各刷新一次）。
    ///
    /// 用途：只有闲置超过 [`IDLE_PROBE`] 才在下次读目录前探活——探活是一次网络
    /// 往返，连续浏览时不该每次进目录都白付。
    last_used: std::time::Instant,
}

/// 活着的连接一览（侧边栏「远程」区按这个列表渲染）。
#[derive(Clone, Debug)]
pub struct LiveConnection {
    /// 编号：点它 / 断开它时用来指代这条连接。
    pub id: SessionId,
    /// 连接地址（含用户名；密码不回显）。
    pub url: RemoteUrl,
}

/// 进程级会话注册表：远程连接活在这里，而不是活在某一个标签页里。
///
/// 用户要的语义是「切回本地不断开、**关标签页也不断开**，只有退出应用才断开」，而每个
/// 标签页各自持有一份 `AppState`——会话若挂在 `AppState` 上，就会被标签页一起带走。
/// 所以连接上移到这里，`AppState` 只记「我看的是哪一条」。注册表是进程级的
/// （[`session_registry`]），于是新开的标签页天然看得见已经连着的服务器。
pub struct SessionRegistry {
    sessions: std::sync::Mutex<Vec<RemoteSession>>,
    /// 编号分配器：从 1 起，**永不复用**——断开又新建时，界面手里那个旧编号
    /// 不会认到新连接上。
    next_id: AtomicU64,
    /// 怎么建连接。默认 [`mo_remote::connect`]。
    ///
    /// 做成字段（而不是就地调 `mo_remote::connect`）是为了让「断了会重连」这件事
    /// **可测**：测试没法真去连一台 FTP，但能验「重连被调用了，而且换了新连接」。
    connector: Connector,
}

/// 建一条远程连接的方式（见 [`SessionRegistry::connector`]）。
///
/// 公开是因为它出现在 [`SessionRegistry::with_connector`] 的签名里，而该方法的
/// 调用方（测试）在另一个 crate——拿不到类型名就只能把整个签名抄一遍。
pub type Connector =
    Arc<dyn Fn(&RemoteUrl) -> Result<Arc<dyn FileSystem>, mo_remote::RemoteError> + Send + Sync>;

impl Default for SessionRegistry {
    fn default() -> Self {
        let connector: Connector = Arc::new(|url: &RemoteUrl| mo_remote::connect(url));
        Self {
            sessions: std::sync::Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            connector,
        }
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// ⚠️ 仅供测试：换一个「建连接」的实现。
    #[doc(hidden)]
    pub fn with_connector(connector: Connector) -> Self {
        Self {
            connector,
            ..Self::default()
        }
    }

    fn connector(&self) -> Connector {
        self.connector.clone()
    }

    /// 刷新「上次确认能用」的时刻（读目录成功 / 探活通过）。
    fn touch(&self, id: SessionId) {
        if let Some(s) = self
            .sessions
            .lock()
            .unwrap()
            .iter_mut()
            .find(|s| s.id == id)
        {
            s.last_used = std::time::Instant::now();
        }
    }

    /// ⚠️ 仅供测试：把这条会话的「上次确认能用」往前拨，模拟「闲置了很久」。
    ///
    /// 生产代码里这个时刻只由真实使用推进（见 [`Self::touch`] / [`Self::set_fs`]）；
    /// 测试没法为了「闲置超时」真等 30 秒。
    #[doc(hidden)]
    pub fn age_for_test(&self, id: SessionId, by: std::time::Duration) {
        if let Some(s) = self
            .sessions
            .lock()
            .unwrap()
            .iter_mut()
            .find(|s| s.id == id)
        {
            s.last_used = s.last_used.checked_sub(by).unwrap_or(s.last_used);
        }
    }

    /// 原地换掉这条会话的连接对象（重连用）：**编号不变**，所以侧边栏那一行不闪、
    /// 高亮也不会跳。
    fn set_fs(&self, id: SessionId, fs: Arc<dyn FileSystem>) {
        self.mutate(id, |s| s.fs = fs);
    }

    /// 原地换掉连接对象**和**地址（用户在同一台服务器上换了密码时走这条）。
    ///
    /// 只更新 `url`、不新增会话：同端点同用户名还是那一行，只是往后「切回来 /
    /// 再重连」用的是新凭据——否则下次闲置重连又会被那个旧密码顶回来。
    fn set_url_and_fs(&self, id: SessionId, url: RemoteUrl, fs: Arc<dyn FileSystem>) {
        self.mutate(id, |s| {
            s.url = url;
            s.fs = fs;
        });
    }

    /// 改一条会话，并顺手刷新「上次确认能用」的时刻（刚换过连接，算刚确认）。
    fn mutate(&self, id: SessionId, f: impl FnOnce(&mut RemoteSession)) {
        if let Some(s) = self
            .sessions
            .lock()
            .unwrap()
            .iter_mut()
            .find(|s| s.id == id)
        {
            f(s);
            s.last_used = std::time::Instant::now();
        }
    }

    /// 登入一条新会话，返回它的编号。
    ///
    /// 顺手摘掉同 key（端点 + 用户名）的旧条目，给「(端点, 用户名) 至多一条」这条
    /// 不变式兜底：正常路径由 [`Self::find`] 先拦住（找得到就在原编号上收场，压根
    /// 走不到这儿），这里再保一道——万一漏进来一条，侧边栏也不该多出一行。
    fn add(&self, url: RemoteUrl, fs: Arc<dyn FileSystem>) -> SessionId {
        let endpoint = url.endpoint();
        let mut list = self.sessions.lock().unwrap();
        list.retain(|s| s.url.endpoint() != endpoint || s.url.user != url.user);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        list.push(RemoteSession {
            id,
            url,
            fs,
            path: "/".to_string(),
            last_used: std::time::Instant::now(),
        });
        id
    }

    /// 已登入的**同一条**连接：端点 + 用户名都对得上就是它。
    ///
    /// 注册表的不变式是「(端点, 用户名) 至多一条」——侧边栏「远程」区一行就是一条
    /// 连接，同一台服务器同一个账号连两次不该长出两行。所以这里**不看密码**：
    /// 密码不同不是「另一条连接」，而是同一条要换凭据，
    /// [`AppState::finish_connect`] 会在**原来那个编号**上把连接换掉。
    ///
    /// 见 [`RemoteUrl::endpoint`]：端点里不含用户名，所以要两个键分别比。
    fn find(&self, url: &RemoteUrl) -> Option<RemoteSession> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.url.endpoint() == url.endpoint() && s.url.user == url.user)
            .cloned()
    }

    /// 取一条会话的副本（`None` = 已经断开了）。
    fn entry(&self, id: SessionId) -> Option<RemoteSession> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    fn fs_of(&self, id: SessionId) -> Option<Arc<dyn FileSystem>> {
        self.entry(id).map(|s| s.fs)
    }

    fn url_of(&self, id: SessionId) -> Option<RemoteUrl> {
        self.entry(id).map(|s| s.url)
    }

    fn path_of(&self, id: SessionId) -> Option<String> {
        self.entry(id).map(|s| s.path)
    }

    fn set_path(&self, id: SessionId, path: String) {
        if let Some(s) = self
            .sessions
            .lock()
            .unwrap()
            .iter_mut()
            .find(|s| s.id == id)
        {
            s.path = path;
        }
    }

    /// 断开一条：从表里摘掉，连接对象随之关闭。
    fn remove(&self, id: SessionId) -> bool {
        let mut list = self.sessions.lock().unwrap();
        let before = list.len();
        list.retain(|s| s.id != id);
        list.len() != before
    }

    /// 活着的连接一览（按建立顺序）。
    fn snapshot(&self) -> Vec<LiveConnection> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .map(|s| LiveConnection {
                id: s.id,
                url: s.url.clone(),
            })
            .collect()
    }

    /// 还有几条活着。
    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 会话闲置超过这个时长，下次读目录前才值得探活。
///
/// 探活是一次网络往返：连续浏览（每次进目录间隔几秒）不该白付这次往返，而
/// 「闲置」正是服务器掐断连接的时机。判据是 [`RemoteSession::last_used`]。
const IDLE_PROBE: std::time::Duration = std::time::Duration::from_secs(30);

/// 网络盘发现的缓存有效期（见 `AppState::net_shares`）。
const NET_SHARE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// 卷宗列表的缓存有效期（见 `AppState::volumes_cache`）。
const VOLUME_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// 本进程的会话注册表。
///
/// 与共享 tokio runtime 同理：会话的生命周期就是进程的生命周期——「只有退出应用才
/// 断开」就是它。`AppState::new` / `with_trash` 默认取这一份，所以每个标签页看到的是
/// 同一张表；测试要隔离就用 [`AppState::with_sessions`]。
pub fn session_registry() -> Arc<SessionRegistry> {
    static REG: std::sync::OnceLock<Arc<SessionRegistry>> = std::sync::OnceLock::new();
    REG.get_or_init(|| Arc::new(SessionRegistry::new())).clone()
}

/// 本标签页在看哪一边。
///
/// 会话本身不在这里（见 [`SessionRegistry`]），这里只剩「看的是哪一条」。
#[derive(Default, Clone)]
struct Source {
    /// 本标签页看着的那条会话。
    ///
    /// **切回本地不清它**：`on_remote` 才是「现在在哪」。留着这个编号，点回远程
    /// 时才知道该回到哪一条（见 [`AppState::open_remote`]）。
    active: Option<SessionId>,
    /// 当前生效的是不是那条会话（`false` = 本地）。
    on_remote: bool,
}

impl AppState {
    /// 默认回收站根目录：用户主目录下的 `.mo-trash`。
    fn default_trash_root() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".mo-trash")
    }

    /// 以默认回收站（~/.mo-trash）构造。
    pub fn new() -> Self {
        Self::with_trash(Self::default_trash_root())
    }

    /// 以指定回收站根目录构造（测试可传入临时目录以保持隔离）。
    ///
    /// 会话表用**进程级**那一份：同一进程里的所有标签页共享同一批远程连接，
    /// 这正是「关标签页不断开」的实现基础。
    pub fn with_trash(trash_root: PathBuf) -> Self {
        Self::build(trash_root, session_registry())
    }

    /// ⚠️ 仅供测试 / 需要显式指定会话表时用。
    ///
    /// 传一份**独占的** [`SessionRegistry`] 就是隔离（并行跑的用例共享进程级单例会
    /// 互相串味：一个用例装上的假服务器，另一个也看得见）；传同一个 `Arc` 给两个
    /// `AppState`，就等于两个标签页——这正是「关标签页不断开」的验证方式。
    #[doc(hidden)]
    pub fn with_sessions(trash_root: PathBuf, sessions: Arc<SessionRegistry>) -> Self {
        Self::build(trash_root, sessions)
    }

    /// 以指定回收站与会话表构造（两个公开构造器共用）。
    fn build(trash_root: PathBuf, sessions: Arc<SessionRegistry>) -> Self {
        let cache = match MetadataCache::open_default() {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                tracing::warn!("元数据缓存不可用，将以无缓存模式运行：{e}");
                None
            }
        };
        let trash = Arc::new(Trash::new(trash_root).expect("failed to init trash"));
        Self {
            inner: Arc::new(RwLock::new(AppStateInner {
                navigation: NavigationState::new(),
                selection: SelectionModel::new(),
                directory: None,
                visible_range: 0..0,
            })),
            source: Arc::new(std::sync::Mutex::new(Source::default())),
            sessions,
            ops: Arc::new(Mutex::new(OperationManager::new())),
            bus: EventBus::new(),
            scheduler: MetadataScheduler::new(),
            thumbs: ThumbnailScheduler::new(),
            cache,
            watcher: Arc::new(Mutex::new(None)),
            dirty: Arc::new(AtomicBool::new(false)),
            index: Arc::new(PlMutex::new(
                FileIndex::open_in_memory().expect("open index"),
            )),
            net_shares: Arc::new(std::sync::Mutex::new((
                // 起点放到「很久以前」，好让第一次问就真的去查一次。
                std::time::Instant::now() - NET_SHARE_TTL,
                Vec::new(),
            ))),
            volumes_cache: Arc::new(std::sync::Mutex::new((
                std::time::Instant::now() - VOLUME_TTL,
                Vec::new(),
            ))),
            file_icon_cache: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            index_stop: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            history: Arc::new(PlMutex::new(Vec::new())),
            trash,
            undo_stack: Arc::new(PlMutex::new(Vec::new())),
            redo_stack: Arc::new(PlMutex::new(Vec::new())),
            clipboard: Arc::new(Mutex::new(None)),
        }
    }

    /// 在 Mo 自带的 tokio runtime 上派发一个后台任务。
    ///
    /// **不要**直接用 `tokio::spawn`：GPUI 有自己的执行器（后台线程 + 主线程），
    /// 从 GPUI 任务里调 `tokio::spawn` 会因当前线程没有 runtime 上下文而 panic。
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        runtime().spawn(future)
    }

    /// 在 blocking 池执行阻塞任务（目录读取、图片解码、SQLite 读写）。
    ///
    /// 同样是走 Mo 自带的 runtime，因此不依赖调用方线程是否处于 tokio 上下文。
    pub fn spawn_blocking<F, R>(&self, f: F) -> tokio::task::JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        runtime().spawn_blocking(f)
    }

    /// 事件总线（UI / 后台任务订阅状态变化）。
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// 本 `AppState` 的后台泵是否该收工了。
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    /// 让本 `AppState` 的后台泵退出（关标签页时调用，见 `RootView::close_tab`）。
    ///
    /// 泵各自握着一份 `AppState` 克隆并在 `loop` 里永不返回，所以「标签页被移出
    /// 列表」并不会释放它。置位后各泵在下一轮自退。
    ///
    /// 远程连接**不在这里收尾**：会话活在 [`SessionRegistry`] 里，关标签页不断开
    /// （用户要的语义），只有 [`AppState::disconnect_connection`] 与进程退出才关。
    pub fn stop_pumps(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    /// 底层文件系统抽象（当前生效的，可能是远程连接）。
    pub fn file_system(&self) -> Arc<dyn FileSystem> {
        self.active_fs()
    }

    /// 当前生效的底层文件系统（本地，或**正在浏览**的那条远程会话）。
    ///
    /// 会话在浏览期间被别处断开时回落本地：绝不返回一条已经关掉的连接。
    fn active_fs(&self) -> Arc<dyn FileSystem> {
        self.active_session()
            .and_then(|id| self.sessions.fs_of(id))
            .unwrap_or_else(|| Arc::new(LocalFileSystem))
    }

    /// 本标签页当前生效的会话编号（在看本地、或那条会话已被断开时为 `None`）。
    fn active_session(&self) -> Option<SessionId> {
        let src = self.source.lock().unwrap();
        if src.on_remote {
            src.active
        } else {
            None
        }
    }

    /// 操作管理器（提交 / 取消文件操作）。
    pub fn operations(&self) -> Arc<Mutex<OperationManager>> {
        self.ops.clone()
    }

    /// 元数据调度器（后台并发加载元数据）。
    pub fn scheduler(&self) -> &MetadataScheduler {
        &self.scheduler
    }

    /// 缩略图调度器（只为可见区生成缩略图）。
    pub fn thumbs(&self) -> &ThumbnailScheduler {
        &self.thumbs
    }

    /// 元数据缓存（无缓存模式时为 `None`）。
    pub fn cache(&self) -> Option<Arc<MetadataCache>> {
        self.cache.clone()
    }

    /// 读取目录并更新模型与监听目标（**不触碰导航栈**）。
    ///
    /// 打开 / 前进 / 后退 / 刷新共用这一条路径，导航栈的维护交给调用方：
    /// 否则 `go_back` 之后再 `visit` 一次，会把回来的位置又压进后退栈、
    /// 并清空前进栈，导致前进按钮失效。
    ///
    /// 大目录优化的关键都在这里：
    /// * `read_dir` 是阻塞 IO，连同**排序**一起放进 blocking 池，主线程只做一次写入；
    /// * 打开瞬间先用缓存把元数据填满（stale-while-revalidate），再后台逐条校验；
    /// * 后台校验按「首屏优先」排队，用户看到的行最先补全。
    async fn load_path(&self, path: &Path) -> Result<(), MoError> {
        // 远程会话闲置久了会被服务器单方面掐断（FTP 的 `idle_session_timeout` 很常见），
        // 这时直接读目录只会拿到一句 `Broken pipe (os error 32)`。所以先确认连接还在，
        // 断了就用会话里记着的凭据原地重连（见 `revive_if_stale`）——这条路径是所有
        // 读目录的必经之地（进目录 / 刷新 / 前进后退），自愈放在这里最省事。
        let session = self.active_session();
        if let Some(id) = session {
            self.revive_if_stale(id)
                .await
                .map_err(|e| MoError::Other(e.to_string()))?;
        }

        let mut fs = self.active_fs();
        let p = path.to_path_buf();
        let cache = self.cache();
        let dir_id = FileId::synthetic(path);

        // 阻塞部分：读目录 + 缓存预填 + 排序，全部在 blocking 线程完成。
        //
        // 这里是个「最多两轮」的循环：连接死在**读的那一瞬间**（探活时还活着）时
        // 重连再读一次——网络抖一下不该变成用户脸上的一条报错。
        let mut retried = false;
        let (dir, for_verify) = loop {
            let fs_task = fs.clone();
            let p_task = p.clone();
            let cache_task = cache.clone();
            let outcome = self
                .spawn_blocking(move || -> Result<(Directory, Vec<Entry>), MoError> {
                    let raw = fs_task.read_dir_blocking(&p_task)?;
                    let mut dir = Directory::new(dir_id, p_task);
                    dir.set_entries(
                        raw.into_iter()
                            .map(|r| Entry::new(r.id, r.name, r.kind, r.path))
                            .collect(),
                    );
                    if let Some(c) = cache_task.as_ref() {
                        let hits = MetadataScheduler::prime_from_cache(c, &mut dir.entries);
                        if hits > 0 {
                            tracing::debug!("元数据缓存命中 {hits}/{} 条", dir.entries.len());
                        }
                    }
                    dir.loading = false;
                    dir.rebuild_view();
                    // 后台校验需要一份条目副本（在 blocking 线程里克隆，不占异步 worker）。
                    let for_verify = dir.entries.clone();
                    Ok((dir, for_verify))
                })
                .await
                .map_err(|e| MoError::Other(format!("读取目录的任务失败：{e}")))?;
            match outcome {
                Ok(v) => break v,
                Err(e) if !retried && session.is_some() && mo_remote::is_disconnected(&e) => {
                    retried = true;
                    let id = session.expect("上面判过 `is_some`");
                    tracing::warn!("读目录时连接已断，重连后重试一次：{e}");
                    self.reconnect(id)
                        .await
                        .map_err(|f| MoError::Other(f.to_string()))?;
                    // 注册表里那条连接已经换成新的了，重新取一份。
                    fs = self.active_fs();
                }
                Err(e) => return Err(e),
            }
        };

        let first_screen = dir.visible_count().min(200);

        {
            let mut inner = self.inner.write().await;
            let mut sel = inner.selection.clone();
            sel.clear();
            inner.directory = Some(dir);
            inner.selection = sel;
        }

        // 记下这条会话待过的地方：切回本地再回来时回到同一层（见 `use_session`）。
        // 只在真读成功之后记，路径打错不该覆盖上次的位置。
        //
        // 顺带刷新「这条连接还能用」的时刻：能走到这里说明刚才那次目录读取真的成功了，
        // 下一次读之前就不必再探活（见 `revive_if_stale`）。
        if let Some(id) = self.active_session() {
            self.sessions
                .set_path(id, path.to_string_lossy().to_string());
            self.sessions.touch(id);
        }

        // 切换监听目标：新目录替换旧 watcher，旧 watcher 被 drop 即停止监听。
        {
            let mut w = self.watcher.lock().await;
            *w = FileSystemWatcher::watch(path).ok();
        }

        // 首屏优先校验：缓存里的值可能已过期，但用户看到的行必须最先准确。
        self.scheduler
            .load(self.clone(), for_verify, Some(0..first_screen));
        self.bus.publish(AppEvent::DirectoryChanged {
            path: path.to_path_buf(),
        });
        Ok(())
    }

    /// 打开目录：记录导航历史 + 读取目录 + 广播事件。
    pub async fn open_directory(&self, path: &Path) -> Result<(), MoError> {
        {
            let mut inner = self.inner.write().await;
            inner.navigation.visit(path.to_path_buf());
        }
        self.load_path(path).await?;
        self.bus.publish(AppEvent::NavigationChanged {
            path: path.to_path_buf(),
        });
        Ok(())
    }

    /// 打开父目录。
    pub async fn open_parent(&self) -> Result<(), MoError> {
        let current = self.inner.read().await.navigation.current.clone();
        if let Some(p) = current {
            // Windows：盘符根（C:\）的上一级是「此电脑」虚拟根（空路径哨兵，
            // 见 mo-fs 的 list_drives），与资源管理器行为一致。
            #[cfg(target_os = "windows")]
            if !p.as_os_str().is_empty() && p.parent().is_none_or(|parent| parent == p) {
                return self.open_directory(Path::new("")).await;
            }
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    return self.open_directory(parent).await;
                }
            }
        }
        Ok(())
    }

    /// 用系统默认应用打开文件（资源管理器双击语义）。
    pub async fn open_with_system(&self, path: &Path) -> Result<(), String> {
        let p = path.to_path_buf();
        self.spawn_blocking(move || shell::open_default(&p))
            .await
            .map_err(|e| format!("打开任务失败：{e}"))?
    }

    /// 枚举该文件的「打开方式」候选应用（读注册表，放 blocking 线程）。
    pub async fn open_with_candidates(&self, path: &Path) -> Vec<shell::OpenWithApp> {
        let p = path.to_path_buf();
        self.spawn_blocking(move || shell::open_with_candidates(&p))
            .await
            .unwrap_or_default()
    }

    /// Mo 自己的「打开方式」选择器的数据源：系统里装了哪些应用。
    ///
    /// 扫目录是毫秒级但仍是 IO，放 blocking 线程。非 macOS 恒为空。
    pub async fn installed_apps(&self) -> Vec<shell::OpenWithApp> {
        self.spawn_blocking(shell::installed_apps)
            .await
            .unwrap_or_default()
    }

    /// 本平台有没有 Mo 自己的「打开方式」选择器。
    ///
    /// 有的话 UI 走选择器（macOS：系统没有 `openas` 那样的对话框）；
    /// 没有就退回系统的（Windows 的 `openas` 动词）。
    pub fn has_app_picker() -> bool {
        cfg!(target_os = "macos")
    }

    /// 用「打开方式」里选中的应用打开文件。
    pub async fn open_with_app(&self, path: &Path, progid: &str) -> Result<(), String> {
        let p = path.to_path_buf();
        let progid = progid.to_string();
        self.spawn_blocking(move || shell::open_with_progid(&p, &progid))
            .await
            .map_err(|e| format!("打开任务失败：{e}"))?
    }

    /// 弹出系统的「打开方式」选择对话框。
    pub async fn open_with_dialog(&self, path: &Path) -> Result<(), String> {
        let p = path.to_path_buf();
        self.spawn_blocking(move || shell::open_with_dialog(&p))
            .await
            .map_err(|e| format!("打开任务失败：{e}"))?
    }

    /// 后退（基于导航栈，只加载不改写历史）。
    pub async fn go_back(&self) -> Result<(), MoError> {
        let target = {
            let mut inner = self.inner.write().await;
            inner.navigation.go_back()
        };
        if let Some(loc) = target {
            self.load_path(&loc).await?;
            self.bus.publish(AppEvent::NavigationChanged { path: loc });
        }
        Ok(())
    }

    /// 前进（基于导航栈，只加载不改写历史）。
    pub async fn go_forward(&self) -> Result<(), MoError> {
        let target = {
            let mut inner = self.inner.write().await;
            inner.navigation.go_forward()
        };
        if let Some(loc) = target {
            self.load_path(&loc).await?;
            self.bus.publish(AppEvent::NavigationChanged { path: loc });
        }
        Ok(())
    }

    /// 刷新当前目录（重读条目，但不产生导航历史）。
    pub async fn refresh(&self) -> Result<(), MoError> {
        if let Some(p) = self.current_path().await {
            self.load_path(&p).await?;
        }
        Ok(())
    }

    // ---- 远程连接 ----

    /// 按地址连上远程服务器并进入其根目录。
    ///
    /// 解析 `scheme://[user[:password]@]host[:port]/path`，建好对应后端后**整体替换**
    /// 底层 `fs`——之后所有目录读写都走远程后端，本地浏览态被清空避免历史混淆。
    /// 连接动作会真去建 socket + 登录，必须放在 blocking 池，绝不能占用
    /// UI / GPUI 执行器（见 `mo-remote` 的 runtime 约定）。
    ///
    /// 地址里**没写用户名**时，先查钥匙串里有没有存过这台服务器的凭据（用户上次
    /// 勾了「记住密码」）——存过就直接用，省掉再弹一次认证框。显式写了用户名就以
    /// 用户写的为准。
    ///
    /// 失败时保持原浏览态不变（不切 fs、不导航），并把失败分成两类返回，见
    /// [`ConnectFailure`]。
    pub async fn connect_remote(&self, input: &str) -> Result<(), ConnectFailure> {
        let mut url = Self::parse_remote(input)?;
        if url.user.is_none() {
            if let Some((user, password)) = credentials::load(&url.endpoint()) {
                url.user = Some(user);
                url.password = Some(password);
            }
        }
        self.finish_connect(url).await
    }

    /// 带上刚在认证框里填的凭据再连一次。
    ///
    /// `input` 是认证框对应的地址（`scheme://host:port`）；用户名 / 密码覆盖掉
    /// 地址里可能写着的旧值。
    pub async fn connect_remote_with_credentials(
        &self,
        input: &str,
        user: &str,
        password: &str,
    ) -> Result<(), ConnectFailure> {
        let mut url = Self::parse_remote(input)?;
        url.user = Some(user.to_string());
        url.password = Some(password.to_string());
        self.finish_connect(url).await
    }

    /// 解析地址 + 检查协议：两条连接入口共用的前置。
    fn parse_remote(input: &str) -> Result<RemoteUrl, ConnectFailure> {
        let url = RemoteUrl::parse(input).map_err(|e| ConnectFailure::Message(e.to_string()))?;
        // SMB / NFS 不走「建远程连接」那条路：它们是**系统挂载**之后当本地目录浏览
        // 的（见 `mo_remote::mount`），所以要在这里放行，由 `finish_connect` 分流。
        if !mo_remote::supports(&url.scheme) && !mo_remote::mount::is_mountable(&url.scheme) {
            return Err(ConnectFailure::Message(format!(
                "暂不支持的协议：{}（目前支持 ftp / sftp / webdav / davs / smb / nfs）",
                url.scheme
            )));
        }
        Ok(url)
    }

    /// 建连接 + 切换浏览态（首次连接与带凭据重试共用同一条路）。
    ///
    /// 同端点同用户名的会话已经在了，就**在它身上收场**，按密码分两种：
    ///
    /// * 密码没变（或地址里干脆没写密码，走钥匙串 / 匿名）——直接切过去，连 socket
    ///   都不重建，这是「切回本地不断开连接」省下的另一半；
    /// * 密码变了——真连一次，然后把那条会话的连接与凭据**原地**换掉。编号不变，
    ///   所以侧边栏还是那一行，不会多出一条来。
    async fn finish_connect(&self, url: RemoteUrl) -> Result<(), ConnectFailure> {
        // SMB / NFS：不建远程会话，触发**系统挂载**，然后当本地目录打开。
        // 挂好之后读写全走 `LocalFileSystem`，所以侧边栏「远程」区也不会多出一行
        // ——它出现在「网络」区，跟系统挂的其它盘在一起。
        if mo_remote::mount::is_mountable(&url.scheme) {
            return self.mount_and_open(url).await;
        }

        if let Some(existing) = self.sessions.find(&url) {
            let id = existing.id;
            let path = url.path.clone();
            if url.password.is_none() || url.password == existing.url.password {
                return self
                    .use_session(id, Some(&path))
                    .await
                    .map_err(|e| ConnectFailure::Message(e.to_string()));
            }
            // 换了凭据：这里以前是「再加一条新会话」，于是同一台服务器同一个账号会
            // 留下两行——旧那行往往已经死了（服务器改了密码 / 闲置被掐断），点它又
            // 是一次失败。一行一条连接才是侧边栏该有的样子。
            let connected = self.connect_now(&url).await?;
            self.sessions.set_url_and_fs(id, url, connected);
            return self
                .use_session(id, Some(&path))
                .await
                .map_err(|e| ConnectFailure::Message(e.to_string()));
        }

        let connected = self.connect_now(&url).await?;

        // 登记成一条新会话，并切到这个标签页看它。
        //
        // 端点相同而**账号不同**的会话仍然并存（注册表允许同端点多个账号）：那种
        // 情况上面的 `find` 匹配不上，确实该另登一条、另占一行。断开只走显式那条路。
        let id = self.sessions.add(url, connected);
        // 切过去并进入远程根目录。读失败会把浏览态整体回滚回原样（连接留着——它
        // 其实是通的，用户可以点侧边栏那一行再试），免得留下「徽标说 FTP、列表是
        // 本地那份」的半切换态。
        self.switch_source(
            Source {
                active: Some(id),
                on_remote: true,
            },
            Path::new("/"),
        )
        .await
        .map_err(|e| ConnectFailure::Message(e.to_string()))?;
        Ok(())
    }

    /// 真去建一条连接（首次连接与「换了凭据原地重连」共用这一条路）。
    ///
    /// 走注册表里那个可注入的 [`Connector`]（生产环境就是 `mo_remote::connect`）：
    /// 「换了凭据之后用的仍是**那一条**会话」这件事得能测——测试没法真连一台 FTP，
    /// 但能让 connector 每次返回一个列着不同文件名的假连接（见 `remote_local` 用例）。
    async fn connect_now(&self, url: &RemoteUrl) -> Result<Arc<dyn FileSystem>, ConnectFailure> {
        let endpoint = url.endpoint();
        let user = url.user.clone().unwrap_or_default();
        let connect = self.sessions.connector();
        let target = url.clone();
        self.spawn_blocking(move || connect(&target))
            .await
            .map_err(|e| ConnectFailure::Message(format!("连接任务失败：{e}")))?
            .map_err(|e| match e {
                // 「服务器要凭据」单独分流：UI 据此弹认证框，而不是干显示一句话。
                mo_remote::RemoteError::AuthRequired { detail, .. } => {
                    ConnectFailure::NeedsCredentials {
                        endpoint,
                        user,
                        detail,
                    }
                }
                other => ConnectFailure::Message(other.to_string()),
            })
    }

    /// 触发系统挂载，然后**当本地目录**打开（SMB / NFS 走这条路）。
    ///
    /// 挂载是阻塞的系统调用（可能要等服务器应答），必须在 blocking 池里跑。
    /// 挂好之后这条连接就不存在「会话」了——它就是个本地目录，卸载由
    /// [`AppState::unmount_share`] 负责（系统也可能自己卸掉）。
    async fn mount_and_open(&self, url: RemoteUrl) -> Result<(), ConnectFailure> {
        let point = self
            .spawn_blocking(move || mo_remote::mount::mount(&url))
            .await
            .map_err(|e| ConnectFailure::Message(format!("挂载任务失败：{e}")))?
            .map_err(|e| ConnectFailure::Message(format!("挂载失败：{e}")))?;
        self.invalidate_net_shares();
        self.open_local(&point)
            .await
            .map_err(|e| ConnectFailure::Message(format!("打开挂载点失败：{e}")))?;
        Ok(())
    }

    /// 系统里已经挂好的**网络盘**（SMB / NFS），供侧边栏「网络」区显示。
    ///
    /// 只读（读 `/proc/mounts` 或跑一次 `mount` / `net use`），不发起任何网络操作。
    pub fn network_shares(&self) -> Vec<mo_remote::mount::NetworkShare> {
        let mut slot = self.net_shares.lock().unwrap();
        if slot.0.elapsed() >= NET_SHARE_TTL {
            *slot = (
                std::time::Instant::now(),
                mo_remote::mount::mounted_shares(),
            );
        }
        slot.1.clone()
    }

    /// 作废网络盘的缓存（挂载 / 卸载之后立刻生效，不必等 TTL 过期）。
    fn invalidate_net_shares(&self) {
        self.net_shares.lock().unwrap().0 = std::time::Instant::now() - NET_SHARE_TTL;
    }

    /// 卸载一块网络盘（侧边栏那个「推出」）。
    ///
    /// 顺序：先问**平台**（macOS 走 AppKit 的 `unmountAndEjectDeviceAtURL:`——
    /// 它会接管「还有文件在用」这类情况，也比直接 `umount` 干净），平台没实现或
    /// 没做成再退回命令行 `umount`。后者是必需的兜底：Mo 自己挂的 NFS / SMB 落在
    /// 应用数据目录下的空目录里，系统并不把它当「设备」，AppKit 那套会拒绝。
    pub async fn unmount_share(&self, path: PathBuf) -> Result<(), MoError> {
        let native = {
            let p = path.clone();
            self.spawn_blocking(move || mo_platform::eject(&p)).await
        };
        let r = match native {
            Ok(Ok(())) => Ok(()),
            _ => {
                let p = path.clone();
                self.spawn_blocking(move || mo_remote::mount::unmount(&p))
                    .await
                    .map_err(|e| MoError::Other(format!("卸载任务失败：{e}")))?
                    .map_err(|e| MoError::Other(e.to_string()))
            }
        };
        // 卸了（哪怕失败）列表都该重新查一次。
        self.invalidate_net_shares();
        r
    }

    /// 本机已挂载的**卷宗**（侧边栏「位置」区）：外接磁盘、DMG、Time Machine 盘……
    ///
    /// 只读，不发起任何网络操作。网络盘不在这里（归「网络」区，由
    /// [`AppState::network_shares`] 负责）。按 [`VOLUME_TTL`] 缓存，因为列表要
    /// `statfs` 逐个查（`mo_platform::volumes` 内已把网络文件系统过滤掉）、还要
    /// 逐块问一次卷宗属性（能否推出）。
    ///
    /// 每块盘带 [`mo_platform::Volume::ejectable`]：侧栏据此决定给不给推出按钮。
    /// ⚠️ 这步走 AppKit，必须在主线程读——所以缓存键里也含它，别在后台线程预填。
    pub fn volumes(&self) -> Vec<mo_platform::Volume> {
        let mut slot = self.volumes_cache.lock().unwrap();
        if slot.0.elapsed() >= VOLUME_TTL {
            *slot = (std::time::Instant::now(), mo_platform::volumes());
        }
        slot.1.clone()
    }

    /// 作废卷宗列表缓存（推出 / 挂载之后立刻生效）。
    fn invalidate_volumes(&self) {
        self.volumes_cache.lock().unwrap().0 = std::time::Instant::now() - VOLUME_TTL;
    }

    /// 推出一块本机卷宗（侧边栏「位置」区那个「推出」）。
    ///
    /// 与 [`AppState::unmount_share`] 同一条纪律：先问平台（macOS 走 AppKit 的
    /// `unmountAndEjectDeviceAtURL:`，它会接管「还有窗口在用」这类情况），平台没实现
    /// 或没做成再退回命令行 `umount`（兜底）。推出后不论成败都作废缓存让列表刷新。
    pub async fn eject_volume(&self, path: PathBuf) -> Result<(), MoError> {
        let native = {
            let p = path.clone();
            self.spawn_blocking(move || mo_platform::eject(&p)).await
        };
        let r = match native {
            Ok(Ok(())) => Ok(()),
            _ => {
                let p = path.clone();
                self.spawn_blocking(move || mo_remote::mount::unmount(&p))
                    .await
                    .map_err(|e| MoError::Other(format!("卸载任务失败：{e}")))?
                    .map_err(|e| MoError::Other(e.to_string()))
            }
        };
        self.invalidate_volumes();
        r
    }

    /// 取一个文件在**系统**里的图标（macOS 走 `NSWorkspace.iconForFile:`）。
    ///
    /// 返回的是缓存后的 PNG 文件路径——GPUI 的 `img()` 吃路径。系统图标是光栅图，
    /// 没法像内置 SVG 那样用文字色描边，所以这里直接出 PNG。缓存按路径键（系统图标
    /// 由文件类型决定，与内容无关），首见才真去问系统、之后命中。
    ///
    /// 当前在看远程时整页都是远程条目，本机没有这些文件，系统给不出图标，返回
    /// `None`——调用方（列表行）退回内置 SVG 图标。非 macOS 也返回 `None`。
    ///
    /// ⚠️ 这是**同步**方法：列表渲染每帧都来要图标，不能 `await`。远程判据用
    /// [`AppState::browsing_remote`]（按当前页，足够——图标总是当前页的条目）。
    pub fn file_icon(&self, path: &Path) -> Option<PathBuf> {
        if self.browsing_remote() {
            return None;
        }
        let mut cache = self.file_icon_cache.lock().unwrap();
        if let Some(hit) = cache.get(path) {
            return Some(hit.clone());
        }
        // 系统图标要问 AppKit，必须走 OS 主线程（`on_main_thread` 在非主线程会
        // `dispatch_sync` 回主队列）。GPUI 的渲染就跑在 OS 主线程上，真机没问题；
        // 但 `cargo test` 的 `TestAppContext` 把渲染跑在子线程、主队列不 drain，
        // 直接 `dispatch_sync` 会死锁（和 `reveal`/`eject` 同款陷阱）。这种「不在
        // 主线程」的情况（测试 / 后台任务）跳过平台调用、回退内置 SVG——系统图标
        // 只是视觉加成，不致命。
        if !mo_platform::is_main_thread() {
            return None;
        }
        let bytes = mo_platform::file_icon(path)?;
        // 写进缓存目录，返回路径让 `img()` 加载。
        let dir = std::env::temp_dir().join("mo-icons");
        let _ = std::fs::create_dir_all(&dir);
        let name = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            path.hash(&mut h);
            format!("{:x}.png", h.finish())
        };
        let out = dir.join(name);
        if std::fs::write(&out, &bytes).is_err() {
            return None;
        }
        // 容量封顶：超出整清空，下次重新逐条问（避免无限涨内存）。
        if cache.len() > 4000 {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), out.clone());
        Some(out)
    }

    /// 「记住的服务器」列表（最近使用的在前）。
    pub fn saved_servers(&self) -> Vec<SavedServer> {
        let mut list = self.config().remote_servers;
        // 最近的在前（`Reverse` 是因为默认是升序）。
        list.sort_by_key(|s| std::cmp::Reverse(s.last_used));
        list
    }

    /// 记下这台服务器，并可选择把密码写进系统钥匙串。
    ///
    /// * 列表（`config.json`，**明文**）里只留地址 / 用户名 / 时间戳；
    /// * `password` 为 `Some` 才写钥匙串——没勾「记住密码」时**不碰**已有条目
    ///   （用户可能更早勾过一次），要删走 [`AppState::forget_server`]。
    ///
    /// 返回钥匙串写入的错误（如果有）：UI 据此提示「这次没记住密码」，但不该
    /// 影响刚刚成功的连接。
    pub fn remember_server(
        &self,
        endpoint: &str,
        user: &str,
        password: Option<&str>,
    ) -> Result<(), String> {
        let mut cfg = self.config();
        cfg.remote_servers.retain(|s| s.endpoint != endpoint);
        cfg.remote_servers.insert(
            0,
            SavedServer {
                endpoint: endpoint.to_string(),
                user: user.to_string(),
                last_used: now_secs(),
            },
        );
        // 只留最近的一批，免得配置文件被连过的临时地址撑爆。
        cfg.remote_servers.truncate(SAVED_SERVERS_MAX);
        self.save_config(&cfg);

        match password {
            Some(pw) => credentials::store(endpoint, user, pw),
            None => Ok(()),
        }
    }

    /// 忘掉一台服务器：从列表里删掉，并清掉钥匙串里存下的密码。
    pub fn forget_server(&self, endpoint: &str) -> Result<(), String> {
        let mut cfg = self.config();
        cfg.remote_servers.retain(|s| s.endpoint != endpoint);
        self.save_config(&cfg);
        credentials::forget(endpoint)
    }

    /// 断开一条远程连接：把它从注册表里摘掉（连接随之关闭）。
    ///
    /// 本标签页若正看着它，就切回本地主目录；别的标签页若还看着它，下一次读目录会
    /// 因为会话不在了而回落本地（见 [`Self::active_fs`]）。
    ///
    /// 这正是「只有退出应用才断开」之外**唯一**会主动关连接的地方——关标签页不算。
    pub async fn disconnect_connection(&self, id: SessionId) -> Result<(), MoError> {
        let dropped_current = {
            let mut src = self.source.lock().unwrap();
            if src.active == Some(id) {
                // 连「本标签页刚才在看它、现在切到本地了」也要清：那条连接已经没了。
                let was_browsing = src.on_remote;
                src.active = None;
                src.on_remote = false;
                was_browsing
            } else {
                false
            }
        };
        self.sessions.remove(id);
        if dropped_current {
            self.inner.write().await.navigation = NavigationState::new();
            if let Some(home) = dirs::home_dir() {
                self.open_directory(&home).await?;
            }
        }
        Ok(())
    }

    /// 断开本标签页当前那条远程连接（命令面板「断开远程连接」走这里）。
    ///
    /// 侧边栏是按行断开的（每行一个图标），走的是 [`Self::disconnect_connection`]。
    /// **只是切回本地不算断开**——那是 [`Self::open_local`]。
    pub async fn disconnect_remote(&self) -> Result<(), MoError> {
        let id = self.source.lock().unwrap().active;
        match id {
            Some(id) => self.disconnect_connection(id).await,
            None => Ok(()),
        }
    }

    /// 确认这条会话的连接还能用：闲置够久就先探活，断了就**原地重连**。
    ///
    /// 用户报的场景：连上 FTP → 切去本地干活 → 过一阵子切回来。服务器早把闲置的
    /// 控制连接掐了，于是「切回去」等于对一条死 socket 发命令，界面弹
    /// `Broken pipe (os error 32)`。连接对象连同凭据都还在注册表里，重连不需要
    /// 用户再做任何事。
    ///
    /// 返回 `Err` 说明这条连接当前不可用：凭据被拒（`NeedsCredentials`，UI 可以
    /// 据此弹认证框）或别的失败。
    async fn revive_if_stale(&self, id: SessionId) -> Result<(), ConnectFailure> {
        let Some(session) = self.sessions.entry(id) else {
            return Err(ConnectFailure::Message(
                "这条远程连接已经断开了".to_string(),
            ));
        };
        // 刚用过就不必探活：一次 NOOP / stat 也是一次网络往返。
        if session.last_used.elapsed() < IDLE_PROBE {
            return Ok(());
        }
        let fs = session.fs.clone();
        let alive = self
            .spawn_blocking(move || fs.is_alive())
            .await
            .unwrap_or(false);
        if alive {
            self.sessions.touch(id);
            return Ok(());
        }
        self.reconnect(id).await
    }

    /// 重建这条会话的连接：**不看探活结果**（要么探活说断了，要么读目录已经失败）。
    ///
    /// 连接对象在注册表里**原地替换**，编号不变——侧边栏那一行不闪、高亮不跳。
    async fn reconnect(&self, id: SessionId) -> Result<(), ConnectFailure> {
        let Some(session) = self.sessions.entry(id) else {
            return Err(ConnectFailure::Message(
                "这条远程连接已经断开了".to_string(),
            ));
        };
        let url = session.url.clone();
        let endpoint = url.endpoint();
        let user = url.user.clone().unwrap_or_default();
        let connect = self.sessions.connector();
        let connected = self
            .spawn_blocking(move || connect(&url))
            .await
            .map_err(|e| ConnectFailure::Message(format!("重连任务失败：{e}")))?
            .map_err(|e| match e {
                // 服务器拒了凭据（比如闲置期间那边改了密码）：交给 UI 弹认证框。
                mo_remote::RemoteError::AuthRequired { detail, .. } => {
                    ConnectFailure::NeedsCredentials {
                        endpoint,
                        user,
                        detail,
                    }
                }
                other => ConnectFailure::Message(format!("重新连接失败：{other}")),
            })?;
        self.sessions.set_fs(id, connected);
        Ok(())
    }

    /// 切来源的统一收尾：先记下原状态，切失败就**整体回滚**。
    ///
    /// 半切换态是最难查的一类 bug：`on_remote` 已经翻了、目录却没换成，于是标签页
    /// 徽标与地址栏说自己在 FTP、列表还是本地那一份，侧边栏两边同时高亮（用户报的
    /// 「关闭弹窗后左侧选中了 2 个项目」正是这个）。
    ///
    /// 回滚连导航栈一起还：`open_directory` 会把新路径压进历史，失败后留着它，
    /// 「后退」就会拿远程路径去本地读。
    async fn switch_source(&self, src: Source, target: &Path) -> Result<(), MoError> {
        let prev_src = self.source.lock().unwrap().clone();
        let prev_nav = self.inner.read().await.navigation.clone();
        let changed = prev_src.on_remote != src.on_remote || prev_src.active != src.active;
        *self.source.lock().unwrap() = src;
        // 换来源 = 换了一套路径命名空间：导航栈必须重置，否则「后退」会拿远程路径
        // 去本地读、或反过来。
        if changed {
            self.inner.write().await.navigation = NavigationState::new();
        }
        if let Err(e) = self.open_directory(target).await {
            *self.source.lock().unwrap() = prev_src;
            self.inner.write().await.navigation = prev_nav;
            return Err(e);
        }
        Ok(())
    }

    /// 打开本地目录。
    ///
    /// 当前在看远程时**只把「看哪边」切回本地，连接留着**：切来源要重置导航栈
    /// （本地路径与远程路径不能混在一个栈里回退），但会话不关——点侧边栏那条连接
    /// （[`Self::open_connection`]）就能接着用，不用重新登录。
    ///
    /// 侧边栏快捷访问与地址栏的本地路径输入都走这里，保证「回到本地」显式且安全。
    pub async fn open_local(&self, path: &Path) -> Result<(), MoError> {
        let src = {
            let cur = self.source.lock().unwrap();
            Source {
                on_remote: false,
                ..cur.clone()
            }
        };
        self.switch_source(src, path).await
    }

    /// 切到指定会话（侧边栏那条连接点一下走这里）：把浏览态切过去，并回到它上次
    /// 待过的目录。
    ///
    /// 连接断了（闲置被服务器掐掉）会**先原地重连**，用户什么也不用做；返回
    /// [`ConnectFailure`] 是为了把「服务器拒绝这组凭据」单独带出来，让 UI 弹认证框
    /// 而不是干显示一句话。
    pub async fn open_connection(&self, id: SessionId) -> Result<(), ConnectFailure> {
        self.revive_if_stale(id).await?;
        self.use_session(id, None)
            .await
            .map_err(|e| ConnectFailure::Message(e.to_string()))
    }

    /// 切回本标签页刚才待过的那条会话（`Ok` 时当前就在远程了）。
    pub async fn open_remote(&self) -> Result<(), MoError> {
        // 已经在这条会话里了：什么也不做。否则「再看一眼当前目录」会把导航栈
        // 清掉——只有**换来源**才该重置历史。
        if self.browsing_remote() {
            return Ok(());
        }
        let id = self
            .source
            .lock()
            .unwrap()
            .active
            .ok_or_else(|| MoError::Other("当前没有远程连接".to_string()))?;
        self.use_session(id, None).await
    }

    /// 切到指定会话；`path` 为 `None` 时回到它上次待过的目录。
    ///
    /// 只改「当前在哪」，不碰连接对象本身。
    async fn use_session(&self, id: SessionId, path: Option<&str>) -> Result<(), MoError> {
        // 会话可能已经被别处断开了（侧边栏、命令面板、或者另一个标签页）。
        // 先确认它还在——这一步拿注册表的锁，**不要**同时持着 `source` 的锁。
        let remembered = self
            .sessions
            .path_of(id)
            .ok_or_else(|| MoError::Other("这条远程连接已经断开了".to_string()))?;
        let target = match path {
            Some(p) => {
                self.sessions.set_path(id, p.to_string());
                p.to_string()
            }
            None => remembered,
        };
        self.switch_source(
            Source {
                active: Some(id),
                on_remote: true,
            },
            Path::new(&target),
        )
        .await
    }

    /// 活着的全部连接（侧边栏「远程」区按它渲染；任何标签页看到的都是同一张表）。
    pub fn live_connections(&self) -> Vec<LiveConnection> {
        self.sessions.snapshot()
    }

    /// 本标签页正在浏览的那条连接的编号（侧边栏据此高亮当前那条）。
    pub fn active_connection_id(&self) -> Option<SessionId> {
        let id = self.active_session()?;
        // 会话在浏览中途被断开时，`active` 还留着旧编号——那时不该报「正在看它」。
        self.sessions.url_of(id).map(|_| id)
    }

    /// 当前**正在浏览**的远程地址（看本地时是 `None`）。地址栏回显 / 标签页徽标用它。
    pub fn remote_url(&self) -> Option<RemoteUrl> {
        self.active_session()
            .and_then(|id| self.sessions.url_of(id))
    }

    /// 当前是否在看远程（标签页徽标 / 地址栏的回车语义用它）。
    pub fn browsing_remote(&self) -> bool {
        self.remote_url().is_some()
    }

    /// ⚠️ 仅供测试：直接登入一条「已连接」的会话，避免单测真去建 socket + 登录。
    ///
    /// 生产路径只有 [`Self::finish_connect`]（真连 → 登记会话 → 进远程根）。
    /// `url` 只用于回显，形如 `ftp://example.com:2121`。
    #[doc(hidden)]
    pub fn install_backend_for_test(&self, fs: Arc<dyn FileSystem>, url: &str) {
        let url = RemoteUrl::parse(url).expect("测试用的远程地址应当能解析");
        let id = self.sessions.add(url, fs);
        let mut src = self.source.lock().unwrap();
        src.active = Some(id);
        src.on_remote = true;
    }

    /// 后退栈是否非空。
    pub async fn can_go_back(&self) -> bool {
        self.inner.read().await.navigation.can_go_back()
    }

    /// 前进栈是否非空。
    pub async fn can_go_forward(&self) -> bool {
        self.inner.read().await.navigation.can_go_forward()
    }

    /// 启动文件系统监听泵（进程内调用一次即可）。
    ///
    /// 外部程序改动目录时，`notify` 只投递单个事件，这里据此做**增量**更新：
    /// 新建则插入一条、删除则移除一条、重命名则就地改名，而不是重读整个目录。
    pub fn spawn_watcher_pump(&self) {
        let app = self.clone();
        self.spawn(async move {
            loop {
                // 标签页已关：收工。泵握着 `AppState` 的克隆，不退出的话这份
                // `AppState`（以及它持有的注册表 `Arc`）就跟着一起被拖着不放。
                if app.stopped() {
                    return;
                }
                let events = {
                    let guard = app.watcher.lock().await;
                    let mut out = Vec::new();
                    if let Some(w) = guard.as_ref() {
                        while let Some(ev) = w.try_recv() {
                            out.push(ev);
                        }
                    }
                    out
                };
                for ev in events {
                    app.apply_watcher_event(ev).await;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    /// 把一个监听事件增量应用到目录模型，并通过事件总线广播。
    async fn apply_watcher_event(&self, ev: WatcherEvent) {
        match ev {
            WatcherEvent::Created(path) => {
                let Some(r) = entry_at(&path) else { return };
                let entry = Entry::new(r.id, r.name, r.kind, r.path);
                let inserted = {
                    let mut inner = self.inner.write().await;
                    match inner.directory.as_mut() {
                        Some(dir) if !dir.entries.iter().any(|e| e.path == path) => {
                            // push_entry 内部会维护索引并重建排序 / 过滤视图。
                            dir.push_entry(entry.clone());
                            true
                        }
                        _ => false,
                    }
                };
                if inserted {
                    // 新条目的元数据交给后台调度补上（渐进式加载）。
                    self.scheduler().load(self.clone(), vec![entry], Some(0..1));
                    self.bus
                        .publish(AppEvent::EntryCreated { path: path.clone() });
                    self.publish_dir_changed().await;
                }
            }
            WatcherEvent::Removed(path) => {
                let removed = {
                    let mut inner = self.inner.write().await;
                    match inner.directory.as_mut() {
                        // remove_entry 内部会重建索引与视图。
                        Some(dir) => dir.remove_entry(&path),
                        None => false,
                    }
                };
                if removed {
                    self.bus.publish(AppEvent::EntryDeleted { path });
                    self.publish_dir_changed().await;
                }
            }
            WatcherEvent::Renamed { from, to } => {
                let renamed = {
                    let mut inner = self.inner.write().await;
                    match inner.directory.as_mut() {
                        Some(dir) => {
                            if let Some(e) = dir.entry_mut_by_path(&from) {
                                e.path = to.clone();
                                e.name = to
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                dir.rebuild_view();
                                true
                            } else {
                                false
                            }
                        }
                        None => false,
                    }
                };
                if renamed {
                    self.bus.publish(AppEvent::EntryRenamed { from, to });
                    self.publish_dir_changed().await;
                }
            }
            WatcherEvent::Modified(path) => {
                let id = {
                    let inner = self.inner.read().await;
                    inner
                        .directory
                        .as_ref()
                        .and_then(|d| d.entries.iter().find(|e| e.path == path).map(|e| e.id))
                };
                if let Some(id) = id {
                    if let Ok(meta) = self.active_fs().metadata(&path).await {
                        self.update_metadata(id, meta).await;
                        self.bus.publish(AppEvent::MetadataLoaded { path });
                    }
                }
            }
        }
    }

    /// 以当前目录路径广播一次「目录已变化」，触发 UI 刷新快照。
    async fn publish_dir_changed(&self) {
        if let Some(p) = self.current_path().await {
            self.bus.publish(AppEvent::DirectoryChanged { path: p });
        }
    }

    /// 当前目录下的条目快照（全量）。
    ///
    /// 注意：大目录请优先用 [`Self::visible_window`] 只取可见区。
    pub async fn current_entries(&self) -> Vec<Entry> {
        self.inner
            .read()
            .await
            .directory
            .as_ref()
            .map(|d| d.entries.clone())
            .unwrap_or_default()
    }

    /// 这个路径**是不是目录**——问列表模型（`EntryKind`），**不问本地磁盘**。
    ///
    /// 为什么不直接用 `Path::is_dir()`：那查的是**本机文件系统**，而远程条目的路径
    /// （`/1`）在本机根本不存在，远程目录会被一律判成「文件」，双击 / 回车把它交给
    /// 系统默认应用去打开——用户看到的就是日志里那几行
    /// `The file /1 does not exist.`，界面一动不动。
    ///
    /// 判据取当前目录列表里那一行的 `kind`（零 IO）：列表画着文件夹图标、双击就该
    /// 进去，两者必须同源。返回 `None` = 这个路径不在当前列表里（分栏中兄弟列的条目、
    /// 磁盘分析 / 搜索结果、书签……），由调用方决定怎么兜底。
    pub async fn entry_is_dir(&self, path: &Path) -> Option<bool> {
        self.inner
            .read()
            .await
            .directory
            .as_ref()?
            .entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.kind.is_dir())
    }

    /// 这条路径**属于当前远程后端吗**——决定文件操作该走远程后端还是本机管线。
    ///
    /// 判据是「它是不是当前列表里的那一行」，**不是**「我现在在看远程吗」：
    /// 去重 / 文件夹同步这类功能会拿着**本地**路径来调同一批 API（`trash_paths`），
    /// 一刀切会把本地文件当远程路径发给服务器（反过来更常见：在看远程时删本地
    /// 文件，被送去本机回收站是对的，不该改成远程删除）。
    ///
    /// 列表里查得到 = 它就是当前后端列出来的条目；在看远程时即远程条目。
    /// 判据与 [`AppState::entry_is_dir`] 同源，都不碰本机磁盘。
    ///
    /// 对外暴露的原因：平台原生动作（在访达中显示 / 移到系统废纸篓）也按这条分流，
    /// 上层（UI 的菜单裁剪）与测试都要问同一个问题，别各写一份判据。
    pub async fn goes_through_remote(&self, path: &Path) -> bool {
        self.browsing_remote() && self.entry_is_dir(path).await.is_some()
    }

    /// `dir` 下这个名字已经被占了吗（新建文件夹 / 新建文件去重用）。
    ///
    /// 顺序：先问**当前列表**（零 IO，远程条目也答得出来），列表不在这页时再问后端
    /// ——远程是 `metadata`（网络往返），本地是 `Path::exists()`。
    /// ⚠️ 不能只用 `Path::exists()`：远程路径在本机不存在，恒返回 `false`，
    /// 于是「新建文件夹」在远程目录里永远不去重，重名时直接撞服务端的错。
    async fn name_taken(&self, path: &Path) -> bool {
        if self.entry_is_dir(path).await.is_some() {
            return true;
        }
        if self.browsing_remote() {
            self.active_fs().metadata(path).await.is_ok()
        } else {
            path.exists()
        }
    }

    /// 当前目录路径。
    pub async fn current_path(&self) -> Option<PathBuf> {
        self.inner.read().await.navigation.current.clone()
    }

    /// 可见（已过滤 + 已排序）条目数量：虚拟化列表的 `item_count`。
    pub async fn visible_count(&self) -> usize {
        self.inner
            .read()
            .await
            .directory
            .as_ref()
            .map(|d| d.visible_count())
            .unwrap_or(0)
    }

    /// 只取可见区间的条目，返回 `(读取时的目录路径, 实际起始下标, 条目)`。
    ///
    /// UI 用它做窗口懒加载：滚动到哪取哪，永远不克隆整份列表——
    /// 这是十万级目录仍能保持流畅的前提。
    ///
    /// 返回路径是为了让调用方落地前校验：取回任务在途时用户可能已切换目录，
    /// 旧目录的快照绝不能覆盖新目录的窗口。
    pub async fn visible_window(&self, range: Range<usize>) -> (PathBuf, usize, Vec<Entry>) {
        let inner = self.inner.read().await;
        let Some(dir) = inner.directory.as_ref() else {
            return (PathBuf::new(), 0, Vec::new());
        };
        let dir_path = dir.path.clone();
        let total = dir.visible_count();
        let start = range.start.min(total);
        let end = range.end.min(total);
        let mut out = Vec::with_capacity(end.saturating_sub(start));
        for i in start..end {
            if let Some(e) = dir.visible_entry(i) {
                out.push(e.clone());
            }
        }
        (dir_path, start, out)
    }

    /// UI 报告当前可见范围：元数据与缩略图都据此排优先级。
    pub async fn set_visible_range(&self, range: Range<usize>) {
        self.inner.write().await.visible_range = range;
    }

    /// 设置名称过滤（快速搜索）。`None` 或空白表示不过滤。
    /// 读取一个目录的轻量条目列表（列视图用）。
    ///
    /// 与 `load_path` 不同：它**不动主目录模型**（导航栈 / 选择 / 监听目标），
    /// 只是给 UI 的一列提供名字与类型。读盘仍是阻塞 IO，走 blocking 池。
    pub async fn list_dir(&self, path: &Path) -> Result<Vec<LightEntry>, MoError> {
        let fs = self.active_fs();
        let p = path.to_path_buf();
        let raw = self
            .spawn_blocking(move || fs.read_dir_blocking(&p))
            .await
            .map_err(|e| MoError::Other(format!("列视图读取目录的任务失败：{e}")))??;
        let mut out: Vec<LightEntry> = raw
            .into_iter()
            .map(|r| LightEntry {
                name: r.name,
                kind: r.kind,
                path: r.path,
            })
            .collect();
        // 目录在前、其余按名称（与列表视图的默认顺序一致）。
        out.sort_by(|a, b| {
            b.kind
                .is_dir()
                .cmp(&a.kind.is_dir())
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    pub async fn set_filter(&self, query: Option<String>) {
        {
            let mut inner = self.inner.write().await;
            if let Some(dir) = inner.directory.as_mut() {
                dir.set_filter(query);
            }
        }
        self.publish_dir_changed().await;
    }

    /// 设置排序方式（键 + 方向）。
    pub async fn set_sort(&self, key: SortKey, dir: SortDir) {
        {
            let mut inner = self.inner.write().await;
            if let Some(dir_state) = inner.directory.as_mut() {
                dir_state.set_sort(key, dir);
            }
        }
        self.publish_dir_changed().await;
    }

    /// 当前排序方式（键 + 方向）；没有目录时为默认值。
    ///
    /// UI 用它给列表头画排序指示箭头。
    pub async fn sort(&self) -> (SortKey, SortDir) {
        self.inner
            .read()
            .await
            .directory
            .as_ref()
            .map(|d| (d.view.sort(), d.view.sort_dir()))
            .unwrap_or_default()
    }

    /// 当前是否处于过滤态（UI 用它显示「清除过滤」入口）。
    pub async fn is_filtered(&self) -> bool {
        self.inner
            .read()
            .await
            .directory
            .as_ref()
            .map(|d| d.view.is_filtered())
            .unwrap_or(false)
    }

    /// 用加载完成的元数据更新某个条目（O(1) 索引查找）。
    ///
    /// 只置 dirty 位，不立即广播——逐条广播会把 UI 刷爆，
    /// 由 [`Self::spawn_refresh_pump`] 合并成节拍性的刷新。
    pub async fn update_metadata(&self, id: FileId, meta: FileMetadata) {
        {
            let mut inner = self.inner.write().await;
            if let Some(dir) = inner.directory.as_mut() {
                if let Some(e) = dir.entry_mut(id) {
                    e.metadata = MetadataState::Loaded(meta);
                    self.dirty.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    /// 用加载完成的元数据**批量**更新条目（一批只拿一次写锁）。
    ///
    /// 后台校验按批（`STAT_BATCH`）回填：写锁次数从「每条一次」降到「每批一次」，
    /// 否则大目录回填期间 UI 的读操作（窗口取回等）会被写锁洪流饿死。
    pub async fn update_metadata_batch(&self, updates: Vec<(FileId, FileMetadata)>) {
        let mut inner = self.inner.write().await;
        let Some(dir) = inner.directory.as_mut() else {
            return;
        };
        let mut changed = false;
        for (id, meta) in updates {
            if let Some(e) = dir.entry_mut(id) {
                e.metadata = MetadataState::Loaded(meta);
                changed = true;
            }
        }
        if changed {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// 更新某个条目的缩略图状态（同样合并广播）。
    pub async fn set_thumbnail(&self, id: FileId, state: ThumbnailState) {
        {
            let mut inner = self.inner.write().await;
            if let Some(dir) = inner.directory.as_mut() {
                if let Some(e) = dir.entry_mut(id) {
                    e.thumbnail = state;
                    self.dirty.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    /// 启动刷新泵：把密集的模型变更合并成节拍性的广播。
    ///
    /// 打开一个一万条目的目录时，元数据是逐条回填的；
    /// 若每回填一条就通知 UI，UI 会重绘一万次。这里每 120ms 最多通知一次，
    /// 视觉上信息依然是「逐步浮现」，但开销降到百分之一。
    pub fn spawn_refresh_pump(&self) {
        let app = self.clone();
        self.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(120)).await;
                if app.stopped() {
                    return;
                }
                if app.dirty.swap(false, Ordering::Relaxed) {
                    app.bus.publish(AppEvent::MetadataLoaded {
                        path: PathBuf::new(),
                    });
                }
            }
        });
    }

    // ---- 选择 ----

    pub async fn selection(&self) -> SelectionModel {
        self.inner.read().await.selection.clone()
    }

    pub async fn select(&self, id: FileId) {
        self.inner.write().await.selection.select(id);
    }

    pub async fn toggle(&self, id: FileId) {
        self.inner.write().await.selection.toggle(id);
    }

    pub async fn clear_selection(&self) {
        self.inner.write().await.selection.clear();
    }

    // ---- 文件操作 ----

    /// 提交一个文件操作：注册到队列 → 后台执行 → 周期性广播进度。
    ///
    /// 返回操作 ID。**不会**等待操作完成，UI 可以继续浏览其他目录。
    pub async fn submit_operation(&self, op: SharedOperation) -> u64 {
        let id = {
            let mut mgr = self.ops.lock().await;
            mgr.register(op.clone());
            op.id()
        };
        self.bus.publish(AppEvent::OperationStarted { id });

        let app = self.clone();
        let bus = self.bus.clone();
        let progress_op = op.clone();
        let running = Arc::new(AtomicBool::new(true));

        // 进度播报：每 150ms 一次，直到操作进入终态。
        {
            let bus = bus.clone();
            let done = running.clone();
            let total = {
                // 先播一次，让 UI 立刻看到 0%。
                let (d, t) = progress_op.progress();
                bus.publish(AppEvent::OperationProgress {
                    id,
                    done: d,
                    total: t,
                });
                t
            };
            let _ = total;
            app.spawn(async move {
                while done.load(Ordering::Relaxed) {
                    let (d, t) = progress_op.progress();
                    bus.publish(AppEvent::OperationProgress {
                        id,
                        done: d,
                        total: t,
                    });
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            });
        }

        let final_op = op.clone();
        let app_task = app.clone();
        app.spawn(async move {
            // 文件操作是阻塞 IO，放 blocking 池。
            let handle = app_task.spawn_blocking(move || op.run());
            let result = handle.await;
            running.store(false, Ordering::Relaxed);

            let (d, t) = final_op.progress();
            bus.publish(AppEvent::OperationProgress {
                id,
                done: d,
                total: t,
            });
            match result {
                Ok(Err(e)) => tracing::warn!("操作 {id} 失败：{e}"),
                Err(e) => tracing::warn!("操作 {id} 任务异常：{e}"),
                Ok(Ok(())) => {}
            }
            bus.publish(AppEvent::OperationFinished { id });
        });

        id
    }

    /// 当前所有操作的快照（进度面板数据源）。
    pub async fn operations_snapshot(&self) -> Vec<OperationHandle> {
        self.ops.lock().await.snapshot()
    }

    /// 取消一个进行中的操作。
    pub async fn cancel_operation(&self, id: u64) {
        self.ops.lock().await.cancel(id);
    }
}

impl AppState {
    // ---- 全局搜索索引 ----

    /// 后台递归爬取 `root` 建立全局搜索索引。
    ///
    /// 爬取在 blocking 池进行（只取 name/kind/path，不逐个 stat），
    /// 期间周期性广播 [`AppEvent::IndexUpdated`]，完成后再次广播最终数量。
    /// `max_depth` 为 0 表示不限深度。
    pub fn index_root(&self, root: PathBuf, max_depth: usize) {
        let app = self.clone();
        let bus = self.bus.clone();
        let root_after = root.clone();
        let index = self.index.clone();
        let stop = self.index_stop.clone();
        let fs = self.active_fs();
        self.spawn(async move {
            stop.store(false, Ordering::Relaxed);
            let bus_p = bus.clone();
            let result = app
                .spawn_blocking(move || {
                    let mut idx = index.lock();
                    crawl(&mut idx, fs.as_ref(), &root, max_depth, &stop, |n| {
                        bus_p.publish(AppEvent::IndexUpdated {
                            indexed: n,
                            root: root.clone(),
                        });
                    })
                    .map_err(|e| e.to_string())
                })
                .await;
            match result {
                Ok(Ok(n)) => bus.publish(AppEvent::IndexUpdated {
                    indexed: n,
                    root: root_after,
                }),
                Ok(Err(e)) => tracing::warn!("索引失败：{e}"),
                Err(e) => tracing::warn!("索引任务异常：{e}"),
            }
        });
    }

    /// 中断正在进行的索引爬取。
    pub fn stop_indexing(&self) {
        self.index_stop.store(true, Ordering::Relaxed);
    }

    /// 查询全局索引（同步、毫秒级）。
    pub fn global_search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        self.index.lock().search(query, limit).unwrap_or_default()
    }

    /// 索引中的文件总数。
    pub fn index_count(&self) -> usize {
        self.index.lock().count()
    }

    // ---- 文件预览 ----

    /// 预览单个文件 / 目录（同步读取，按需提取文本 / 图片路径 / 目录摘要）。
    pub fn preview(&self, path: &Path) -> Result<Preview, MoError> {
        mo_preview::preview_path(path)
    }

    /// 图片预览的**降采样副本**：返回 `Some` 时应加载它而不是原图。
    ///
    /// 超过长边上限的图会先按需生成一份缓存副本（`<缓存>/mo/preview`），
    /// 之后同路径直接命中磁盘、不再解码。详见
    /// [`mo_thumbnails::preview_scaled`]。
    ///
    /// **阻塞操作**（含解码），必须在 blocking 池调用。返回 `None` 一律表示
    /// 「用原图」——尺寸本来就够小、不是图片、或降采样失败；这是一项优化，
    /// 失败时静默回退，绝不让预览打不开。
    pub fn preview_image_scaled(&self, path: &Path) -> Option<PathBuf> {
        mo_thumbnails::preview_scaled(path, mo_thumbnails::PREVIEW_MAX_EDGE)
    }

    /// 全部工作流：配置里写的 + 扩展清单带的（`workflows` 字段），坏定义丢弃。
    pub fn workflows(&self) -> Vec<Workflow> {
        let mut out = workflows::sanitize(self.config().workflows);
        for e in self.extensions() {
            for w in e.manifest.workflows.clone() {
                let mut w = w;
                if w.source.is_none() {
                    w.source = Some(e.path.display().to_string());
                }
                if out.iter().any(|x| x.name == w.name) {
                    tracing::warn!("工作流「{}」重名，忽略扩展里的那份", w.name);
                    continue;
                }
                out.push(w);
            }
        }
        workflows::sanitize(out)
    }

    /// 顺序执行一个工作流（在 blocking 池跑，UI 期间可继续操作）。
    pub async fn run_workflow(
        &self,
        wf: Workflow,
        ctx: usercmds::CommandContext,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> workflows::WorkflowReport {
        self.spawn_blocking(move || workflows::run_workflow(&wf, &ctx, &cancel, |_| {}))
            .await
            .unwrap_or_default()
    }

    /// 某个目录的同步配对目标（未配对则 `None`）。
    pub fn sync_target(&self, src: &Path) -> Option<PathBuf> {
        self.config()
            .sync_pairs
            .get(&src.to_string_lossy().to_string())
            .map(PathBuf::from)
    }

    /// 记下 / 更新一个目录的同步配对目标；传 `None` 表示解除配对。
    pub fn set_sync_target(&self, src: &Path, dst: Option<&Path>) {
        let mut cfg = self.config();
        let key = src.to_string_lossy().to_string();
        match dst {
            Some(d) => {
                cfg.sync_pairs.insert(key, d.to_string_lossy().to_string());
            }
            None => {
                cfg.sync_pairs.remove(&key);
            }
        }
        self.save_config(&cfg);
    }

    /// 生成同步计划（**只读**，不改动任何文件）。
    ///
    /// 扫描整棵树可能耗时，所以放 blocking 池；UI 必须先拿到计划给用户过目，
    /// 点了执行才 apply。
    pub async fn sync_plan(
        &self,
        src: PathBuf,
        dst: PathBuf,
        opts: mo_operations::SyncOptions,
    ) -> Result<mo_operations::SyncPlan, String> {
        self.spawn_blocking(move || mo_operations::plan_sync(&src, &dst, opts))
            .await
            .map_err(|e| format!("生成计划的任务失败：{e}"))
    }

    /// 按计划执行同步，返回（执行报告，待清理的多余文件）。
    ///
    /// ⚠️ 删除**不在这里做**：blocking 线程里拿不到异步操作队列（也不该
    /// `block_on` 一个 tokio 锁），所以引擎只把「要清理哪些」交回来，由调用方
    /// 走 [`AppState::trash_paths`] 送回收站——既复用了撤销记录，也让删除
    /// 始终经过操作队列（有进度、可取消、可撤销）。
    pub async fn sync_apply(
        &self,
        src: PathBuf,
        dst: PathBuf,
        plan: mo_operations::SyncPlan,
    ) -> (mo_operations::SyncReport, Vec<PathBuf>) {
        let mut victims: Vec<PathBuf> = Vec::new();
        let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
        let collect = sink.clone();
        let report = self
            .spawn_blocking(move || {
                mo_operations::apply_sync_plan(&src, &dst, &plan, |victim| {
                    // 引擎承诺不做永久删除：这里只登记，不动文件。
                    collect.lock().unwrap().push(victim.to_path_buf());
                    Ok(())
                })
            })
            .await
            .unwrap_or_default();
        if let Ok(got) = Arc::try_unwrap(sink) {
            victims = got.into_inner().unwrap_or_default();
        }
        (report, victims)
    }

    /// 在 `root` 下查找重复文件（阻塞 IO 放 blocking 池，UI 期间可继续操作）。
    ///
    /// `cancel` 由调用方持有：整盘扫描可能跑几分钟，必须能中途停下。
    pub async fn find_duplicates(
        &self,
        root: PathBuf,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> mo_operations::DedupReport {
        self.spawn_blocking(move || mo_operations::find_duplicates(&[root], &cancel))
            .await
            .unwrap_or_default()
    }

    // ---- 文件 / 文件夹比较 ----

    /// 比较两个条目（自动选择文件比较或树比较）。
    ///
    /// 阻塞 IO 在 blocking 池执行，UI 可以在等待期间继续刷新。
    pub async fn compare_paths(
        &self,
        a: PathBuf,
        b: PathBuf,
    ) -> Result<mo_diff::Comparison, String> {
        self.spawn_blocking(move || mo_diff::compare(&a, &b))
            .await
            .map_err(|e| format!("比较任务失败：{e}"))?
    }

    // ---- 批量操作 ----

    /// 当前选中的条目路径（按选择模型）；若没有任何选中，回退到聚焦项。
    pub async fn selection_paths(&self) -> Vec<PathBuf> {
        let inner = self.inner.read().await;
        let Some(dir) = inner.directory.as_ref() else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = inner
            .selection
            .selected_ids()
            .iter()
            .filter_map(|id| {
                dir.entry_index(*id)
                    .and_then(|i| dir.entries.get(i))
                    .map(|e| e.path.clone())
            })
            .collect();
        if paths.is_empty() {
            if let Some(fid) = inner.selection.focused() {
                if let Some(i) = dir.entry_index(fid) {
                    if let Some(e) = dir.entries.get(i) {
                        paths.push(e.path.clone());
                    }
                }
            }
        }
        paths
    }

    /// 全选当前可见条目。
    pub async fn select_all_visible(&self) {
        let ids: Vec<FileId> = {
            let inner = self.inner.read().await;
            let Some(dir) = inner.directory.as_ref() else {
                return;
            };
            dir.view
                .visible_indices()
                .iter()
                .filter_map(|&i| dir.entries.get(i))
                .map(|e| e.id)
                .collect()
        };
        self.inner.write().await.selection.select_all(&ids);
    }

    /// 选择一段可见条目（Shift 连选），`from`/`to` 为可见下标。
    pub async fn select_range(&self, from: usize, to: usize) {
        let ids: Vec<FileId> = {
            let inner = self.inner.read().await;
            let Some(dir) = inner.directory.as_ref() else {
                return;
            };
            dir.view
                .visible_indices()
                .iter()
                .filter_map(|&i| dir.entries.get(i))
                .map(|e| e.id)
                .collect()
        };
        self.inner
            .write()
            .await
            .selection
            .select_range(&ids, from, to);
    }

    /// 当前选择集的快照（UI 以 app 侧为唯一事实来源，用它回灌本地缓存）。
    pub async fn selection_ids(&self) -> Vec<FileId> {
        self.inner
            .read()
            .await
            .selection
            .selected_ids()
            .iter()
            .copied()
            .collect()
    }

    /// 键盘 ↑↓ 移动焦点：`step` 为相对位移（-1 / +1），`extend` 为 Shift 连选。
    ///
    /// 返回移动后的可见下标（供 UI 滚动跟随 / Enter 打开时定位）。
    /// 焦点条目基于可见（已过滤 + 已排序）序列，与列表渲染顺序一致。
    pub async fn move_cursor(&self, step: isize, extend: bool) -> Option<usize> {
        let ids: Vec<FileId> = {
            let inner = self.inner.read().await;
            let dir = inner.directory.as_ref()?;
            dir.view
                .visible_indices()
                .iter()
                .filter_map(|&i| dir.entries.get(i))
                .map(|e| e.id)
                .collect()
        };
        if ids.is_empty() {
            return None;
        }

        let (focused, anchor) = {
            let inner = self.inner.read().await;
            (inner.selection.focused(), inner.selection.anchor())
        };
        let cur = focused.and_then(|f| ids.iter().position(|&i| i == f));
        let next = match cur {
            Some(i) => (i as isize + step).clamp(0, ids.len() as isize - 1) as usize,
            // 还没有焦点：↓ 聚焦第一项、↑ 聚焦最后一项（Finder / Explorer 习惯）。
            None => {
                if step < 0 {
                    ids.len() - 1
                } else {
                    0
                }
            }
        };

        if extend {
            let from = anchor
                .and_then(|a| ids.iter().position(|&i| i == a))
                .unwrap_or(next);
            self.select_range(from, next).await;
        } else {
            self.select(ids[next]).await;
        }
        Some(next)
    }

    /// 侧边栏快捷访问位置（存在才列出）。
    pub fn quick_locations(&self) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        if let Some(p) = dirs::home_dir() {
            out.push(("主目录".to_string(), p));
        }
        if let Some(p) = dirs::desktop_dir() {
            out.push(("桌面".to_string(), p));
        }
        if let Some(p) = dirs::document_dir() {
            out.push(("文档".to_string(), p));
        }
        if let Some(p) = dirs::download_dir() {
            out.push(("下载".to_string(), p));
        }
        if let Some(p) = dirs::picture_dir() {
            out.push(("图片".to_string(), p));
        }
        if let Some(p) = dirs::video_dir() {
            out.push(("影片".to_string(), p));
        }
        out
    }

    /// 删除选中（无选中则删除聚焦项）：本地条目移入回收站，远程条目删在服务端。
    ///
    /// 远程删除**不可撤销**（服务端没有回收站），失败时把错误返回给 UI 弹提示。
    pub async fn delete_selection(&self) -> Result<Vec<u64>, MoError> {
        let paths = self.selection_paths().await;
        self.delete_paths(paths).await
    }

    /// 删除一批路径：**逐条**判断该走哪条路（见 [`AppState::goes_through_remote`]）。
    ///
    /// * 本地条目 → 回收站（`trash_paths`，可撤销）；
    /// * 远程条目 → 远程后端的 `remove_file` / `remove_dir`（不可撤销）。
    pub async fn delete_paths(&self, paths: Vec<PathBuf>) -> Result<Vec<u64>, MoError> {
        let mut remote = Vec::new();
        let mut local = Vec::new();
        for p in paths {
            if self.goes_through_remote(&p).await {
                remote.push(p);
            } else {
                local.push(p);
            }
        }
        let ids = self.trash_paths(local).await;
        if !remote.is_empty() {
            self.delete_remote(remote).await?;
        }
        Ok(ids)
    }

    /// 删掉几条**远程**条目（服务端没有回收站，不可撤销）。
    ///
    /// 逐条删，第一条失败就收手并上报：剩下的多半同因（连接断了 / 没权限），
    /// 继续删只会刷出一串一样的错。删完（或失败后）都要重读——远程没有
    /// watcher，不重读的话列表里还留着已经不存在的条目。
    async fn delete_remote(&self, paths: Vec<PathBuf>) -> Result<(), MoError> {
        let fs = self.active_fs();
        for p in paths {
            let is_dir = self.entry_is_dir(&p).await.unwrap_or(false);
            let result = if is_dir {
                fs.remove_dir(&p).await
            } else {
                fs.remove_file(&p).await
            };
            if let Err(e) = result {
                let _ = self.refresh().await;
                return Err(e);
            }
            self.record_history("删除（远程）", vec![p], None);
        }
        self.refresh().await
    }

    /// 把**指定**路径逐个移入回收站（可撤销）。
    ///
    /// ⚠️ 这是**本机**回收站的路径：调用方给的路径必须是本地的。要「按路径自己
    /// 判断走哪条路」请用 [`AppState::delete_paths`]——去重 / 同步的待删副本
    /// 就是本地路径，它们照旧走这里，不该被当成远程条目发给服务器。
    ///
    /// 与 `delete_selection` 同一条流水线，只是不走选择模型——重复文件清理
    /// 要删的是「某个组里的其余副本」，跟当前选中项无关。
    pub async fn trash_paths(&self, paths: Vec<PathBuf>) -> Vec<u64> {
        let mut ids = Vec::new();
        for p in paths {
            let id = self.ops.lock().await.next_id();
            let op = TrashOperation::new(id, p.clone(), self.trash.clone());
            let hid = self.submit_operation(op).await;
            self.record_history("删除", vec![p.clone()], None);
            self.push_reversible(Reversible::Delete { original: p });
            ids.push(hid);
        }
        ids
    }

    /// 复制选中到 `dest` 目录（每个源按原名落到 dest 下）。
    pub async fn copy_selection(&self, dest: &Path) -> Vec<u64> {
        self.duplicate_selection(dest, false).await
    }

    /// 移动选中到 `dest` 目录。
    pub async fn move_selection(&self, dest: &Path) -> Vec<u64> {
        self.duplicate_selection(dest, true).await
    }

    async fn duplicate_selection(&self, dest: &Path, move_: bool) -> Vec<u64> {
        let paths = self.selection_paths().await;
        self.transfer(paths, dest, move_).await
    }

    /// 在同一目录内就地复制一批路径（Finder 的「复制」语义：`a.txt` → `a 2.txt`）。
    ///
    /// 不走 [`AppState::transfer`]：那里是「目标目录 + 沿用原名」，而这里源与目标同目录，
    /// 沿用原名会**覆盖源文件**——目标名必须先去重。
    pub async fn duplicate_paths(&self, paths: Vec<PathBuf>) -> Vec<u64> {
        let mut ids = Vec::new();
        for src in paths {
            let to = mo_operations::unique_path(&src);
            let id = self.ops.lock().await.next_id();
            let op: SharedOperation = CopyOperation::new(id, src.clone(), to.clone());
            let hid = self.submit_operation(op).await;
            self.record_history("复制", vec![src.clone()], None);
            self.push_reversible(Reversible::Copy { src, dest: to });
            ids.push(hid);
        }
        ids
    }

    /// 在 `dir` 下解析出一个**不冲突**的新名字（仅算路径，不落盘）。
    ///
    /// `name` 为空时用 `fallback`。注意**先判断目标是否存在再决定是否去重**：
    /// [`mo_operations::unique_path`] 总是从 ` 2` 起编号，直接拿它会把一个
    /// 本来不冲突的名字变成「新建文件夹 2」。
    ///
    /// 「存在」的判据走 [`AppState::name_taken`]（问列表 / 后端），不是
    /// `Path::exists()`——后者在远程目录里恒为 `false`。
    async fn free_path(&self, dir: &Path, name: &str, fallback: &str) -> PathBuf {
        let name = name.trim();
        let name = if name.is_empty() { fallback } else { name };
        let base = dir.join(name);
        if self.name_taken(&base).await {
            mo_operations::unique_path(&base)
        } else {
            base
        }
    }

    /// 在 `dir` 下新建文件夹，返回创建出的**真实路径**（重名时加序号，不覆盖）。
    ///
    /// `name` 为空时用「新建文件夹」。
    pub async fn create_folder(&self, dir: &Path, name: &str) -> Result<PathBuf, MoError> {
        let target = self.free_path(dir, name, "新建文件夹").await;
        self.active_fs().create_dir(&target).await?;
        Ok(target)
    }

    /// 在 `dir` 下新建**空文本文件**，返回创建出的**真实路径**（重名时加序号）。
    ///
    /// `name` 为空时用「新建文本.txt」。目标名同样走 [`AppState::free_path`] 去重，
    /// 底层 `write_file` 用的是 `create_new`——即便去重算错也不会覆盖已有文件。
    pub async fn create_file(&self, dir: &Path, name: &str) -> Result<PathBuf, MoError> {
        let target = self.free_path(dir, name, "新建文本.txt").await;
        self.active_fs().write_file(&target, b"").await?;
        Ok(target)
    }

    /// 把一批路径复制 / 移动到 `dest`（拖拽与剪贴板粘贴的公共实现）。
    ///
    /// 与 [`AppState::copy_selection`] 的区别：这里不读取当前选择，
    /// 而是用调用方给的一批路径——拖拽时拖的可能是「选中集合」，
    /// 也可能只是鼠标下那一行。
    pub async fn transfer(&self, paths: Vec<PathBuf>, dest: &Path, move_: bool) -> Vec<u64> {
        let mut ids = Vec::new();
        for src in paths {
            let name = src
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let to = dest.join(name);
            let id = self.ops.lock().await.next_id();
            let op: SharedOperation = if move_ {
                MoveOperation::new(id, src.clone(), to.clone())
            } else {
                CopyOperation::new(id, src.clone(), to.clone())
            };
            let hid = self.submit_operation(op).await;
            self.record_history(
                if move_ { "移动" } else { "复制" },
                vec![src.clone()],
                Some(dest.to_path_buf()),
            );
            self.push_reversible(if move_ {
                Reversible::Move {
                    from: src,
                    to: to.clone(),
                }
            } else {
                Reversible::Copy {
                    src,
                    dest: to.clone(),
                }
            });
            ids.push(hid);
        }
        ids
    }

    // ---- 操作历史 ----

    /// 记录一条操作历史（环形，最多保留 200 条）。
    pub fn record_history(&self, kind: &str, sources: Vec<PathBuf>, dest: Option<PathBuf>) {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let entry = HistoryEntry {
            kind: kind.to_string(),
            sources,
            dest,
            at,
        };
        let mut g = self.history.lock();
        g.push(entry);
        let len = g.len();
        if len > 200 {
            g.drain(0..len - 200);
        }
    }

    /// 操作历史快照（最新在前）。
    pub fn history_snapshot(&self) -> Vec<HistoryEntry> {
        self.history.lock().iter().rev().cloned().collect()
    }

    // ---- 撤销 / 重做 ----

    /// 把一条「可撤销操作」压入撤销栈，并清空重做栈（新操作使已撤销的重做失效）。
    fn push_reversible(&self, r: Reversible) {
        self.undo_stack.lock().push(r);
        self.redo_stack.lock().clear();
    }

    /// 是否可撤销。
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.lock().is_empty()
    }

    /// 是否可重做。
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.lock().is_empty()
    }

    /// 撤销栈顶操作的简短描述（如「撤销 移动」），无可撤销时返回 `None`。
    pub fn undo_label(&self) -> Option<String> {
        self.undo_stack
            .lock()
            .last()
            .map(|r| format!("撤销 {}", r.kind_label()))
    }

    /// 重做栈顶操作的简短描述（如「重做 复制」），无可重做时返回 `None`。
    pub fn redo_label(&self) -> Option<String> {
        self.redo_stack
            .lock()
            .last()
            .map(|r| format!("重做 {}", r.kind_label()))
    }

    /// 撤销最近一次操作：弹出撤销栈，异步提交其逆操作，并压入重做栈。
    pub fn undo(&self) {
        let r = self.undo_stack.lock().pop();
        if let Some(r) = r {
            let app = self.clone();
            let captured = r.clone();
            self.spawn(async move {
                app.apply_reversible(&captured, true).await;
            });
            self.redo_stack.lock().push(r);
        }
    }

    /// 重做最近一次被撤销的操作：弹出重做栈，异步提交其正向操作，并压回撤销栈。
    pub fn redo(&self) {
        let r = self.redo_stack.lock().pop();
        if let Some(r) = r {
            let app = self.clone();
            let captured = r.clone();
            self.spawn(async move {
                app.apply_reversible(&captured, false).await;
            });
            self.undo_stack.lock().push(r);
        }
    }

    // ---- 平台原生集成（mo-platform）----

    /// 在系统的文件管理器里**显示**一条路径（macOS 叫「在访达中显示」）。
    ///
    /// * 只认**本地**路径：远程条目（`/pub/x`）在系统文件管理器里根本不存在，传
    ///   过去只会静默失败，所以这里直接挡掉。判据是 [`AppState::goes_through_remote`]
    ///   ——「这条路径属不属于当前后端」，**不是**「我现在在看远程吗」：去重 / 同步
    ///   拿的是本地路径，而它们经常在远程页上被调用（与 `delete_paths` 同一条纪律）。
    /// * 多选时只定位第一条——访达一次也只能选中一处，与系统行为一致。
    pub async fn reveal_in_file_manager(&self, paths: Vec<PathBuf>) -> Result<(), MoError> {
        let Some(first) = paths.into_iter().next() else {
            return Ok(());
        };
        if self.goes_through_remote(&first).await {
            return Err(MoError::Other(
                "远程条目没法在系统文件管理器里显示".to_string(),
            ));
        }
        // AppKit 那条路是同步的、还可能在主线程弹 UI，必须放 blocking 池。
        self.spawn_blocking(move || mo_platform::reveal(&first))
            .await
            .map_err(|e| MoError::Other(format!("显示任务失败：{e}")))?
            .map_err(|e| MoError::Other(e.to_string()))
    }

    /// 把本地文件交给**系统**废纸篓（macOS 上就是访达那份）。
    ///
    /// 与 [`AppState::trash_paths`]（Mo 自己的回收站）是**两条路**：
    ///
    /// | | Mo 回收站 | 系统废纸篓（这里） |
    /// |---|---|---|
    /// | 撤销 / 回收站面板 | 支持 | 不支持（交出去就由系统负责） |
    /// | 同宗卷删除 | 搬文件（大文件慢） | O(1) 重命名 |
    /// | 外接卷宗 / 网络盘 | 搬回本机（慢，可能失败） | 就地进卷宗自己的废纸篓 |
    ///
    /// 所以它是**显式动作**（右键 / 命令面板），不接在 ⌘⌫ 上——默认删除仍走
    /// Mo 回收站，那才是「可撤销、面板里看得见」的那条路。
    pub async fn recycle_to_system(&self, paths: Vec<PathBuf>) -> Result<(), MoError> {
        if paths.is_empty() {
            return Ok(());
        }
        // 同样按「这条路径属于哪个后端」过滤：远程条目走不了这条路（它在本机不存在），
        // 但**本地**路径（去重 / 同步送来的副本）在看远程时也要照办。
        let mut local = Vec::with_capacity(paths.len());
        for p in paths {
            if !self.goes_through_remote(&p).await {
                local.push(p);
            }
        }
        if local.is_empty() {
            return Err(MoError::Other(
                "远程条目没法进本机废纸篓（请用「删除」）".to_string(),
            ));
        }
        let paths = local;
        let history = paths.clone();
        let r = self
            .spawn_blocking(move || mo_platform::recycle(&paths))
            .await
            .map_err(|e| MoError::Other(format!("删除任务失败：{e}")))?
            .map_err(|e| MoError::Other(e.to_string()));
        if r.is_ok() {
            self.record_history("移到系统废纸篓", history, None);
        }
        // 刷新失败不该盖掉删除结果（列表没跟上用户手动刷一下就是了）。
        let _ = self.refresh().await;
        r
    }

    // ---- 回收站 ----

    /// 回收站条目快照（最新在前）。
    pub fn trash_list(&self) -> Vec<TrashEntry> {
        self.trash.list()
    }

    /// 回收站条目数。
    pub fn trash_count(&self) -> usize {
        self.trash.count()
    }

    /// 还原一条回收站记录（异步提交 `RestoreOperation`）。
    pub async fn restore_trash_entry(&self, original: PathBuf) {
        let id = self.ops.lock().await.next_id();
        let op = RestoreOperation::new(id, original, self.trash.clone());
        self.submit_operation(op).await;
    }

    /// 永久删除某条回收站记录（阻塞 IO 放 blocking 池），完成后广播 `TrashChanged`。
    pub fn purge_trash_entry(&self, entry: TrashEntry) {
        let app = self.clone();
        let bus = self.bus.clone();
        let trash = self.trash.clone();
        self.spawn(async move {
            let _ = app.spawn_blocking(move || trash.purge(&entry)).await;
            bus.publish(AppEvent::TrashChanged);
        });
    }

    /// 清空回收站，完成后广播 `TrashChanged`。
    pub fn empty_trash(&self) {
        let app = self.clone();
        let bus = self.bus.clone();
        let trash = self.trash.clone();
        self.spawn(async move {
            let _ = app.spawn_blocking(move || trash.empty()).await;
            bus.publish(AppEvent::TrashChanged);
        });
    }

    /// 执行一条可逆操作的正向（inverse=false）或逆向（inverse=true）版本，提交到操作队列。
    async fn apply_reversible(&self, r: &Reversible, inverse: bool) {
        // 「移动」这条逆操作在**远程**条目上是反向 `rename`（`MoveOperation` 是本机
        // 管线，拿来撤销一次远程重命名会静默什么都不做）。
        if let Reversible::Move { from, to } = r {
            let (a, b) = if inverse {
                (to.clone(), from.clone())
            } else {
                (from.clone(), to.clone())
            };
            if self.goes_through_remote(&a).await {
                // 撤销 / 重做这条路不返回结果（与本机管线一致），失败只能记日志；
                // 但**必须重读**：远端改名成没成，只有列表说了算。
                if let Err(e) = self.active_fs().rename(&a, &b).await {
                    tracing::warn!("远程撤销重命名失败 {} → {}：{e}", a.display(), b.display());
                }
                let _ = self.refresh().await;
                return;
            }
        }
        let id = self.ops.lock().await.next_id();
        let op: SharedOperation = match r {
            Reversible::Move { from, to } => {
                let (a, b) = if inverse {
                    (to.clone(), from.clone())
                } else {
                    (from.clone(), to.clone())
                };
                MoveOperation::new(id, a, b)
            }
            Reversible::Copy { src, dest } => {
                if inverse {
                    TrashOperation::new(id, dest.clone(), self.trash.clone())
                } else {
                    CopyOperation::new(id, src.clone(), dest.clone())
                }
            }
            Reversible::Delete { original } => {
                if inverse {
                    RestoreOperation::new(id, original.clone(), self.trash.clone())
                } else {
                    TrashOperation::new(id, original.clone(), self.trash.clone())
                }
            }
        };
        self.submit_operation(op).await;
    }
}

/// 一条「可撤销操作」的描述。只保存路径，运行时再据其构造正 / 逆操作。
///
/// 这样无论撤销 / 重做多少次，都无需持有已移入回收站的 `TrashEntry` 生命周期。
#[derive(Debug, Clone)]
pub enum Reversible {
    /// 移动：from → to。逆操作为 to → from。
    Move { from: PathBuf, to: PathBuf },
    /// 复制：src → dest。逆操作为把 dest 移入回收站。
    Copy { src: PathBuf, dest: PathBuf },
    /// 删除（已入回收站）：original。逆操作为按原路径从回收站还原。
    Delete { original: PathBuf },
}

impl Reversible {
    /// 操作类型的中文标签（用于 UI 提示）。
    pub fn kind_label(&self) -> &'static str {
        match self {
            Reversible::Move { .. } => "移动",
            Reversible::Copy { .. } => "复制",
            Reversible::Delete { .. } => "删除",
        }
    }
}

/// 一条操作历史记录（轻量，仅用于「操作历史」展示）。
///
/// 可撤销性由 [`Reversible`] 单独承载，这里只做展示用途。
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// 操作类型：删除 / 复制 / 移动。
    pub kind: String,
    /// 源路径。
    pub sources: Vec<PathBuf>,
    /// 目标目录（复制 / 移动时）。
    pub dest: Option<PathBuf>,
    /// 发生时刻（Unix 秒）。
    pub at: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- 标签与书签

/// 可用标签颜色（Finder 标签 nc 七色）。
pub const TAG_COLORS: [(&str, &str); 7] = [
    ("red", "红"),
    ("orange", "橙"),
    ("yellow", "黄"),
    ("green", "绿"),
    ("blue", "蓝"),
    ("purple", "紫"),
    ("gray", "灰"),
];

/// 客户端剪贴板：记住一批路径以及「剪切（移动）」还是「复制」。
///
/// 与系统剪贴板无关：这里只在同一应用内传递文件引用，
/// 粘贴时才真正落到操作队列（见 [`AppState::paste_clipboard`]）。
#[derive(Debug, Clone)]
pub struct Clipboard {
    pub paths: Vec<PathBuf>,
    /// true = 剪切（粘贴后移动），false = 复制。
    pub cut: bool,
}

/// 磁盘用量的一行结果（某个子树的汇总）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirUsage {
    pub path: PathBuf,
    /// 递归总大小（字节）。
    pub size: u64,
    pub files: usize,
    pub dirs: usize,
}

impl AppState {
    /// 配置文件路径（`~/Library/Application Support/mo/config.json` 等）。
    ///
    /// 设了 `MO_CONFIG_DIR` 环境变量时用该目录——测试靠它把配置钉到临时路径，
    /// 否则测试会读到开发者机器上的真实配置（视图模式、侧边栏开关都会改变
    /// 渲染结构，导致断言在别人机器上莫名失败）。便携部署也认这个变量。
    fn config_path() -> PathBuf {
        if let Ok(dir) = std::env::var("MO_CONFIG_DIR") {
            return PathBuf::from(dir).join("config.json");
        }
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("mo")
            .join("config.json")
    }

    /// 读取配置；读不到就用默认配置（缓存不可用不影响启动）。
    pub fn config(&self) -> mo_config::Config {
        mo_config::Config::load(&Self::config_path()).unwrap_or_default()
    }

    fn save_config(&self, cfg: &mo_config::Config) {
        if let Err(e) = cfg.save(&Self::config_path()) {
            tracing::warn!("配置保存失败：{e}");
        }
    }

    /// 当前主题名：`light` / `dark` / `system`（跟随系统）/ 自定义主题 key。
    /// 配置里留空时按浅色处理。
    pub fn theme_setting(&self) -> String {
        let t = self.config().theme;
        if t.is_empty() {
            "light".to_string()
        } else {
            t
        }
    }

    /// 切换主题并持久化。
    pub fn set_theme(&self, name: &str) {
        let mut cfg = self.config();
        cfg.theme = name.to_string();
        self.save_config(&cfg);
    }

    /// 自定义主题表（配置文件的 `custom_themes`）。
    pub fn custom_themes(&self) -> std::collections::HashMap<String, ThemeColors> {
        self.config().custom_themes
    }

    /// 新增 / 覆盖一个自定义主题（供「另存为自定义主题」与配置写回使用）。
    pub fn set_custom_theme(&self, name: &str, colors: ThemeColors) {
        let mut cfg = self.config();
        cfg.custom_themes.insert(name.to_string(), colors);
        self.save_config(&cfg);
    }

    /// 删除自定义主题；若它正是当前主题则退回浅色。
    pub fn remove_custom_theme(&self, name: &str) {
        let mut cfg = self.config();
        cfg.custom_themes.remove(name);
        if cfg.theme == name {
            cfg.theme = "light".to_string();
        }
        self.save_config(&cfg);
    }

    /// 列布局偏好（顺序 + 宽度）。
    pub fn column_prefs(&self) -> ColumnPrefs {
        self.config().columns
    }

    /// 保存列布局（拖动列序 / 调整列宽后调用）。
    pub fn save_column_prefs(&self, prefs: ColumnPrefs) {
        let mut cfg = self.config();
        cfg.columns = prefs;
        self.save_config(&cfg);
    }

    /// 界面布局偏好（侧边栏 / 状态栏 / 斑马纹 / 默认视图）。
    pub fn ui_prefs(&self) -> UiPrefs {
        self.config().ui
    }

    /// 逐项改界面偏好并持久化（第四阶段·自定义布局）。
    pub fn set_ui_prefs(&self, prefs: UiPrefs) {
        let mut cfg = self.config();
        cfg.ui = prefs;
        self.save_config(&cfg);
    }

    /// 快捷键覆盖表：动作 id → 键串（空串 = 解绑）。
    pub fn keybindings(&self) -> std::collections::HashMap<String, String> {
        self.config().keybindings
    }

    /// 改一个动作的键位并持久化（`spec` 为空串表示解绑）。
    pub fn set_keybinding(&self, id: &str, spec: &str) {
        let mut cfg = self.config();
        cfg.keybindings.insert(id.to_string(), spec.to_string());
        self.save_config(&cfg);
    }

    /// 恢复某个动作的默认键位（删掉覆盖项）。
    pub fn clear_keybinding(&self, id: &str) {
        let mut cfg = self.config();
        cfg.keybindings.remove(id);
        self.save_config(&cfg);
    }

    /// 全部快捷键回到默认。
    pub fn reset_keybindings(&self) {
        let mut cfg = self.config();
        cfg.keybindings.clear();
        self.save_config(&cfg);
    }

    /// 恢复默认布局：清掉列偏好 + 界面开关回到全开、默认列表视图。
    pub fn reset_layout(&self) {
        let mut cfg = self.config();
        cfg.columns = ColumnPrefs::default();
        cfg.ui = UiPrefs::default();
        self.save_config(&cfg);
    }

    /// 用户自定义命令：配置里写的 + `commands/*.json` 清单 + 启用的扩展。
    ///
    /// 清单与扩展目录都只认**自己的配置目录**，绝不扫描正在浏览的目录——否则
    /// 打开别人给的文件夹就等于跑了它带的脚本。同名命令保留先加载的那条并告警。
    /// `selected_exts` 是选中项的扩展名（小写含点），供扩展的 `when_ext` 条件用。
    pub fn user_commands(&self, selected_exts: &[String]) -> Vec<UserCommand> {
        let cfg = self.config();
        let mut out: Vec<UserCommand> = cfg
            .commands
            .iter()
            .filter(|c| {
                let bad = usercmds::validate(c).is_some();
                if bad {
                    tracing::warn!("配置里有一条自定义命令不合法，已忽略");
                }
                !bad
            })
            .cloned()
            .collect();
        for c in usercmds::load_manifests(&Self::commands_dir()) {
            if out.iter().any(|e| e.name == c.name) {
                tracing::warn!(
                    "命令「{}」重名，忽略 {}",
                    c.name,
                    c.source.unwrap_or_default()
                );
                continue;
            }
            out.push(c);
        }
        for c in extensions::flatten(&self.extensions(), selected_exts) {
            if out.iter().any(|e| e.name == c.name) {
                tracing::warn!(
                    "命令「{}」重名，忽略 {}",
                    c.name,
                    c.source.clone().unwrap_or_default()
                );
                continue;
            }
            out.push(c);
        }
        out
    }

    /// 已加载的扩展（`<配置目录>/mo/extensions/<id>/manifest.json`）。
    pub fn extensions(&self) -> Vec<extensions::Extension> {
        extensions::load(&extensions::extensions_root(&Self::config_path()))
    }

    /// 启用 / 停用某个扩展：改写它自己清单里的 `enabled`。
    pub fn set_extension_enabled(&self, id: &str, on: bool) -> Result<(), String> {
        let ext = self
            .extensions()
            .into_iter()
            .find(|e| e.manifest.id == id)
            .ok_or_else(|| format!("找不到扩展「{id}」"))?;
        let text = std::fs::read_to_string(&ext.path).map_err(|e| e.to_string())?;
        let mut m: extensions::Manifest = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        m.enabled = on;
        std::fs::write(
            &ext.path,
            serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }

    /// 命令清单目录（`<配置目录>/mo/commands`，受 `MO_CONFIG_DIR` 影响）。
    pub fn commands_dir() -> PathBuf {
        Self::config_path()
            .parent()
            .map(|p| p.join("commands"))
            .unwrap_or_else(|| PathBuf::from("commands"))
    }

    /// 执行一条用户自定义命令（占位符展开 + 平台 shell + 输出捕获）。
    ///
    /// 返回 `(是否可执行, 结果)`：占位符缺上下文时不执行，避免把
    /// `rm {file}` 跑成 `rm`（少了参数就等于对空目标动手）。
    pub async fn run_user_command(
        &self,
        cmd: UserCommand,
        ctx: usercmds::CommandContext,
    ) -> Result<(String, usercmds::CommandOutput), String> {
        let (line, ok) = usercmds::expand(&cmd.shell, &ctx);
        if !ok {
            return Err("这条命令需要先在右侧列表里选中条目（用到 {file} / {files}）".to_string());
        }
        let cwd = ctx.dir.clone();
        let shown = line.clone();
        // ⚠️ 必须走 `spawn_blocking`（进程级 runtime）：这里的 `await` 跑在
        // GPUI 执行器上，直接 `tokio::spawn` 会 panic「no reactor running」。
        self.spawn_blocking(move || usercmds::run(&line, cwd.as_deref()))
            .await
            .map_err(|e| format!("命令任务失败：{e}"))
            .and_then(|r| r.map(|out| (shown, out)))
    }

    /// 侧边栏书签（含用户自己加的，去重且只保留仍在的目录）。
    pub fn bookmarks(&self) -> Vec<PathBuf> {
        self.config()
            .sidebar_bookmarks
            .iter()
            .map(PathBuf::from)
            .collect()
    }

    /// 加入书签（已在列表里则不重复添加）。
    pub fn add_bookmark(&self, path: PathBuf) {
        let mut cfg = self.config();
        let s = path.to_string_lossy().to_string();
        if cfg.sidebar_bookmarks.contains(&s) {
            return;
        }
        cfg.sidebar_bookmarks.push(s);
        self.save_config(&cfg);
    }

    /// 移除书签。
    pub fn remove_bookmark(&self, path: &Path) {
        let mut cfg = self.config();
        let s = path.to_string_lossy().to_string();
        cfg.sidebar_bookmarks.retain(|b| b != &s);
        self.save_config(&cfg);
    }

    /// 全部标签：`路径 → 颜色名`。
    pub fn tags(&self) -> std::collections::HashMap<PathBuf, String> {
        self.config()
            .tags
            .iter()
            .map(|(k, v)| (PathBuf::from(k), v.clone()))
            .collect()
    }

    /// 给一个路径设置颜色标签；颜色名为空表示清除标签。
    /// 查某个路径的颜色标签（没有则 `None`），供列表渲染色点。
    pub fn tag_of(&self, path: &Path) -> Option<String> {
        let key = path.to_string_lossy().to_string();
        self.config()
            .tags
            .get(&key)
            .cloned()
            .filter(|c| !c.is_empty())
    }

    pub fn set_tag(&self, path: PathBuf, color: String) {
        let mut cfg = self.config();
        let key = path.to_string_lossy().to_string();
        if color.is_empty() {
            cfg.tags.remove(&key);
        } else {
            cfg.tags.insert(key, color);
        }
        self.save_config(&cfg);
    }

    /// 在当前目录打开终端（平台差异见实现）。
    ///
    /// 各平台的候选终端按优先级排列，逐个尝试直到 `spawn` 成功；
    /// 全部失败时带上最后一个 OS 错误，方便排查「装了终端但拉不起来」。
    pub fn open_terminal(&self, dir: &Path) -> Result<(), MoError> {
        let plans = terminal_plans(dir);
        let mut last = None;
        for (prog, args) in plans {
            match std::process::Command::new(&prog).args(&args).spawn() {
                Ok(_) => return Ok(()),
                Err(e) => last = Some(e),
            }
        }
        Err(MoError::Other(format!(
            "未能打开终端：{}",
            last.map(|e| e.to_string())
                .unwrap_or_else(|| "没有可用的终端程序".to_string())
        )))
    }
}

/// 各平台打开终端的候选命令（程序 + 参数），按优先级排列。
///
/// macOS 先检查 `.app` 是否真的存在，避免 `open -a iTerm` 在未安装时
/// 静默弹出「找不到应用」的系统对话框。
fn terminal_plans(dir: &Path) -> Vec<(String, Vec<String>)> {
    let dir = dir.display().to_string();
    #[cfg(target_os = "macos")]
    {
        ["iTerm", "Terminal"]
            .into_iter()
            .filter(|app| {
                let p1 = format!("/Applications/{app}.app");
                let p2 = format!("/System/Applications/{app}.app");
                std::path::Path::new(&p1).exists() || std::path::Path::new(&p2).exists()
            })
            .map(|app| {
                (
                    "open".to_string(),
                    vec!["-a".to_string(), app.to_string(), dir.clone()],
                )
            })
            .collect()
    }
    #[cfg(target_os = "windows")]
    {
        vec![(
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "start".to_string(),
                "cmd".to_string(),
                "/K".to_string(),
                "cd".to_string(),
                "/D".to_string(),
                dir,
            ],
        )]
    }
    #[cfg(target_os = "linux")]
    {
        [
            "x-terminal-emulator",
            "gnome-terminal",
            "konsole",
            "kitty",
            "alacritty",
            "xterm",
        ]
        .into_iter()
        .map(|t| (t.to_string(), Vec::new()))
        .collect()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = dir;
        Vec::new()
    }
}

impl AppState {
    // ------------------------------------------------------------ 客户端剪贴板

    /// 把当前选择复制进内部剪贴板（`cut = false`）。
    pub async fn copy_selection_to_clipboard(&self) {
        let paths = self.selection_paths().await;
        if paths.is_empty() {
            return;
        }
        *self.clipboard.lock().await = Some(Clipboard { paths, cut: false });
    }

    /// 把当前选择**剪切**进内部剪贴板（`cut = true`）。
    pub async fn cut_selection_to_clipboard(&self) {
        let paths = self.selection_paths().await;
        if paths.is_empty() {
            return;
        }
        *self.clipboard.lock().await = Some(Clipboard { paths, cut: true });
    }

    /// 粘贴：`dest` 为空时粘贴到当前目录。
    pub async fn paste_clipboard(&self, dest: Option<PathBuf>) -> Vec<u64> {
        let clip = self.clipboard.lock().await.clone();
        let Some(clip) = clip else {
            return Vec::new();
        };
        let dest = match dest.or(self.current_path().await) {
            Some(d) => d,
            None => return Vec::new(),
        };
        let ids = if clip.cut {
            // 逐个源项移动到目标目录下的同名位置，并记入历史 / 撤销栈，
            // 这样「剪切 → 粘贴」也能被 ⌘Z 撤销（与 UI 里的移动走同一路径）。
            let mut ids = Vec::new();
            for src in &clip.paths {
                let name = src
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let to = dest.join(name);
                let id = self.ops.lock().await.next_id();
                let op: SharedOperation = MoveOperation::new(id, src.clone(), to.clone());
                ids.push(self.submit_operation(op).await);
                self.record_history("移动", vec![src.clone()], Some(dest.clone()));
                self.push_reversible(Reversible::Move {
                    from: src.clone(),
                    to: to.clone(),
                });
            }
            ids
        } else {
            self.copy_selection(&dest).await
        };
        // 剪切是一次性消耗品：粘贴后清空，避免二次粘贴重复执行。
        if clip.cut {
            *self.clipboard.lock().await = None;
        }
        ids
    }

    /// 磁盘用量分析：统计 `root` 的**每个直接子目录**的递归大小。
    ///
    /// 递归全在 blocking 池里完成——几万个文件的深度遍历若在 async worker
    /// 上做，会把 UI 的补窗任务排在后面（大目录滚动闪烁就是这么来的）。
    pub async fn analyze_usage(&self, root: PathBuf) -> Result<Vec<DirUsage>, MoError> {
        let entries = self.list_dir(&root).await.unwrap_or_default();
        let roots: Vec<PathBuf> = entries.into_iter().map(|e| e.path).collect();
        self.spawn_blocking(move || {
            let mut out = Vec::with_capacity(roots.len());
            for p in roots {
                let mut usage = DirUsage {
                    path: p.clone(),
                    size: 0,
                    files: 0,
                    dirs: 0,
                };
                if p.is_dir() {
                    sum_dir(&p, &mut usage);
                } else if let Ok(meta) = std::fs::metadata(&p) {
                    usage.size = meta.len();
                    usage.files = 1;
                }
                out.push(usage);
            }
            out.sort_by_key(|u| std::cmp::Reverse(u.size));
            Ok(out)
        })
        .await
        .map_err(|e| MoError::Other(format!("磁盘分析的后台任务失败：{e}")))?
    }
}

/// 递归累加一个目录的大小（符号链接不跟随，避免环）。
fn sum_dir(path: &Path, usage: &mut DirUsage) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            usage.dirs += 1;
            sum_dir(&entry.path(), usage);
        } else {
            usage.files += 1;
            usage.size += meta.len();
        }
    }
}

// ---------------------------------------------------------------- 新增能力入口

impl AppState {
    /// 创建链接：`link` 是指向 `target` 的链接路径。
    /// 为当前选中项在同目录创建软 / 硬链接（`-符号链接` / `-硬链接` 后缀）。
    ///
    /// 硬链接对目录无效，失败项跳过；返回成功创建的个数。
    pub async fn create_links(&self, hard: bool) -> usize {
        let paths = self.selection_paths().await;
        let suffix = if hard { "硬链接" } else { "符号链接" };
        let mut ok = 0usize;
        for src in paths {
            let Some(parent) = src.parent().map(|p| p.to_path_buf()) else {
                continue;
            };
            let name = src
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let link = match src.extension() {
                Some(ext) => {
                    let stem = src
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    parent.join(format!("{stem}-{suffix}.{}", ext.to_string_lossy()))
                }
                None => parent.join(format!("{name}-{suffix}")),
            };
            if link.exists() {
                continue;
            }
            self.create_link(src, link, hard).await;
            ok += 1;
        }
        ok
    }

    pub async fn create_link(&self, target: PathBuf, link: PathBuf, hard: bool) -> u64 {
        let id = self.ops.lock().await.next_id();
        let kind = if hard {
            LinkKind::Hardlink
        } else {
            LinkKind::Symlink
        };
        self.submit_operation(LinkOperation::new(id, target, link, kind))
            .await
    }

    /// 修改权限位（如 0o644）；阻塞调用走 blocking 池。
    pub async fn set_permissions(&self, path: PathBuf, mode: u32) -> Result<(), MoError> {
        self.spawn_blocking(move || mo_operations::set_permissions(&path, mode))
            .await
            .map_err(|e| MoError::Other(format!("权限修改任务失败：{e}")))?
    }

    /// 批量重命名：`pairs` 为 (原路径, 新路径)，逐个提交重命名操作。
    ///
    /// 远程条目走远程后端的 `rename`（判据见 [`AppState::goes_through_remote`]），
    /// 本地条目走操作队列——重命名一条远程路径若交给本机管线，只会「成功」地
    /// 什么都不做（那个路径在本机不存在）。
    ///
    /// 逆操作是「把新名改回旧名」，因此每一种情况都能被 ⌘Z 撤销
    /// （撤销同样按路径分流，见 [`AppState::apply_reversible`]）。
    pub async fn rename_many(&self, pairs: Vec<(PathBuf, PathBuf)>) -> Result<Vec<u64>, MoError> {
        let mut ids = Vec::new();
        let mut touched_remote = false;
        for (from, to) in pairs {
            if from == to {
                continue;
            }
            if self.goes_through_remote(&from).await {
                self.active_fs().rename(&from, &to).await?;
                self.record_history("重命名", vec![from.clone()], Some(to.clone()));
                self.push_reversible(Reversible::Move {
                    from: to.clone(),
                    to: from.clone(),
                });
                touched_remote = true;
                continue;
            }
            let id = self.ops.lock().await.next_id();
            let op = RenameOperation::new(id, from.clone(), to.clone());
            ids.push(self.submit_operation(op).await);
            self.record_history("重命名", vec![from.clone()], Some(to.clone()));
            self.push_reversible(Reversible::Move {
                from: to.clone(),
                to: from.clone(),
            });
        }
        // 远程没有 watcher：改完名得重读一次，否则列表里还是旧名字。
        if touched_remote {
            self.refresh().await?;
        }
        Ok(ids)
    }

    /// 压缩：把 `sources` 打包到 `dest`（格式按后缀推断）。
    pub async fn create_archive(
        &self,
        dest: PathBuf,
        sources: Vec<PathBuf>,
    ) -> Result<(), MoError> {
        self.spawn_blocking(move || mo_operations::create_archive(&dest, &sources))
            .await
            .map_err(|e| MoError::Other(format!("压缩任务失败：{e}")))?
    }

    /// 解压：把 `archive` 解到 `dest`，返回解出的条目数。
    pub async fn extract_archive(&self, archive: PathBuf, dest: PathBuf) -> Result<usize, MoError> {
        self.spawn_blocking(move || mo_operations::extract_archive(&archive, &dest))
            .await
            .map_err(|e| MoError::Other(format!("解压任务失败：{e}")))?
    }
}
