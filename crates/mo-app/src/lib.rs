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
mod metadata;
mod thumbnail;

pub use controller::DirectoryController;
pub use metadata::MetadataScheduler;
pub use thumbnail::ThumbnailScheduler;

use std::future::Future;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mo_cache::MetadataCache;
use mo_core::{
    AppEvent, Directory, Entry, EventBus, FileId, FileMetadata, LightEntry, MetadataState, MoError,
    NavigationState, SelectionModel, SortKey, ThumbnailState,
};
use mo_fs::{entry_at, FileSystem, FileSystemWatcher, LocalFileSystem, WatcherEvent};
use mo_operations::{
    CopyOperation, LinkKind, LinkOperation, MoveOperation, OperationHandle, OperationManager,
    RenameOperation, RestoreOperation, SharedOperation, Trash, TrashEntry, TrashOperation,
};
use mo_preview::Preview;
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
    fs: Arc<dyn FileSystem>,
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
    /// 全局搜索索引（内存 SQLite）。跨目录搜索的数据源。
    index: Arc<PlMutex<FileIndex>>,
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
    pub fn with_trash(trash_root: PathBuf) -> Self {
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
            fs: Arc::new(LocalFileSystem),
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
            index_stop: Arc::new(AtomicBool::new(false)),
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

    /// 底层文件系统抽象。
    pub fn file_system(&self) -> &Arc<dyn FileSystem> {
        &self.fs
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
        let fs = self.fs.clone();
        let p = path.to_path_buf();
        let cache = self.cache();
        let dir_id = FileId::synthetic(path);

        // 阻塞部分：读目录 + 缓存预填 + 排序，全部在 blocking 线程完成。
        let (dir, for_verify) = self
            .spawn_blocking(move || -> Result<(Directory, Vec<Entry>), MoError> {
                let raw = fs.read_dir_blocking(&p)?;
                let mut dir = Directory::new(dir_id, p);
                dir.set_entries(
                    raw.into_iter()
                        .map(|r| Entry::new(r.id, r.name, r.kind, r.path))
                        .collect(),
                );
                if let Some(c) = cache.as_ref() {
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
            .map_err(|e| MoError::Other(format!("读取目录的任务失败：{e}")))??;

        let first_screen = dir.visible_count().min(200);

        {
            let mut inner = self.inner.write().await;
            let mut sel = inner.selection.clone();
            sel.clear();
            inner.directory = Some(dir);
            inner.selection = sel;
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
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    return self.open_directory(parent).await;
                }
            }
        }
        Ok(())
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
                    if let Ok(meta) = self.fs.metadata(&path).await {
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
        let fs = self.fs.clone();
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

    /// 设置排序方式。
    pub async fn set_sort(&self, key: SortKey) {
        {
            let mut inner = self.inner.write().await;
            if let Some(dir) = inner.directory.as_mut() {
                dir.set_sort(key);
            }
        }
        self.publish_dir_changed().await;
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
        let fs = self.fs.clone();
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
            out.push(("🏠 主目录".to_string(), p));
        }
        if let Some(p) = dirs::desktop_dir() {
            out.push(("🖥 桌面".to_string(), p));
        }
        if let Some(p) = dirs::document_dir() {
            out.push(("📄 文档".to_string(), p));
        }
        if let Some(p) = dirs::download_dir() {
            out.push(("📥 下载".to_string(), p));
        }
        if let Some(p) = dirs::picture_dir() {
            out.push(("🖼 图片".to_string(), p));
        }
        out
    }

    /// 删除选中（无选中则删除聚焦项）：移入回收站（非永久删除），逐条提交到操作队列。
    ///
    /// 因为走回收站，删除天然可撤销——后续 `undo()` 会按原路径从回收站还原。
    pub async fn delete_selection(&self) -> Vec<u64> {
        let paths = self.selection_paths().await;
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
    fn config_path() -> PathBuf {
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
    /// 逆操作是「把新名改回旧名」，因此每一种情况都能被 ⌘Z 撤销。
    pub async fn rename_many(&self, pairs: Vec<(PathBuf, PathBuf)>) -> Vec<u64> {
        let mut ids = Vec::new();
        for (from, to) in pairs {
            if from == to {
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
        ids
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
