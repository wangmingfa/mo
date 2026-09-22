//! 系统图标：把「问系统要图标」从渲染路径上挪到后台。
//!
//! ## 为什么必须挪
//!
//! 「问系统拿一张图标」的完整代价是：`NSWorkspace.iconForFile:` → 新建 40px 画布
//! 重绘 → TIFF → PNG 编码 → 写盘。实测**每张 1.5–12ms**（普通文件 1.5ms，`.app`
//! 这种要读自己图标的包 12ms 起，进程内第一张还要额外几十毫秒预热）。
//!
//! 而列表渲染是**渲染线程**上的事：一屏三十来行，进去一个新目录时全部是冷路径 →
//! 30×1.5ms 到 30×12ms，也就是**一帧里卡住 50–400ms**。这正是用户报的「进入内容
//! 多的目录会明显卡一下」。（这条也踩了项目自己的红线：渲染路径上不许出现 AppKit
//! 调用 + 位图编码 + 写盘。）
//!
//! ## 现在的分工
//!
//! * **渲染路径**（[`super::AppState::file_icon`]）：只查表；查不到就记一笔「这行
//!   要图标」并返回 `None`（调用方就此退回内置 SVG）。零 IO、零 AppKit。
//! * **图标泵**（[`super::AppState::spawn_icon_pump`]）：把它记下的键**按时间配额
//!   一条条**拿出来问系统、写盘、落缓存，然后置 `dirty` 让 UI 在下一个 120ms 节拍
//!   重绘——图标于是「晚一两帧浮现」，而不是「当场把界面冻住」。
//!
//! ## `dispatch_sync` 的活是主线程干的：把单价拆开看
//!
//! 平台层取图标的动作跑在 `on_main_thread` 里，也就是 `dispatch_sync` 回**主队列**
//! ——泵虽然在 blocking 池，那份活最终仍是**主线程**在执行。实测每张 0.9ms 的构成：
//!
//! | 段 | 单价 |
//! |---|---|
//! | `iconForFile:` 问系统 | 0.03ms |
//! | 重绘到 40px | 0.07ms |
//! | 取回像素 | ~0.1ms |
//! | **PNG 编码** | **0.62ms（70%）** |
//!
//! 于是两头一起收：
//!
//! * **编码挪走**——平台层只交出**像素**（[`mo_platform::file_icon_raster`]），
//!   预乘还原 + PNG 编码交给 [`encode_icon_png`]，它在 blocking 池里跑，
//!   主线程单价掉到 ~0.2ms；
//! * **按时间配额滴灌**——泵每拍只花 `ICON_BUDGET_MS`，一条条取，配额用完把剩下的
//!   留在队列里等下一拍。以前一拍抓 40 张 = 主线程连着忙 40ms 以上，那正是用户报的
//!   「切到下载目录还是会卡一下」。
//!
//! ## 键：同一张图只问一次
//!
//! 系统图标由**类型**决定，不是由文件决定：一个目录里三百个 `.txt` 该共享同一张图。
//! 但有两类必须按**路径**问，否则会串图：
//!
//! * **目录**——`Desktop`/`Downloads`/`Applications` 这些特殊文件夹各有自己的图标，
//!   尽管它们都是目录；
//! * **包**（`.app`、`.bundle`、`framework`…）——每个包显示自己的图标；
//! * **无扩展名的文件**——系统按内容/UTI 给图（可执行文件 ≠ 无后缀的文本文件），
//!   拿不到可靠的共享键。
//!
//! 代价写在明处：给单个文件设过**自定义图标**的，会显示成它那个类型的大众图标
//! （自定义图标要从 FinderInfo 扩展属性读，是 IO，不能放在渲染路径上）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// 图标身份：同一个键 → 系统给同一张图。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconKey {
    /// 按路径问（目录 / 包 / 无扩展名的文件）。
    Path(PathBuf),
    /// 按类型问（同扩展名共享），存的是小写扩展名（含点，如 `.txt`）。
    Type(String),
}

/// 一个文件该用哪个键去问系统。
///
/// 规律：**图标由类型决定才共享**（`.txt` 的图标就是同一个）；由**这个条目自己**
/// 决定就必须按路径问。
pub fn icon_key(path: &Path, is_dir: bool) -> IconKey {
    if !is_dir {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_lowercase();
            if !ext.is_empty() && !is_package_ext(&ext) {
                return IconKey::Type(format!(".{ext}"));
            }
        }
    }
    IconKey::Path(path.to_path_buf())
}

/// 这些后缀在系统里是**包**：每个包显示自己的图标，一律按路径问。
///
/// 目录形态的包已经被上面的 `is_dir` 拦住了，这张表是给**符号链接形态**兜底的——
/// 指向 `.app` 的快捷方式在 Mo 里是文件，不特判的话一堆 app 会共用同一个图标
/// （谁先被问到就用谁的，看起来像串图）。
fn is_package_ext(ext: &str) -> bool {
    matches!(
        ext,
        "app"
            | "bundle"
            | "framework"
            | "plugin"
            | "kext"
            | "xpc"
            | "appex"
            | "prefpane"
            | "qlgenerator"
            | "mdimporter"
            | "saver"
            | "scptd"
    )
}

/// 图标缓存 + 待取队列。
///
/// 渲染路径只允许碰 [`IconCache::lookup`] 与 [`IconCache::request`]（都是纯内存操作）；
/// 真正的系统调用只在 `take` 出去的那批上做，且发生在后台线程。
#[derive(Default)]
pub struct IconCache {
    /// 路径 → 已落盘的 PNG。
    by_path: HashMap<PathBuf, PathBuf>,
    /// 类型 → 已落盘的 PNG（同类型共享一张）。
    by_type: HashMap<String, PathBuf>,
    /// 已经问过系统的键——**成功与否都记**。问不到的（文件已被删、系统就是不给）
    /// 不重复排队，省得每帧都去撞一次系统。
    asked: HashSet<IconKey>,
    /// 排队等着后台取的。渲染路径只管往里塞，从不在这里做任何 IO。
    queue: VecDeque<(PathBuf, IconKey)>,
}

/// 缓存条数上限：超过就整个清空重来（否则长时间浏览会把内存涨满）。
/// 只清缓存、不清「问过」的账本意义不大，所以一起清——反正清了就得重新问。
const ICON_CACHE_MAX: usize = 4000;

impl IconCache {
    /// 查表。**渲染路径唯一允许的动作**，纯内存。
    pub fn lookup(&self, path: &Path, is_dir: bool) -> Option<PathBuf> {
        if let Some(hit) = self.by_path.get(path) {
            return Some(hit.clone());
        }
        match icon_key(path, is_dir) {
            IconKey::Type(t) => self.by_type.get(&t).cloned(),
            // 按路径问的键已经在上面那一层查过了。
            IconKey::Path(_) => None,
        }
    }

    /// 记下「这一行要图标」，等后台去取。返回是否**新**入队（测试用）。
    ///
    /// 同一个键只会入队一次：`asked` 里有的（在途 / 问过 / 问不到）一律跳过。
    pub fn request(&mut self, path: &Path, is_dir: bool) -> bool {
        let key = icon_key(path, is_dir);
        if !self.asked.insert(key.clone()) {
            return false;
        }
        self.queue.push_back((path.to_path_buf(), key));
        true
    }

    /// 取**一条**待取的（`None` = 队列空了）。
    ///
    /// 泵要按时间配额一条条地取：取出来就等于「问过了」，所以配额用完时**没取走的
    /// 那些必须还在队列里**，下一拍接着来——不能一次 `take(40)` 再丢掉剩下的。
    pub fn pop_next(&mut self) -> Option<(PathBuf, IconKey)> {
        self.queue.pop_front()
    }

    /// 队列是否空（泵每拍先看一眼，省得空转一趟 blocking 线程）。
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// 落库：按路径、按类型（同类型共享）都能命中。
    pub fn insert(&mut self, path: &Path, key: &IconKey, png: PathBuf) {
        self.by_path.insert(path.to_path_buf(), png.clone());
        if let IconKey::Type(t) = key {
            self.by_type.insert(t.clone(), png);
        }
        if self.by_path.len() > ICON_CACHE_MAX {
            self.clear();
        }
    }

    /// 清空（含队列与账本）。
    pub fn clear(&mut self) {
        self.by_path.clear();
        self.by_type.clear();
        self.asked.clear();
        self.queue.clear();
    }

    /// 已缓存的**路径**条数。只给单测看内部状态用，所以跟着 `cfg(test)` 走
    /// （生产构建里留着它就是死代码，会撞 `-D warnings`）。
    #[cfg(test)]
    pub fn cached_paths(&self) -> usize {
        self.by_path.len()
    }

    /// 待取条数。
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }
}

/// 图标像素 → PNG 字节：预乘还原 + 编码。
///
/// **在后台（blocking 池）跑**，别在主线程调：这两步占了图标整段耗时的 70%
/// （实测编码 0.62ms / 总 0.9ms），留在主线程是白卡界面。平台层
/// （[`mo_platform::file_icon_raster`]）只做必须主线程的那部分——问系统 + 重绘 +
/// 拷像素。
///
/// AppKit 交出来的像素是**预乘 alpha**（半透明像素的 RGB 被压过），PNG 存的是直通
/// alpha，所以先还原再编码，否则抗锯齿边缘发暗。
pub fn encode_icon_png(raster: &mut mo_platform::IconRaster) -> Option<Vec<u8>> {
    mo_thumbnails::unpremultiply_rgba(&mut raster.rgba);
    mo_thumbnails::encode_rgba_png(raster.width, raster.height, &raster.rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同扩展名的普通文件共享一个键：三百个 `.txt` 只该问系统一次。
    #[test]
    fn plain_files_share_one_key_per_extension() {
        let a = icon_key(Path::new("/tmp/a.txt"), false);
        let b = icon_key(Path::new("/tmp/别的目录/b.TXT"), false);
        assert_eq!(a, IconKey::Type(".txt".to_string()));
        assert_eq!(a, b, "大小写不同的同后缀也该共享");
        assert_ne!(
            a,
            icon_key(Path::new("/tmp/c.pdf"), false),
            "不同后缀必须分开"
        );
    }

    /// 目录 / 包 / 无扩展名的文件必须按路径问——它们的图标各不相同。
    #[test]
    fn directories_bundles_and_extensionless_files_key_by_path() {
        // 目录：`Desktop` / `Downloads` 这类特殊文件夹各有自己的图标，不能共享；
        // 包（`.app`）在 Mo 里就是目录，于是天然走这一支，每个包问到自己的图标。
        for p in [
            "/Users/me/Documents",
            "/Users/me/Downloads",
            "/Applications/Xcode.app",
        ] {
            let path = Path::new(p);
            assert_eq!(
                icon_key(path, true),
                IconKey::Path(path.to_path_buf()),
                "{p} 该按路径问"
            );
        }
        // 无扩展名的文件：系统按内容 / UTI 给图（可执行文件 ≠ 无后缀的文本文件），
        // 没有可靠的共享键，也按路径问。
        assert_eq!(
            icon_key(Path::new("/tmp/LICENSE"), false),
            IconKey::Path(PathBuf::from("/tmp/LICENSE"))
        );
    }

    /// 渲染路径只记账，不重复排队：同一个键被问过一次之后不再入队。
    #[test]
    fn the_same_key_is_only_queued_once() {
        let mut c = IconCache::default();
        assert!(c.request(Path::new("/tmp/a.txt"), false));
        assert!(
            !c.request(Path::new("/tmp/b.txt"), false),
            "同类型的第二行不该再排队"
        );
        assert!(
            !c.request(Path::new("/tmp/a.txt"), false),
            "同一个文件也不该"
        );
        assert_eq!(c.queued(), 1);

        // 取走之后也不会因为「队列空了」再排一次。
        assert!(c.pop_next().is_some());
        assert!(!c.request(Path::new("/tmp/a.txt"), false));
    }

    /// 一条类型记录要同时让「同类型的新路径」和「原路径」都命中——否则往目录里
    /// 新加一个 `.txt`，那一行会一直挂着内置 SVG。
    #[test]
    fn a_type_hit_serves_every_path_of_that_type() {
        let mut c = IconCache::default();
        let key = icon_key(Path::new("/tmp/a.txt"), false);
        c.insert(
            Path::new("/tmp/a.txt"),
            &key,
            PathBuf::from("/icons/txt.png"),
        );

        assert_eq!(
            c.lookup(Path::new("/tmp/a.txt"), false),
            Some(PathBuf::from("/icons/txt.png"))
        );
        assert_eq!(
            c.lookup(Path::new("/tmp/后来才出现的.txt"), false),
            Some(PathBuf::from("/icons/txt.png")),
            "同类型的新文件应当直接命中，不必再问系统"
        );
        assert_eq!(c.lookup(Path::new("/tmp/c.pdf"), false), None);
    }

    /// 按路径问的（目录 / 包）**绝不能**互相命中。
    ///
    /// 这条守的是一类看起来像「串图」的事故：目录形态的包靠 `is_dir` 就分开了，
    /// 但**符号链接形态**的包（指向 `.app` 的快捷方式在 Mo 里就是文件）得靠后缀表
    /// 拦——漏了的话一堆 app 会共用先被问到的那张图标。
    #[test]
    fn path_keys_never_cross_serve() {
        let mut c = IconCache::default();
        let key = icon_key(Path::new("/Applications/A.app"), false);
        assert_eq!(
            key,
            IconKey::Path(PathBuf::from("/Applications/A.app")),
            "包即使呈现为文件（符号链接）也必须按路径问"
        );
        c.insert(
            Path::new("/Applications/A.app"),
            &key,
            PathBuf::from("/icons/a.png"),
        );

        assert_eq!(
            c.lookup(Path::new("/Applications/A.app"), false),
            Some(PathBuf::from("/icons/a.png"))
        );
        assert_eq!(
            c.lookup(Path::new("/Applications/B.app"), false),
            None,
            "另一个包必须自己去问，不能借用 A 的图标"
        );
    }

    /// 队列要能**一条条**取：配额用完时剩下的必须还留在队列里（下一拍再取）。
    #[test]
    fn items_are_taken_one_at_a_time_and_the_rest_stay_queued() {
        let mut c = IconCache::default();
        c.request(Path::new("/tmp/a.txt"), false);
        c.request(Path::new("/tmp/b.pdf"), false);
        c.request(Path::new("/tmp/c.png"), false);

        assert!(c.pop_next().is_some());
        assert_eq!(c.queued(), 2, "取一条只该少一条，其余留给下一个节拍");

        assert!(c.pop_next().is_some());
        assert!(c.pop_next().is_some());
        assert_eq!(c.queued(), 0);
        assert!(c.pop_next().is_none(), "空了就是空了");
    }

    /// 缓存涨到上限要整个清掉，而不是无限涨。
    #[test]
    fn the_cache_is_capped() {
        let mut c = IconCache::default();
        let over = ICON_CACHE_MAX + 10;
        for i in 0..over {
            let p = PathBuf::from(format!("/tmp/dir{i}"));
            let key = icon_key(&p, true);
            c.insert(&p, &key, PathBuf::from("/icons/dir.png"));
        }
        assert!(c.cached_paths() <= ICON_CACHE_MAX, "封顶后不该还留这么多");
    }
}
