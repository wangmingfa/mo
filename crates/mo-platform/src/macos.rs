//! macOS：AppKit 的 `NSWorkspace`（回收站 / 在访达中显示 / 推出卷宗）+
//! `statfs`（卷宗列表）。
//!
//! 用 `objc` 直接发消息，不走 `osascript`：
//!
//! * `osascript` 要起一个 AppleScript 进程、还要等 Finder 应答，几百毫秒起跳；
//! * 而且 Finder 不一定在跑（用户可以把它关掉），`NSWorkspace` 由系统服务接管。
//!
//! ## 为什么这些调用必须在**主线程**
//!
//! `NSWorkspace` 是 AppKit 的，`recycleURLs:` 内部会弹认证 / 进度 UI。AppKit 要求
//! 这类调用在主线程（其它线程上行为未定义，可能直接崩）。Mo 的后台任务都跑在
//! tokio 的 worker 上，所以先看当前是不是主线程（`NSThread.isMainThread`），不是
//! 就用 `dispatch::Queue::main().exec_sync()` 把它丢回主线程执行。

// 发 objc 消息只能走 `unsafe`，与 `mo-ui::icon` 的 macOS FFI 层同一口径：
// 局部豁免，unsafe 代码不许出现在本模块之外。
#![allow(unsafe_code)]
// `objc` 的 `msg_send!` 宏展开里带 `cfg(feature = "cargo-clippy")`（它自己 crate 的
// 特性），在本 crate 展开就成了「意外的 cfg」——与本 crate 无关，别污染输出。
#![allow(unexpected_cfgs)]

use std::path::{Path, PathBuf};

// ⚠️ 必须显式链接这两个框架：`objc` 只链接了 `libobjc`，而 Foundation / AppKit 的类
// 是**懒加载**的——不链接就 `Class::get("NSThread")` 直接返回 None（表现为「系统
// 里没有这个类」，在裸二进制里尤其容易撞上）。
#[link(name = "Foundation", kind = "framework")]
extern "C" {}
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

use crate::{IconRaster, PlatformError, Volume};

// CoreGraphics：把 `iconForFile:` 给出的 `NSImage` 一次缩到我们要的像素尺寸。
//
// 为什么用它而不是 AppKit 的位图：`NSImage` 重绘 + `imageRepWithData:` 那条路在
// Retina 上按 2x 出图、而且解出来是 **16 位/通道**的位图（实测 bps=16、bpr=640，
// 按 8 位读就是错位数据）。CG 是纯 C、位深与字节序都由我们指定，还能顺手设插值质量。
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut std::ffi::c_void;
    fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
    fn CGBitmapContextCreate(
        data: *mut u8,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut std::ffi::c_void,
        bitmap_info: u32,
    ) -> *mut std::ffi::c_void;
    fn CGContextSetInterpolationQuality(ctx: *mut std::ffi::c_void, quality: i32);
    fn CGContextDrawImage(ctx: *mut std::ffi::c_void, rect: NSRect, image: *mut std::ffi::c_void);
    fn CGContextRelease(ctx: *mut std::ffi::c_void);

    // PDF：系统自带的渲染器（`CGPDFDocument`），不需要任何第三方依赖。
    fn CGPDFDocumentCreateWithURL(url: *mut Object) -> *mut std::ffi::c_void;
    fn CGPDFDocumentGetNumberOfPages(doc: *mut std::ffi::c_void) -> usize;
    fn CGPDFDocumentGetPage(doc: *mut std::ffi::c_void, page: usize) -> *mut std::ffi::c_void;
    fn CGPDFDocumentRelease(doc: *mut std::ffi::c_void);
    fn CGPDFPageGetBoxRect(page: *mut std::ffi::c_void, box_kind: i32) -> NSRect;
    fn CGContextDrawPDFPage(ctx: *mut std::ffi::c_void, page: *mut std::ffi::c_void);
    fn CGContextSetRGBFillColor(ctx: *mut std::ffi::c_void, r: f64, g: f64, b: f64, a: f64);
    fn CGContextFillRect(ctx: *mut std::ffi::c_void, rect: NSRect);
    fn CGContextScaleCTM(ctx: *mut std::ffi::c_void, sx: f64, sy: f64);
}

/// `kCGImageAlphaPremultipliedLast`：通道序 R,G,B,A，alpha 在最后、**预乘**。
const K_CG_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
/// `kCGBitmapByteOrder32Big`：按内存里的 R,G,B,A 顺序（不是 BGRA）。
const K_CG_BYTE_ORDER_32_BIG: u32 = 4 << 12;
/// `kCGInterpolationHigh`：512px 缩到 40px 是 12 倍下采样，默认质量会明显发糊。
const K_CG_INTERPOLATION_HIGH: i32 = 3;

/// AppKit 的 `NSSize`（两个 `double`，与 `CGSize` 同构）。
#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    width: f64,
    height: f64,
}

// SAFETY: 布局与 ObjC 的 `CGSize` / `NSSize` 一致（`{CGSize=dd}`，字段都是 `double`），
// 所以按值把 `&self` 以外的这份结构体传给 `msg_send!` 是安全的。
unsafe impl objc::Encode for NSSize {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGSize=dd}") }
    }
}

/// CoreGraphics 的 `CGPoint`（与 AppKit 的 `NSPoint` 同构）。
#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}

/// CoreGraphics 的 `CGRect`（与 AppKit 的 `NSRect` 同构）。
#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

// SAFETY: 布局与 ObjC 的 `CGRect` / `NSRect` 一致（四个 `double`）。
unsafe impl objc::Encode for NSRect {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

/// 把一批路径送进**系统**废纸篓（访达里那份）。
///
/// 用 `NSFileManager.trashItemAtURL:resultingItemURL:error:` 而不是
/// `NSWorkspace.recycleURLs:completionHandler:`：
///
/// * 后者是**异步**的（要带 completionHandler、结果靠回调），在裸二进制 / 后台任务
///   里实测直接返回 NO 而拿不到原因——`NSWorkspace` 那套还要主线程与 run loop；
/// * 前者同步、返回 `NSError`，可以原样把系统的说法带给用户（「宗卷不支持废纸篓」
///   这种只有系统知道）。
///
/// `NSFileManager` 是线程安全的，不必绕主线程。
pub fn recycle(paths: &[PathBuf]) -> Result<(), PlatformError> {
    if paths.is_empty() {
        return Ok(());
    }
    for (done, p) in paths.iter().enumerate() {
        let Some(url) = nsurl_for(p) else {
            return Err(PlatformError::Failed(format!(
                "无法把路径交给系统：{}",
                p.display()
            )));
        };
        // 任一条失败就停下，绝不「跳过它继续」——用户以为删了三条，实际只进了两条
        // 废纸篓。但已经进去的那些没法撤回，所以错误里要写清楚进了几条。
        unsafe {
            let manager: *mut Object = msg_send![class("NSFileManager")?, defaultManager];
            let mut error: *mut Object = std::ptr::null_mut();
            let ok: bool = msg_send![manager, trashItemAtURL: url resultingItemURL: std::ptr::null_mut::<Object>() error: &mut error];
            if !ok {
                let why = error_text(error).unwrap_or_else(|| "系统没有说明原因".to_string());
                return Err(PlatformError::Failed(format!(
                    "无法把 {} 送进废纸篓：{why}（前面 {done} 条已经进去了）",
                    p.display()
                )));
            }
        }
    }
    Ok(())
}

/// 「在访达中显示」：选中并滚动到那个条目。
pub fn reveal(path: &Path) -> Result<(), PlatformError> {
    let owned = path.to_path_buf();
    on_main_thread(move || {
        let Some(url) = nsurl_for(&owned) else {
            return Err(PlatformError::Failed(format!(
                "无法把路径交给访达：{}",
                owned.display()
            )));
        };
        unsafe {
            let array: *mut Object = msg_send![class("NSArray")?, arrayWithObject: url];
            let workspace = workspace()?;
            // `activateFileViewerSelectingURLs:` 会把访达带到前台并选中这些条目。
            let _: () = msg_send![workspace, activateFileViewerSelectingURLs: array];
        }
        Ok(())
    })
}

/// 推出 / 卸载一个卷宗（挂载点路径）。
pub fn eject(path: &Path) -> Result<(), PlatformError> {
    let owned = path.to_path_buf();
    on_main_thread(move || {
        let Some(url) = nsurl_for(&owned) else {
            return Err(PlatformError::Failed(format!(
                "无法把挂载点交给系统：{}",
                owned.display()
            )));
        };
        unsafe {
            let workspace = workspace()?;
            let mut error: *mut Object = std::ptr::null_mut();
            let ok: bool = msg_send![workspace, unmountAndEjectDeviceAtURL: url error: &mut error];
            if ok {
                Ok(())
            } else {
                // 系统的说法比我们猜的准（「还有一个窗口在用」「卷宗不存在」都只有
                // 系统知道），拿得到就原样带回去。
                let why = error_text(error).unwrap_or_else(|| "可能还有文件正在使用".to_string());
                Err(PlatformError::Failed(format!(
                    "推出失败：{}（{why}）",
                    owned.display()
                )))
            }
        }
    })
}

// ---- objc 小工具 ----

/// `NSWorkspace.sharedWorkspace`。
fn workspace() -> Result<*mut Object, PlatformError> {
    let cls = class("NSWorkspace")?;
    Ok(unsafe { msg_send![cls, sharedWorkspace] })
}

/// 取一个类；拿不到就报错（而不是 `unwrap()` 崩掉——那会把整个进程带走）。
fn class(name: &'static str) -> Result<&'static Class, PlatformError> {
    Class::get(name)
        .ok_or_else(|| PlatformError::Failed(format!("系统里没有 {name}（AppKit 没加载？）")))
}

/// 路径 → `file://` URL（`NSURL.fileURLWithPath:`）。
///
/// 百分号编码（空格、中文、`#`…）由 `NSURL` 自己处理，别自己拼 `file://` 字符串——
/// 拼错了就是「系统说文件不存在」这种最难查的错。
fn nsurl_for(path: &Path) -> Option<*mut Object> {
    let s = path.to_string_lossy().to_string();
    let nsstring = nsstring(&s)?;
    let url: *mut Object = unsafe { msg_send![class("NSURL").ok()?, fileURLWithPath: nsstring] };
    if url.is_null() {
        None
    } else {
        Some(url)
    }
}

/// 把 `NSError *` 的 `localizedDescription` 读成 Rust 字符串。
///
/// # Safety
/// `err` 必须是 `nil` 或一个真的 `NSError *`。
unsafe fn error_text(err: *mut Object) -> Option<String> {
    if err.is_null() {
        return None;
    }
    let desc: *mut Object = msg_send![err, localizedDescription];
    if desc.is_null() {
        return None;
    }
    let utf8: *const std::os::raw::c_char = msg_send![desc, UTF8String];
    if utf8.is_null() {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(utf8).to_string_lossy().to_string())
}

fn nsstring(s: &str) -> Option<*mut Object> {
    let cls = Class::get("NSString")?; // NSString 属于 Foundation，拿不到就是没链接上。
    let obj: *mut Object =
        unsafe { msg_send![cls, stringWithUTF8String: s.as_ptr() as *const std::os::raw::c_char] };
    if obj.is_null() {
        None
    } else {
        Some(obj)
    }
}

/// 当前是不是 OS 主线程。
///
/// `on_main_thread` 据此决定「直接跑」还是「`dispatch_sync` 回主队列」。GPUI 的
/// 事件循环（含渲染）就跑在 OS 主线程上，所以渲染里要系统图标时这里返回 `true`；
/// `cargo test` 的 `TestAppContext` 把渲染跑在子线程、主队列不 drain，这里返回
/// `false`——`AppState::file_icon` 据此跳过平台调用，避免 `dispatch_sync` 死锁。
pub fn is_main_thread() -> bool {
    Class::get("NSThread")
        .map(|thread| unsafe { msg_send![thread, isMainThread] })
        .unwrap_or(false)
}

/// 主线程的 run loop 是否已经跑起来（见 `crate::mark_main_loop_ready`）。
///
/// 单向闩：只由真实应用启动时置位一次。用 `Relaxed` 就够——读到旧值最多让一个
/// 后台任务晚一轮才动手，没有任何数据要跟着这个标志同步。
static MAIN_LOOP_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn mark_main_loop_ready() {
    MAIN_LOOP_READY.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn appkit_usable() -> bool {
    appkit_usable_with(
        is_main_thread(),
        MAIN_LOOP_READY.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// [`appkit_usable`] 的纯逻辑部分——单测能测的那半（真去置位闩会把测试进程带进
/// 「主队列有人 drain」的假象里，反而危险，所以不测副作用、只测判定）。
fn appkit_usable_with(is_main: bool, main_loop_ready: bool) -> bool {
    is_main || main_loop_ready
}

/// 在主线程上跑 `f`，把结果带回来。
///
/// 已经在主线程就直接跑；否则 `dispatch_sync` 到主队列。⚠️ 后台任务里调用它时，
/// 主线程必须还能处理这个队列（Mo 的主线程在跑 GPUI 事件循环，正常情况没问题）。
fn on_main_thread<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    use dispatch::Queue;

    // 已经是主线程就直接跑；判不出来（类拿不到）也按「不是主线程」处理——
    // `dispatch_sync` 到主队列在任何情况下都是安全的。
    let is_main: bool = Class::get("NSThread")
        .map(|thread| unsafe { msg_send![thread, isMainThread] })
        .unwrap_or(false);
    if is_main {
        return f();
    }

    let mut slot = Some(f);
    let mut out: Option<T> = None;
    Queue::main().exec_sync(|| {
        if let Some(f) = slot.take() {
            out = Some(f());
        }
    });
    // `exec_sync` 一定跑完闭包：拿不到结果说明主线程没能执行（App 正在退出）。
    out.expect("主线程没能执行这个 AppKit 调用")
}

/// 已挂载的**本机**卷宗。
///
/// macOS 把所有卷宗都挂在 `/Volumes` 下（启动盘也在那里，是一个 firmlink），
/// 所以列这一层就够了。两个过滤：
///
/// * 以点开头的是系统自己用的（`.timemachine` 之类）；
/// * **网络文件系统要排除**——它们归侧边栏的「网络」区，两边都列同一个盘
///   只会让人困惑。判据是 `statfs` 的 `f_fstypename`，不靠路径猜。
///
/// 另外逐块问一下 [`volume_is_ejectable`]：内置硬盘推不动，UI 不该给它画推出按钮。
pub fn volumes() -> Vec<Volume> {
    let Ok(read) = std::fs::read_dir("/Volumes") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if fs_type(&path).is_some_and(|t| is_network_fs(&t)) {
            continue;
        }
        out.push(Volume {
            name,
            ejectable: volume_is_ejectable(&path),
            path,
        });
    }
    out.sort_by_key(|v| v.name.to_lowercase());
    out
}

/// 这块卷宗能不能「推出」。
///
/// **内置硬盘（启动盘）不能**——访达也不给它画推出按钮，点下去只会得到
/// 「推出失败」。判据取 Foundation 的三个 volume resource key：
///
/// * 可弹出介质（USB / SD / 光盘）→ `NSURLVolumeIsEjectableKey`；
/// * 可移动卷（磁盘映像）→ `NSURLVolumeIsRemovableKey`；
/// * 非本地（网络卷）→ `NSURLVolumeIsLocalKey == false`；
/// * 内置盘靠 `NSURLVolumeIsInternalKey` 直接否掉。
///
/// ⚠️ 不在主线程一律返回 `false`（= 不给按钮），与 `file_icon_raster` 同一条纪律：读
/// resource value 走 `on_main_thread`，而测试把渲染跑在子线程，`dispatch_sync` 回
/// 主队列会挂死。渲染在真机的主线程上跑，那里拿得到真值。
fn volume_is_ejectable(path: &Path) -> bool {
    if !is_main_thread() {
        return false;
    }
    let owned = path.to_path_buf();
    on_main_thread(move || unsafe {
        let Some(url) = nsurl_for(&owned) else {
            return false;
        };
        decide_ejectable(
            volume_flag(url, "NSURLVolumeIsInternalKey"),
            volume_flag(url, "NSURLVolumeIsEjectableKey"),
            volume_flag(url, "NSURLVolumeIsRemovableKey"),
            volume_flag(url, "NSURLVolumeIsLocalKey"),
        )
    })
}

/// 判据本体（纯函数：四个 resource value → 能不能推出）。
///
/// 抽出来是为了可测：真机上每块盘问一次 AppKit 没法进单测（`on_main_thread` 在
/// 测试子线程里要先被拦掉），而这四条逻辑是「内置盘不给按钮」这件事的全部依据。
///
/// `None` = 那个 key 没读到。**读不到不算证据**：不能因为问不到 `IsLocal` 就当成
/// 网络盘把按钮画出来，所以除了明确的 `false`，一律不成立。
fn decide_ejectable(
    internal: Option<bool>,
    ejectable: Option<bool>,
    removable: Option<bool>,
    local: Option<bool>,
) -> bool {
    if internal == Some(true) {
        return false; // 内置盘（启动盘）：没有「推出」这个概念。
    }
    ejectable == Some(true)          // U 盘 / SD 卡 / 光盘
        || removable == Some(true)   // 磁盘映像（.dmg）
        || local == Some(false) // 网络盘
}

/// 读一个 BOOL 型的 URL resource value；读不到（key 不认识 / 该卷不提供）返回 `None`。
///
/// `None` 与 `Some(false)` 必须分开：判据里要能区分「系统说它不是内置盘」与
/// 「压根没问到」——后者不能当成「可推出」的证据。
///
/// # Safety
/// `url` 必须是有效的 `NSURL *`。
unsafe fn volume_flag(url: *mut Object, key_name: &str) -> Option<bool> {
    let key = nsstring(key_name)?;
    let mut value: *mut Object = std::ptr::null_mut();
    let mut err: *mut Object = std::ptr::null_mut();
    let ok: bool = msg_send![
        url,
        getResourceValue: &mut value as *mut *mut Object
        forKey: key
        error: &mut err as *mut *mut Object
    ];
    if !ok || value.is_null() {
        return None;
    }
    Some(msg_send![value, boolValue])
}

/// `statfs` 的文件系统类型名（`f_fstypename`：`apfs` / `smbfs` / `nfs`…）。
fn fs_type(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let raw = unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) };
    raw.to_str().ok().map(|s| s.to_string())
}

/// 网络文件系统：这些由侧边栏的「网络」区负责（`mo_remote::mount`）。
fn is_network_fs(fs: &str) -> bool {
    matches!(
        fs,
        "smbfs" | "nfs" | "nfs4" | "afpfs" | "afp" | "webdav" | "cifs" | "ftp"
    )
}

/// 取一个文件在系统里的图标，**重绘**到 `px` 见方后交出 RGBA 像素。
///
/// `px` 是**物理像素**边长，由调用方按显示槽位与屏幕倍率决定（见
/// `mo_app::icon::icon_px_for_slot`）。这里不设默认值是有意的：系统图标是光栅图，
/// 取 40px 再放进 96pt 的方框里就是近 5 倍上采样，糊得很明显；而统一按 128px 取，
/// 每一张的主线程重绘 + 拷像素都要涨 10 倍（面积比），列表视图白付这笔钱。
///
/// 整段包在 `on_main_thread` 里——`NSWorkspace` 按 Apple 的约定要在主线程调，所以
/// 这里的耗时**就是主线程的耗时**，一分都不该多花：
///
/// * `iconForFile:` 给的是**原始尺寸**的 `NSImage`（512×512 起），当年直接编码它要
///   250–500ms/张，一屏几十张就是几秒的沙滩球。`setSize:` 只改逻辑尺寸（出图照样按
///   原始像素），所以要**真画一张小的**——访达同款的小图标就是这么来的。
/// * 编码 PNG 那一步（占整段 70%）**不在这里做**：只把像素拷出去，交给调用方在
///   后台编码（见 [`crate::IconRaster`]）。
pub fn file_icon_raster(path: &Path, px: u32) -> Option<IconRaster> {
    let owned = path.to_path_buf();
    on_main_thread(move || unsafe {
        let s = owned.to_string_lossy();
        let ns_path = nsstring(&s)?;
        // 闭包返回 `Option`，所以 Result 这边的 `?` 要走 `.ok()?`（失败即 `None`）。
        let ws = workspace().ok()?;
        let image: *mut Object = msg_send![ws, iconForFile: ns_path];
        if image.is_null() {
            return None;
        }
        // 至少 1px：0 会让 `CGBitmapContextCreate` 拿到 0 长度缓冲。
        let side = px.max(1) as f64;
        let px = side as usize;
        let src = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize {
                width: side,
                height: side,
            },
        };
        // 从 NSImage 拿一张 CGImage——AppKit 到此为止，后面全是纯 CoreGraphics。
        //
        // 为什么不再走「新建 40pt 的 NSImage → lockFocus → drawInRect → TIFF →
        // NSBitmapImageRep」：那条路在 Retina 上会按 2x 出图（40pt = 80px），而且
        // `imageRepWithData:` 解出来的是 **16 位/通道**的位图（实测 bps=16、bpr=640），
        // 按 8 位读就是错位数据。CG 这边可以直接缩放到我们的像素尺寸、位深由我们指定。
        let cg: *mut std::ffi::c_void = msg_send![
            image,
            CGImageForProposedRect: &src as *const NSRect as *mut NSRect
            context: std::ptr::null_mut::<Object>()
            hints: std::ptr::null_mut::<Object>()
        ];
        if cg.is_null() {
            return None;
        }

        let space = CGColorSpaceCreateDeviceRGB();
        if space.is_null() {
            return None;
        }
        // 8 位/通道、RGBA、**alpha 预乘**（AppKit / CG 的位图约定，上层编码前还原）。
        let mut rgba = vec![0u8; px * px * 4];
        let ctx = CGBitmapContextCreate(
            rgba.as_mut_ptr(),
            px,
            px,
            8,
            px * 4,
            space,
            K_CG_ALPHA_PREMULTIPLIED_LAST | K_CG_BYTE_ORDER_32_BIG,
        );
        CGColorSpaceRelease(space);
        if ctx.is_null() {
            return None;
        }
        // 从 512px 缩到 40–128px 是 4–12 倍下采样，插值质量不设高档会明显发糊。
        CGContextSetInterpolationQuality(ctx, K_CG_INTERPOLATION_HIGH);
        CGContextDrawImage(ctx, src, cg);
        CGContextRelease(ctx);

        Some(IconRaster {
            width: px as u32,
            height: px as u32,
            rgba,
        })
    })
}

/// `kCGPDFMediaBox`：页面「纸张」尺寸（区别于裁切 / 出血框）。
const K_CGPDF_MEDIA_BOX: i32 = 0;

/// 把 PDF 的**第一页**渲染成一张位图（长边不超过 `max_edge` 像素）。
///
/// PDF 是日常高频格式，而 mo-preview 只能给「二进制文件」——空格键预览一个 PDF 看到
/// 一句乱码说明，等于没有预览。这里用系统自带的 CoreGraphics 渲染，不引第三方依赖。
///
/// 与 [`file_icon_raster`] 的关键区别：**不需要主线程**。`CGPDFDocument` 是纯 C 的，
/// 不碰 `NSWorkspace` / AppKit，所以可以放心放在 blocking 池里跑（首页渲染几毫秒到
/// 几十毫秒，大页面更久，绝不能压在主线程上）。
///
/// 返回 `None` 一律表示「这个 PDF 渲染不出来」（打不开 / 加密 / 零页 / 尺寸异常），
/// 调用方退回文本提示即可。
pub fn pdf_page_raster(path: &Path, max_edge: u32) -> Option<IconRaster> {
    if max_edge == 0 {
        return None;
    }
    // SAFETY: 全部是 CoreGraphics 的 C 入口，指针要么非空判过、要么来自上面的创建函数。
    unsafe {
        let url = nsurl_for(path)?;
        let doc = CGPDFDocumentCreateWithURL(url);
        if doc.is_null() {
            return None;
        }
        // 页数 0 / 取不到第一页：加密或已损坏的 PDF 会走这里。
        let pages = CGPDFDocumentGetNumberOfPages(doc);
        if pages == 0 {
            CGPDFDocumentRelease(doc);
            return None;
        }
        // 1-based（CoreGraphics 的页码从 1 开始）。
        let page = CGPDFDocumentGetPage(doc, 1);
        if page.is_null() {
            CGPDFDocumentRelease(doc);
            return None;
        }

        let media = CGPDFPageGetBoxRect(page, K_CGPDF_MEDIA_BOX);
        let (pw, ph) = (media.size.width, media.size.height);
        if !pw.is_finite() || !ph.is_finite() || pw <= 0.0 || ph <= 0.0 {
            CGPDFDocumentRelease(doc);
            return None;
        }
        // 等比缩到长边 `max_edge`（页面本身比这小时**不放大**：放大只会糊，
        // 而预览要的是「看得出是什么」）。
        let scale = (max_edge as f64 / pw.max(ph)).min(1.0);
        let w = ((pw * scale).round()).max(1.0) as usize;
        let h = ((ph * scale).round()).max(1.0) as usize;

        let space = CGColorSpaceCreateDeviceRGB();
        if space.is_null() {
            CGPDFDocumentRelease(doc);
            return None;
        }
        let mut rgba = vec![0u8; w * h * 4];
        let ctx = CGBitmapContextCreate(
            rgba.as_mut_ptr(),
            w,
            h,
            8,
            w * 4,
            space,
            K_CG_ALPHA_PREMULTIPLIED_LAST | K_CG_BYTE_ORDER_32_BIG,
        );
        CGColorSpaceRelease(space);
        if ctx.is_null() {
            CGPDFDocumentRelease(doc);
            return None;
        }
        // 先铺白底：PDF 只有文字笔画、页面本身是透明的，不铺底就是一张黑图
        // （透明像素在 PNG 里看着是黑的）。
        CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
        CGContextFillRect(
            ctx,
            NSRect {
                origin: NSPoint { x: 0.0, y: 0.0 },
                size: NSSize {
                    width: w as f64,
                    height: h as f64,
                },
            },
        );
        // 位图上下文是「像素」单位，PDF 页面是「点」单位：先整体缩放再画页面，
        // 剩下的交给 CoreGraphics。
        CGContextScaleCTM(ctx, scale, scale);
        CGContextDrawPDFPage(ctx, page);
        CGContextRelease(ctx);
        CGPDFDocumentRelease(doc);

        Some(IconRaster {
            width: w as u32,
            height: h as u32,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `NSURL` 必须自己处理百分号编码——名字里有空格 / 中文 / `#` 的路径都得能转成 URL。
    ///
    /// 这条是这个模块的**命门**：早年自己拼 `file://` 字符串时，带空格的路径会变成
    /// 「系统说文件不存在」，而报错又只落在系统那一侧，极难定位。
    #[test]
    fn paths_with_spaces_and_unicode_become_urls() {
        for name in [
            "/tmp/plain.txt",
            "/tmp/with space.txt",
            "/tmp/中文 目录/#hash?.txt",
        ] {
            let url = nsurl_for(Path::new(name)).unwrap_or_else(|| panic!("转不出 URL：{name}"));
            unsafe {
                let desc: *mut Object = msg_send![url, absoluteString];
                let utf8: *const std::os::raw::c_char = msg_send![desc, UTF8String];
                let s = std::ffi::CStr::from_ptr(utf8).to_string_lossy().to_string();
                assert!(s.starts_with("file://"), "应当是 file URL：{s}");
                assert!(!s.contains(' '), "空格必须被编码掉：{s}");
            }
        }
    }

    /// `NSWorkspace.sharedWorkspace` 拿得到（AppKit 可用）。
    #[test]
    fn the_shared_workspace_exists() {
        let ws = workspace().expect("NSWorkspace 应当拿得到（AppKit 已链接）");
        assert!(!ws.is_null());
    }

    /// 「能不能推出」的判据：内置盘不给按钮，可弹出 / 可移动 / 网络盘给。
    ///
    /// 这是这条链路上**唯一能进单测的一环**（真机问 AppKit 那步在测试子线程里会先被
    /// `is_main_thread` 拦掉），所以四类卷宗都在这儿钉住——用户报的「内置硬盘也画了
    /// 推出按钮」就靠它不再回来。
    #[test]
    fn only_removable_volumes_can_be_ejected() {
        // 启动盘：internal = true、local = true，其余 false。
        assert!(
            !decide_ejectable(Some(true), Some(false), Some(false), Some(true)),
            "内置硬盘不该给推出按钮"
        );
        // U 盘 / SD 卡 / 光盘：可弹出。
        assert!(decide_ejectable(
            Some(false),
            Some(true),
            Some(false),
            Some(true)
        ));
        // 磁盘映像（.dmg）：可移动但不可弹出。
        assert!(decide_ejectable(
            Some(false),
            Some(false),
            Some(true),
            Some(true)
        ));
        // 网络盘：非本地。
        assert!(decide_ejectable(
            Some(false),
            Some(false),
            Some(false),
            Some(false)
        ));
        // ⚠️ 问不到 key 不算证据：不能因为 `IsLocal` 读不到就当网络盘、把按钮画出来。
        assert!(
            !decide_ejectable(None, None, None, None),
            "全读不到时必须保守（宁可不画按钮）"
        );
        assert!(
            !decide_ejectable(None, Some(false), Some(false), None),
            "只读到一堆 false 也不构成「能推出」"
        );
        // `internal` 优先：内置盘就是不给，哪怕别的 key 说可以。
        assert!(
            !decide_ejectable(Some(true), Some(true), Some(true), Some(false)),
            "internal = true 应当直接否掉"
        );
    }

    /// 非主线程里 `volumes()` 不碰 AppKit：`on_main_thread` 会 `dispatch_sync` 回主
    /// 队列，而 headless 测试的主队列不 drain，真调了就是整套挂死（且**没有 panic**，
    /// 最难查的那种）。这条守的就是「守卫别忘了加」。
    #[test]
    fn volumes_stay_conservative_off_the_main_thread() {
        assert!(
            !is_main_thread(),
            "cargo test 的用例跑在子线程，正好覆盖这条路径"
        );
        assert!(
            volumes().iter().all(|v| !v.ejectable),
            "子线程里必须保守：一块盘都不该带推出按钮"
        );
    }

    /// 「能不能动 AppKit」是**两个**条件的或：调用方在主线程（直接跑），或主 run loop
    /// 已在跑（可以 `dispatch_sync` 回去）。
    ///
    /// 只判前者会让所有后台任务（图标泵就在后台）永远跳过平台调用；只判后者则会在
    /// 测试进程里放行——而测试的主队列无人 drain，`dispatch_sync` 直接挂死。
    #[test]
    fn appkit_needs_either_the_main_thread_or_a_running_loop() {
        assert!(appkit_usable_with(true, false), "主线程上直接跑，不需要闩");
        assert!(
            appkit_usable_with(false, true),
            "闩置位后后台可以派回主队列"
        );
        assert!(
            !appkit_usable_with(false, false),
            "测试进程就是这样：必须保守跳过"
        );
        // 交叉验证：`cargo test` 里闩从未置位，所以后台任务一律保守。
        assert!(!appkit_usable(), "测试进程没标记过主循环，必须保守");
    }
}
