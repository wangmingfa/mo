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
//!
//! ## 尺寸：按槽位分两档（[`ICON_PX_SMALL`] / [`ICON_PX_LARGE`]）
//!
//! 系统图标是**光栅**图，取出来多大就是多大，显示时再缩放。四个视图的槽位差得很远
//! （列表 / 列视图 16pt、网格 36pt、画廊 96pt），所以不能一刀切：
//!
//! * 一刀按 **40px** 取 → 画廊那个 96pt 的方框里放的就是近 5 倍上采样，糊；
//! * 一刀按 **128px** 取 → 成本按**面积**涨 10 倍（重绘 + 拷像素 + PNG 编码），
//!   而列表里那些 16pt 的小图标根本用不上，等于每进一个目录都白付这笔钱。
//!
//! 于是按槽位就近选档（[`icon_px_for_slot`]），**两档各自缓存、互不串用**：档位是
//! 缓存键的一部分，[`IconKey`] 里那个 `u32` 就是它。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// 小档位图的边长（物理像素）：列表 / 列视图的 16pt 槽位按 @2x 屏取 40px，留了点余量。
pub const ICON_PX_SMALL: u32 = 40;

/// 大档位图的边长（物理像素）：网格（36pt）/ 画廊（96pt）的方框都用它。
///
/// 和缩略图的约定取同一个数（`mo_thumbnails::DEFAULT_SIZE = 128`）：画廊那个 96pt
/// 的方框按 @2x 其实要 192px，128px 是 1.5 倍上采样——**与缩略图同等**，肉眼在
/// 「有缩略图的行」和「只有图标的行」之间看不出差别。再往上取就要为整屏图标白花
/// 成倍的主线程时间了。
pub const ICON_PX_LARGE: u32 = 128;

/// 小档能撑到的最大槽位（**逻辑 pt**）。
///
/// 40px 在 @2x 屏上正好画 20pt；放到 24pt 是 1.2 倍上采样——这个量级看不出来，
/// 却能把「行内小槽位」（列表行 / 列视图行，16pt）留在小档上。网格 / 画廊那些
/// 长方框（36 / 96pt）要的是铺满方框的位图，本来就该走大档。
const SMALL_MAX_SLOT_PT: f32 = 24.0;

/// 槽位尺寸（**逻辑 pt**，即 `img().w(px(x))` 里的 x）→ 该取哪一档位图。
///
/// 调用方只需说自己那个槽位多大，档位（以及为什么是这两个数）留在这一处。
pub fn icon_px_for_slot(slot_pt: f32) -> u32 {
    if slot_pt <= SMALL_MAX_SLOT_PT {
        ICON_PX_SMALL
    } else {
        ICON_PX_LARGE
    }
}

/// 图标身份：同一个键 **+ 同一个档位** → 系统给同一张图。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconKey {
    /// 按路径问（目录 / 包 / 无扩展名的文件）。
    Path(PathBuf, u32),
    /// 按类型问（同扩展名共享），存的是小写扩展名（含点，如 `.txt`）。
    Type(String, u32),
}

impl IconKey {
    /// 这个键要的位图边长（物理像素）。
    ///
    /// 泵按它去问平台——**键里存的就应该是实际取图用的那个数**，不然缓存会串档。
    pub fn px(&self) -> u32 {
        match self {
            IconKey::Path(_, px) | IconKey::Type(_, px) => *px,
        }
    }
}

/// 一个文件该用哪个键去问系统。
///
/// 规律：**图标由类型决定才共享**（`.txt` 的图标就是同一个）；由**这个条目自己**
/// 决定就必须按路径问。`slot_pt` 只影响档位，不影响「按路径还是按类型」。
pub fn icon_key(path: &Path, is_dir: bool, slot_pt: f32) -> IconKey {
    let px = icon_px_for_slot(slot_pt);
    if !is_dir {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_lowercase();
            if !ext.is_empty() && !is_package_ext(&ext) {
                return IconKey::Type(format!(".{ext}"), px);
            }
        }
    }
    IconKey::Path(path.to_path_buf(), px)
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
    /// (路径, 档位) → 已落盘的 PNG。
    ///
    /// 主键带上档位：同一个目录在列表里要 40px、在画廊里要 128px，两张不能互相顶掉。
    by_path: HashMap<(PathBuf, u32), PathBuf>,
    /// (类型, 档位) → 已落盘的 PNG（同类型同档位共享一张）。
    by_type: HashMap<(String, u32), PathBuf>,
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
    ///
    /// 档位是主键的一部分：[小档]命中的图**不会**拿去填[大档]的槽位（拿 40px 放
    /// 96pt 的方框就是糊的），反过来也一样——两种槽位各存各的。代价是切视图模式时
    /// 新槽位要重新问一轮，但那是一屏几十张、且每种类型/路径只问一次。
    ///
    /// [小档]: ICON_PX_SMALL
    /// [大档]: ICON_PX_LARGE
    pub fn lookup(&self, path: &Path, is_dir: bool, slot_pt: f32) -> Option<PathBuf> {
        let px = icon_px_for_slot(slot_pt);
        if let Some(hit) = self.by_path.get(&(path.to_path_buf(), px)) {
            return Some(hit.clone());
        }
        match icon_key(path, is_dir, slot_pt) {
            IconKey::Type(t, px) => self.by_type.get(&(t, px)).cloned(),
            // 按路径问的键已经在上面那一层查过了。
            IconKey::Path(..) => None,
        }
    }

    /// 记下「这一行要图标」，等后台去取。返回是否**新**入队（测试用）。
    ///
    /// 同一个键只会入队一次：`asked` 里有的（在途 / 问过 / 问不到）一律跳过。
    /// 换了档位就是另一个键，会各自入队一次。
    pub fn request(&mut self, path: &Path, is_dir: bool, slot_pt: f32) -> bool {
        let key = icon_key(path, is_dir, slot_pt);
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

    /// 落库：按路径、按类型（同类型共享）都能命中。档位取自 `key`。
    pub fn insert(&mut self, path: &Path, key: &IconKey, png: PathBuf) {
        self.by_path
            .insert((path.to_path_buf(), key.px()), png.clone());
        if let IconKey::Type(t, px) = key {
            self.by_type.insert((t.clone(), *px), png);
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

    /// 小槽位 / 大槽位各取一档的代表值（列表行 16pt、列视图 16pt、网格 36pt、画廊 96pt
    /// ——视图侧那张表在 `mo_ui::listing::icon_slot`，这里只验本模块的档位判据）。
    const SLOT_SMALL: f32 = 16.0;
    const SLOT_LARGE: f32 = 96.0;

    /// 同扩展名的普通文件共享一个键：三百个 `.txt` 只该问系统一次。
    #[test]
    fn plain_files_share_one_key_per_extension() {
        let a = icon_key(Path::new("/tmp/a.txt"), false, SLOT_SMALL);
        let b = icon_key(Path::new("/tmp/别的目录/b.TXT"), false, SLOT_SMALL);
        assert_eq!(a, IconKey::Type(".txt".to_string(), ICON_PX_SMALL));
        assert_eq!(a, b, "大小写不同的同后缀也该共享");
        assert_ne!(
            a,
            icon_key(Path::new("/tmp/c.pdf"), false, SLOT_SMALL),
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
                icon_key(path, true, SLOT_SMALL),
                IconKey::Path(path.to_path_buf(), ICON_PX_SMALL),
                "{p} 该按路径问"
            );
        }
        // 无扩展名的文件：系统按内容 / UTI 给图（可执行文件 ≠ 无后缀的文本文件），
        // 没有可靠的共享键，也按路径问。
        assert_eq!(
            icon_key(Path::new("/tmp/LICENSE"), false, SLOT_SMALL),
            IconKey::Path(PathBuf::from("/tmp/LICENSE"), ICON_PX_SMALL)
        );
    }

    /// 槽位 → 档位：小槽位要 40px、大槽位要 128px，**没有中间态**。
    ///
    /// 这条钉的是「一刀切」的两个极端都会出事：全按 40px 取，画廊那个 96pt 的方框里
    /// 就是近 5 倍上采样（糊）；全按 128px 取，列表里 16pt 的小图标白花 10 倍主线程
    /// 时间（面积比）。四个视图的槽位都落在这条曲线上。
    #[test]
    fn small_slots_take_small_rasters_and_big_slots_take_large() {
        for slot in [16.0, 20.0, SLOT_SMALL, SMALL_MAX_SLOT_PT] {
            assert_eq!(
                icon_px_for_slot(slot),
                ICON_PX_SMALL,
                "{slot}pt 的槽位该用小档（40px）"
            );
        }
        // 阈值 24pt：行内小槽位（16pt）在小档，网格 36pt 方框 / 画廊 96pt 方框在大档。
        for slot in [SMALL_MAX_SLOT_PT + 0.1, 36.0, SLOT_LARGE] {
            assert_eq!(
                icon_px_for_slot(slot),
                ICON_PX_LARGE,
                "{slot}pt 的槽位该用大档（128px）"
            );
        }
        // 大档必须比小档大——写反了整套分档就没意义。这是常量之间的关系，
        // 放编译期断言里（`assert!` 直接写会被 clippy 判「断言恒定量」）。
        const { assert!(ICON_PX_LARGE > ICON_PX_SMALL) };
    }

    /// 档位是**缓存键的一部分**：小档的图不能拿去填大槽位（40px 放 96pt 就是糊的），
    /// 大档的图也不能顶掉小槽位（那让小图标白白背着 10 倍内存）。
    ///
    /// 两种键都要验：按类型的（`.txt`）和按路径的（目录）。
    #[test]
    fn the_two_buckets_never_cross_serve() {
        let mut c = IconCache::default();
        // 按类型：小档落了库，大档查不到。
        let small = icon_key(Path::new("/tmp/a.txt"), false, SLOT_SMALL);
        c.insert(
            Path::new("/tmp/a.txt"),
            &small,
            PathBuf::from("/icons/txt-40.png"),
        );
        assert_eq!(
            c.lookup(Path::new("/tmp/b.txt"), false, SLOT_SMALL),
            Some(PathBuf::from("/icons/txt-40.png")),
            "同类型同档位该命中"
        );
        assert_eq!(
            c.lookup(Path::new("/tmp/b.txt"), false, SLOT_LARGE),
            None,
            "同类型但换了档位，必须重新问系统，不能拿小图充数"
        );
        // 按路径：同理。
        let dir_small = icon_key(Path::new("/Users/me/Downloads"), true, SLOT_SMALL);
        c.insert(
            Path::new("/Users/me/Downloads"),
            &dir_small,
            PathBuf::from("/icons/dl-40.png"),
        );
        assert_eq!(
            c.lookup(Path::new("/Users/me/Downloads"), false, SLOT_LARGE),
            None,
            "同一个目录换了档位也要重新问"
        );
    }

    /// 渲染路径只记账，不重复排队：同一个键被问过一次之后不再入队。
    #[test]
    fn the_same_key_is_only_queued_once() {
        let mut c = IconCache::default();
        assert!(c.request(Path::new("/tmp/a.txt"), false, SLOT_SMALL));
        assert!(
            !c.request(Path::new("/tmp/b.txt"), false, SLOT_SMALL),
            "同类型的第二行不该再排队"
        );
        assert!(
            !c.request(Path::new("/tmp/a.txt"), false, SLOT_SMALL),
            "同一个文件也不该"
        );
        assert_eq!(c.queued(), 1);

        // 取走之后也不会因为「队列空了」再排一次。
        assert!(c.pop_next().is_some());
        assert!(!c.request(Path::new("/tmp/a.txt"), false, SLOT_SMALL));

        // 换了档位是另一个键，该排就排（否则切到画廊时那批图标永远补不上）。
        assert!(c.request(Path::new("/tmp/a.txt"), false, SLOT_LARGE));
        assert_eq!(c.queued(), 1);
    }

    /// 一条类型记录要同时让「同类型的新路径」和「原路径」都命中——否则往目录里
    /// 新加一个 `.txt`，那一行会一直挂着内置 SVG。
    #[test]
    fn a_type_hit_serves_every_path_of_that_type() {
        let mut c = IconCache::default();
        let key = icon_key(Path::new("/tmp/a.txt"), false, SLOT_SMALL);
        c.insert(
            Path::new("/tmp/a.txt"),
            &key,
            PathBuf::from("/icons/txt.png"),
        );

        assert_eq!(
            c.lookup(Path::new("/tmp/a.txt"), false, SLOT_SMALL),
            Some(PathBuf::from("/icons/txt.png"))
        );
        assert_eq!(
            c.lookup(Path::new("/tmp/后来才出现的.txt"), false, SLOT_SMALL),
            Some(PathBuf::from("/icons/txt.png")),
            "同类型的新文件应当直接命中，不必再问系统"
        );
        assert_eq!(c.lookup(Path::new("/tmp/c.pdf"), false, SLOT_SMALL), None);
    }

    /// 按路径问的（目录 / 包）**绝不能**互相命中。
    ///
    /// 这条守的是一类看起来像「串图」的事故：目录形态的包靠 `is_dir` 就分开了，
    /// 但**符号链接形态**的包（指向 `.app` 的快捷方式在 Mo 里就是文件）得靠后缀表
    /// 拦——漏了的话一堆 app 会共用先被问到的那张图标。
    #[test]
    fn path_keys_never_cross_serve() {
        let mut c = IconCache::default();
        let key = icon_key(Path::new("/Applications/A.app"), false, SLOT_SMALL);
        assert_eq!(
            key,
            IconKey::Path(PathBuf::from("/Applications/A.app"), ICON_PX_SMALL),
            "包即使呈现为文件（符号链接）也必须按路径问"
        );
        c.insert(
            Path::new("/Applications/A.app"),
            &key,
            PathBuf::from("/icons/a.png"),
        );

        assert_eq!(
            c.lookup(Path::new("/Applications/A.app"), false, SLOT_SMALL),
            Some(PathBuf::from("/icons/a.png"))
        );
        assert_eq!(
            c.lookup(Path::new("/Applications/B.app"), false, SLOT_SMALL),
            None,
            "另一个包必须自己去问，不能借用 A 的图标"
        );
    }

    /// 队列要能**一条条**取：配额用完时剩下的必须还留在队列里（下一拍再取）。
    #[test]
    fn items_are_taken_one_at_a_time_and_the_rest_stay_queued() {
        let mut c = IconCache::default();
        c.request(Path::new("/tmp/a.txt"), false, SLOT_SMALL);
        c.request(Path::new("/tmp/b.pdf"), false, SLOT_SMALL);
        c.request(Path::new("/tmp/c.png"), false, SLOT_SMALL);

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
            let key = icon_key(&p, true, SLOT_SMALL);
            c.insert(&p, &key, PathBuf::from("/icons/dir.png"));
        }
        assert!(c.cached_paths() <= ICON_CACHE_MAX, "封顶后不该还留这么多");
    }
}
