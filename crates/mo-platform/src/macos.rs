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

use crate::{PlatformError, Volume};

/// `NSWorkspace` 的图标类型枚举值（`NSWorkspaceIconCreationOptions` 之前就是
/// `NSCompositeImageRep` / 直接 `iconForFile:`）。这里只用 `iconForFile:`。
///
/// `NSBitmapImageRep representationUsingType:properties:` 的第二个参数是
/// `NSDictionary *`，传 `nil` 即可（不指定额外属性）。
///
/// `NSPNGFileType` 在 AppKit 头里是 `4`（`NSBitmapImageFileType` 是 `NSUInteger`）。
const NSPNG_FILE_TYPE: usize = 4;

/// 系统图标要出的像素尺寸。
///
/// 列表行是 20pt 见方（`file_item` 里 `img(...).w(px(20.0)).h(px(20.0))`），
/// 按 @2x 屏幕取 40px——再大就是白白多花编码时间与内存。
const ICON_PX: f64 = 40.0;

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
/// ⚠️ 不在主线程一律返回 `false`（= 不给按钮），与 `file_icon` 同一条纪律：读
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

/// 取一个文件在系统里的图标（PNG 字节）。
///
/// `NSWorkspace.iconForFile:` 给的是 `NSImage`；转 PNG 走
/// `TIFFRepresentation` → `NSBitmapImageRep` → `representationUsingType:properties:`。
/// 整段包在 `on_main_thread` 里——`NSWorkspace` 的调用按 Apple 的约定要在主线程。
pub fn file_icon(path: &Path) -> Option<Vec<u8>> {
    let owned = path.to_path_buf();
    on_main_thread(move || unsafe {
        let s = owned.to_string_lossy();
        let nsstring = nsstring(&s)?;
        // 闭包返回 `Option`，所以 Result 这边的 `?` 要走 `.ok()?`（失败即 `None`）。
        let ws = workspace().ok()?;
        let image: *mut Object = msg_send![ws, iconForFile: nsstring];
        if image.is_null() {
            return None;
        }
        // ⚠️ `iconForFile:` 给的是**原始尺寸**的 NSImage（512×512 起，转出来的 PNG 常有
        // 几百 KB）。实测每张要 250–500ms——而列表一屏要几十张，直接编码就是几秒的
        // 沙滩球（用户报的「启动后完全卡住、鼠标一直转圈」）。
        //
        // `setSize:` 只改逻辑尺寸，`TIFFRepresentation` 照样按原始像素出图（试过：PNG
        // 仍是 787KB、耗时几乎没降），所以要**真画一张小的**：新建 40px 画布 → 把原图
        // `drawInRect:` 进去 → 再走 TIFF → PNG。PNG 从几百 KB 掉到几 KB。
        let size = NSSize {
            width: ICON_PX,
            height: ICON_PX,
        };
        let small: *mut Object = msg_send![class("NSImage").ok()?, alloc];
        let small: *mut Object = msg_send![small, initWithSize: size];
        if small.is_null() {
            return None;
        }
        // `lockFocus` 把这张新图设成当前绘图上下文（AppKit 要求主线程，我们就在主线程）。
        let rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        };
        let _: () = msg_send![small, lockFocus];
        let _: () = msg_send![image, drawInRect: rect];
        let _: () = msg_send![small, unlockFocus];
        // NSImage → TIFF 数据（通用中间格式，任何 NSImage 都有）。
        let tiff: *mut Object = msg_send![small, TIFFRepresentation];
        // `alloc` / `initWithSize:` 的持有权在我们手里（没开 ARC），画完就还。
        let _: () = msg_send![small, release];
        if tiff.is_null() {
            return None;
        }
        // TIFF → NSBitmapImageRep（才能转成别的格式）。
        let rep: *mut Object = msg_send![class("NSBitmapImageRep").ok()?, imageRepWithData: tiff];
        if rep.is_null() {
            return None;
        }
        // NSBitmapImageRep → PNG 字节（NSData）。
        let png: *mut Object = msg_send![rep, representationUsingType: NSPNG_FILE_TYPE properties: std::ptr::null_mut::<Object>()];
        if png.is_null() {
            return None;
        }
        let bytes: *const std::os::raw::c_uchar = msg_send![png, bytes];
        let len: usize = msg_send![png, length];
        if bytes.is_null() || len == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(bytes, len).to_vec())
    })
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
}
