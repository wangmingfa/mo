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

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Protocol, Sel, BOOL};
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

/// 把一个路径送进**系统**废纸篓（访达里那份），返回它在废纸篓里的**实际落点**。
///
/// 用 `NSFileManager.trashItemAtURL:resultingItemURL:error:` 而不是
/// `NSWorkspace.recycleURLs:completionHandler:`：
///
/// * 后者是**异步**的（要带 completionHandler、结果靠回调），在裸二进制 / 后台任务
///   里实测直接返回 NO 而拿不到原因——`NSWorkspace` 那套还要主线程与 run loop；
/// * 前者同步、返回 `NSError`，可以原样把系统的说法带给用户（「宗卷不支持废纸篓」
///   这种只有系统知道）。
///
/// `resultingItemURL` 是自记账的关键（见 `devlog/trash-unify.md`）：系统负责搬
/// （重名自动改名、外接卷进卷上 `.Trashes/<uid>`），落点只有它知道——拿到它，
/// Mo 的回收站账本才能在还原 / 永久删除时定位文件。
///
/// `NSFileManager` 是线程安全的，不必绕主线程。
pub fn recycle_one(path: &Path) -> Result<PathBuf, PlatformError> {
    let Some(url) = nsurl_for(path) else {
        return Err(PlatformError::Failed(format!(
            "无法把路径交给系统：{}",
            path.display()
        )));
    };
    unsafe {
        let manager: *mut Object = msg_send![class("NSFileManager")?, defaultManager];
        let mut error: *mut Object = std::ptr::null_mut();
        let mut out_url: *mut Object = std::ptr::null_mut();
        let ok: bool = msg_send![manager, trashItemAtURL: url resultingItemURL: &mut out_url error: &mut error];
        if !ok {
            let why = error_text(error).unwrap_or_else(|| "系统没有说明原因".to_string());
            return Err(PlatformError::Failed(format!(
                "无法把 {} 送进废纸篓：{why}",
                path.display()
            )));
        }
        if out_url.is_null() {
            return Err(PlatformError::Failed(format!(
                "系统没有返回 {} 在废纸篓里的落点，无法记账",
                path.display()
            )));
        }
        let path_obj: *mut Object = msg_send![out_url, path];
        let c_str: *const std::os::raw::c_char = msg_send![path_obj, UTF8String];
        if c_str.is_null() {
            return Err(PlatformError::Failed(format!(
                "落点路径无法解析：{}",
                path.display()
            )));
        }
        Ok(PathBuf::from(
            std::ffi::CStr::from_ptr(c_str)
                .to_string_lossy()
                .into_owned(),
        ))
    }
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

/// 把一批本机文件写进系统剪贴板（`NSPasteboard` + `NSURL` 数组）。
///
/// ⚠️ 这条**不进单元测试**：它会覆盖开发者机器上真实的剪贴板（与 Windows 侧
/// `write_file_clipboard` 同一条红线）。真发出去那一步用
/// `cargo run -p mo-platform --example clipboard_probe -- <路径>...` 在真机上验。
///
/// `cut` 参数收下但**只能按复制处理**：Finder 的「剪切」记在一条私有 pasteboard
/// 标记里（读侧 §19 同款判据：公开可读 / 可写的类型里没有这一位），写不出来。
/// 与「猜错方向会把用户的文件搬走」同一立场——宁可让对方粘出一份复制。
pub fn write_file_clipboard(paths: &[PathBuf], _cut: bool) -> Result<(), PlatformError> {
    if paths.is_empty() {
        return Err(PlatformError::Failed("没有要写进剪贴板的文件".to_string()));
    }
    let owned = paths.to_vec();
    on_main_thread(move || {
        let pasteboard: *mut Object =
            unsafe { msg_send![class("NSPasteboard")?, generalPasteboard] };
        if pasteboard.is_null() {
            return Err(PlatformError::Failed("拿不到系统剪贴板".to_string()));
        }
        unsafe {
            // `clearContents` 是 `writeObjects:` 的前置——它返回新的 changeCount，
            // 0 表示失败。跳过它直接写是老式 `setData:` 的混用，会被 AppKit 拒。
            let change_count: isize = msg_send![pasteboard, clearContents];
            if change_count == 0 {
                return Err(PlatformError::Failed("清空系统剪贴板失败".to_string()));
            }
            let array: *mut Object =
                msg_send![class("NSMutableArray")?, arrayWithCapacity: owned.len()];
            for p in &owned {
                let Some(url) = nsurl_for(p) else {
                    return Err(PlatformError::Failed(format!(
                        "无法把路径交给剪贴板：{}",
                        p.display()
                    )));
                };
                let _: () = msg_send![array, addObject: url];
            }
            // NSURL 自己遵守 NSPasteboardWriting，会写出 `public.file-url`；
            // 访达与其它应用按这个类型粘出文件。
            let ok: bool = msg_send![pasteboard, writeObjects: array];
            if !ok {
                return Err(PlatformError::Failed("写系统剪贴板失败".to_string()));
            }
        }
        Ok(())
    })
}

// -------------------------------------------------------------- 拖出到系统

/// `NSPoint` 的 Rust 镜像。`objc` 0.2 的 `Encode` 没有结构体编码的现成实现，
/// 手工按 Apple 的类型编码表给（`{NSPoint=dd}`）。
#[repr(C)]
#[derive(Clone, Copy)]
struct DragPoint {
    x: f64,
    y: f64,
}

// SAFETY: 编码串与 `NSPoint` 的真实布局一致（双 `double`）。
unsafe impl objc::Encode for DragPoint {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{NSPoint=dd}") }
    }
}

/// `NSRect` 的 Rust 镜像（`NSSize` 同为双 `double`，复用 [`DragPoint`]）。
#[repr(C)]
#[derive(Clone, Copy)]
struct DragRect {
    origin: DragPoint,
    size: DragPoint,
}

// SAFETY: 编码串与 `NSRect` 的真实布局一致。
unsafe impl objc::Encode for DragRect {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{NSRect={NSPoint=dd}{NSSize=dd}}") }
    }
}

/// 拖拽结论回调。`None` = 没落地（取消 / Esc / 起拖失败）。
type DragDone = Box<dyn FnOnce(Option<bool>) + Send>;

/// `NSDragOperation` 里我们关心的两位。
const DRAG_OP_COPY: usize = 1;
const DRAG_OP_MOVE: usize = 16;
/// **只声明复制**。曾与 Windows 同口径「复制与移动都声明、目标定」，实测发现
/// 交互不对等：拖出（同卷）被访达判成移动、拖入（gpui 的 drop 通道拿不到修饰
/// 键 / 操作码，见 `mo_ui::app::submit_os_drop`）却恒是复制——同一对文件在两个
/// 方向上下场不同。拖拽语义统一成复制（拖入拖出都一样），移动走剪切粘贴；
/// `drag_ended` 的 MOVE 分支留着，是给将来接修饰键透传后的移动语义的。
const DRAG_MASK: usize = DRAG_OP_COPY;

/// 拖拽源类（`NSDraggingSource`）：一次拖拽一个实例，握着把结论递回 UI 的回调。
///
/// 不用 `declare_class!` 而手工 `ClassDecl`：要挂协议（`add_protocol`），顺手把三个
/// 方法的注册写在一处。类只注册一次（`OnceLock`）。
fn drag_source_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<&'static Class> = std::sync::OnceLock::new();
    CLASS.get_or_init(|| {
        let superclass = Class::get("NSObject").expect("NSObject 一定在");
        let mut decl = ClassDecl::new("MoDragSource", superclass).expect("注册 MoDragSource");
        // 回调槽：`Box<Option<DragDone>>` 的裸指针存成 usize（`None` = 已消费）。
        decl.add_ivar::<usize>("mo_callback");
        unsafe {
            decl.add_method(
                sel!(draggingSourceOperationMaskForLocal:),
                drag_mask_for_local as extern "C" fn(&mut Object, Sel, BOOL) -> usize,
            );
            // 新式会话（beginDraggingSession）问的是这一个；旧式问上面那个。
            // 两个都答，谁问都一样。
            decl.add_method(
                sel!(draggingSession:sourceOperationMaskForDraggingContext:),
                drag_mask_for_context
                    as extern "C" fn(&mut Object, Sel, *mut Object, isize) -> usize,
            );
            decl.add_method(
                sel!(draggingSession:endedAtPoint:operation:),
                drag_ended as extern "C" fn(&mut Object, Sel, *mut Object, DragPoint, usize),
            );
        }
        decl.add_protocol(Protocol::get("NSDraggingSource").expect("NSDraggingSource"));
        decl.register()
    })
}

extern "C" fn drag_mask_for_local(_this: &mut Object, _sel: Sel, _local: BOOL) -> usize {
    DRAG_MASK
}

extern "C" fn drag_mask_for_context(
    _this: &mut Object,
    _sel: Sel,
    _session: *mut Object,
    _context: isize,
) -> usize {
    DRAG_MASK
}

/// 会话结束：按目标回的操作判「移动 / 复制 / 没落地」，把回调消费掉。
extern "C" fn drag_ended(
    this: &mut Object,
    _sel: Sel,
    _session: *mut Object,
    _at: DragPoint,
    operation: usize,
) {
    let Some(done) = take_drag_callback(this) else {
        return;
    };
    let result = if operation & DRAG_OP_MOVE != 0 {
        Some(true)
    } else if operation & DRAG_OP_COPY != 0 {
        Some(false)
    } else {
        None
    };
    done(result);
}

/// 取走回调槽（只消费一次；`None` = 已经取过 / 从没放过）。
fn take_drag_callback(this: &mut Object) -> Option<DragDone> {
    let slot = unsafe { *this.get_ivar::<usize>("mo_callback") };
    if slot == 0 {
        return None;
    }
    unsafe { this.set_ivar("mo_callback", 0usize) };
    let boxed = unsafe { Box::from_raw(slot as *mut Option<DragDone>) };
    *boxed
}

/// 把一批本机文件起一次**真**拖拽（`NSView.beginDraggingSessionWithItems:`）。
///
/// ⚠️ 这条**不进单元测试**：起真拖拽会抓住用户的指针跟着走。真机验证只能人工：
/// 按住文件拖出 Mo 窗口、落到访达里松手。
///
/// 手写拖拽没有留下 NSEvent，但按住拖动期间窗口**持续**收到 `mouseDragged`
/// （事件循环跟着按键走，与指针在不在窗口里无关），`[NSApp currentEvent]` 就是
/// 最新那一条——起拖直接用它，拖图的位置也锚在它上面（AppKit 记的是「拖图离
/// 事件位置的距离」作为光标偏移）。
pub fn begin_drag(paths: Vec<PathBuf>, on_done: DragDone) -> bool {
    if paths.is_empty() {
        return false;
    }
    if !appkit_usable() {
        return false;
    }
    // AppKit 调用；不在主线程就丢回去（Mo 的起拖点在 gpui 输入回调里，多半已在）。
    on_main_thread(move || unsafe { begin_drag_on_main(paths, on_done) })
}

/// ⚠️ 只在主线程跑（见 [`begin_drag`]）。
unsafe fn begin_drag_on_main(paths: Vec<PathBuf>, on_done: DragDone) -> bool {
    let declined = |slot: *mut Option<DragDone>| {
        // 起拖失败：回调作废（契约是「没起来就不会被调」）。
        drop(unsafe { Box::from_raw(slot) });
        false
    };
    let slot: *mut Option<DragDone> = Box::into_raw(Box::new(Some(on_done)));

    let Some(app_cls) = Class::get("NSApplication") else {
        return declined(slot);
    };
    let app: *mut Object = msg_send![app_cls, sharedApplication];
    if app.is_null() {
        return declined(slot);
    }
    // 按住拖动时 Mo 的窗口是 key；拿不到再试 main。
    let mut window: *mut Object = msg_send![app, keyWindow];
    if window.is_null() {
        window = msg_send![app, mainWindow];
    }
    if window.is_null() {
        return declined(slot);
    }
    let view: *mut Object = msg_send![window, contentView];
    if view.is_null() {
        return declined(slot);
    }
    let event: *mut Object = msg_send![app, currentEvent];
    if event.is_null() {
        return declined(slot);
    }
    // `NSEventTypeLeftMouseDown = 1`、`LeftMouseDragged = 6`。别的类型（滚轮 /
    // 键盘……）说明此刻根本没有拖拽进行，不硬起。
    let event_type: isize = msg_send![event, type];
    if event_type != 1 && event_type != 6 {
        return declined(slot);
    }
    let location: DragPoint = msg_send![event, locationInWindow];

    let Some(items_cls) = Class::get("NSMutableArray") else {
        return declined(slot);
    };
    let items: *mut Object = msg_send![items_cls, array];
    for p in &paths {
        let Some(url) = nsurl_for(p) else {
            return declined(slot);
        };
        let Some(item_cls) = Class::get("NSDraggingItem") else {
            return declined(slot);
        };
        let item: *mut Object = msg_send![item_cls, alloc];
        let item: *mut Object = msg_send![item, initWithPasteboardWriter: url];
        if item.is_null() {
            return declined(slot);
        }
        // 拖图按**扩展名**取（与 gpui-pre 同款）：`iconForFile:` 会同步打
        // LaunchServices（还会抖，见 icon.rs 的坑），一批几十条能把起拖卡住。
        let kind = if p.is_dir() {
            "public.folder".to_string()
        } else {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_string())
                .unwrap_or_else(|| "public.data".to_string())
        };
        let Some(ns_kind) = nsstring(&kind) else {
            return declined(slot);
        };
        let ws: *mut Object = msg_send![Class::get("NSWorkspace").unwrap(), sharedWorkspace];
        let icon: *mut Object = msg_send![ws, iconForFileType: ns_kind];
        // 拖图框锚在事件位置上：AppKit 把这个框与事件位置的距离记成拖图的光标偏移。
        let frame = DragRect {
            origin: DragPoint {
                x: location.x - 16.0,
                y: location.y - 16.0,
            },
            size: DragPoint { x: 32.0, y: 32.0 },
        };
        // 便捷入口是**一条**消息：setDraggingFrame:contents:（frame 与 contents
        // 同发）。没有 setImageContents: 这个选择器——上一版把它拆成两条发，
        // 第二条打到不认识它的 NSDraggingItem 上，直接 NSInvalidArgumentException 闪退。
        let _: () = msg_send![item, setDraggingFrame: frame contents: icon];
        let _: () = msg_send![items, addObject: item];
    }
    let count: usize = msg_send![items, count];
    if count == 0 {
        return declined(slot);
    }

    let cls = drag_source_class();
    let source: *mut Object = msg_send![cls, alloc];
    let source: *mut Object = msg_send![source, init];
    if source.is_null() {
        return declined(slot);
    }
    (*source).set_ivar("mo_callback", slot as usize);

    let session: *mut Object =
        msg_send![view, beginDraggingSessionWithItems: items event: event source: source];
    if session.is_null() {
        // source 这时还没被 AppKit 持有，我们自己的 +1 自己放。
        (*source).set_ivar("mo_callback", 0usize);
        let _: () = msg_send![source, release];
        return declined(slot);
    }
    // 会话持有 source 到结束；结束时 AppKit 放掉 → dealloc。我们这边的 +1 用
    // autorelease 交出去，引用账目两清。
    let _: () = msg_send![source, autorelease];
    true
}

/// 原生目录选择框（`NSOpenPanel`，只选目录）。
///
/// `runModal` 在主线程上跑模态循环，所以走 [`on_main_thread`]；用户点「取消」
/// 时 `runModal` 回 `NSModalResponseCancelled`，那是正常出路不是错误，折成
/// `Ok(None)`。
pub fn pick_folder(title: &str) -> Result<Option<PathBuf>, PlatformError> {
    let owned = title.to_string();
    on_main_thread(move || {
        let Some(cls) = Class::get("NSOpenPanel") else {
            return Err(PlatformError::Failed(
                "系统里没有 NSOpenPanel（AppKit 没链接上？）".into(),
            ));
        };
        unsafe {
            let panel: *mut Object = msg_send![cls, openPanel];
            if panel.is_null() {
                return Err(PlatformError::Failed("NSOpenPanel 建不出来".into()));
            }
            let Some(title) = nsstring(&owned) else {
                return Err(PlatformError::Failed("标题无法交给系统".into()));
            };
            let _: () = msg_send![panel, setTitle: title];
            let _: () = msg_send![panel, setCanChooseDirectories: true];
            let _: () = msg_send![panel, setCanChooseFiles: false];
            let _: () = msg_send![panel, setAllowsMultipleSelection: false];
            // `NSModalResponseOK == 1`；取消（按钮 / Esc）回 0。
            let response: isize = msg_send![panel, runModal];
            if response != 1 {
                return Ok(None);
            }
            let urls: *mut Object = msg_send![panel, URLs];
            let url: *mut Object = msg_send![urls, firstObject];
            if url.is_null() {
                return Ok(None);
            }
            let path_obj: *mut Object = msg_send![url, path];
            let c_str: *const std::os::raw::c_char = msg_send![path_obj, UTF8String];
            if c_str.is_null() {
                return Err(PlatformError::Failed("所选目录的路径无法解析".into()));
            }
            Ok(Some(PathBuf::from(
                std::ffi::CStr::from_ptr(c_str)
                    .to_string_lossy()
                    .into_owned(),
            )))
        }
    })
}

/// 原生文件选择框（`NSOpenPanel`，只选文件）。
///
/// 与 [`pick_folder`] 同一条纪律：走 [`on_main_thread`] 跑模态、取消折成 `Ok(None)`。
/// 区别只在 `[panel setCanChooseFiles:true]` + `[panel setCanChooseDirectories:false]`
/// ——只让选文件，不让选目录。
pub fn pick_file(title: &str) -> Result<Option<PathBuf>, PlatformError> {
    let owned = title.to_string();
    on_main_thread(move || {
        let Some(cls) = Class::get("NSOpenPanel") else {
            return Err(PlatformError::Failed(
                "系统里没有 NSOpenPanel（AppKit 没链接上？）".into(),
            ));
        };
        unsafe {
            let panel: *mut Object = msg_send![cls, openPanel];
            if panel.is_null() {
                return Err(PlatformError::Failed("NSOpenPanel 建不出来".into()));
            }
            let Some(title) = nsstring(&owned) else {
                return Err(PlatformError::Failed("标题无法交给系统".into()));
            };
            let _: () = msg_send![panel, setTitle: title];
            let _: () = msg_send![panel, setCanChooseFiles: true];
            let _: () = msg_send![panel, setCanChooseDirectories: false];
            let _: () = msg_send![panel, setAllowsMultipleSelection: false];
            let response: isize = msg_send![panel, runModal];
            if response != 1 {
                return Ok(None);
            }
            let urls: *mut Object = msg_send![panel, URLs];
            let url: *mut Object = msg_send![urls, firstObject];
            if url.is_null() {
                return Ok(None);
            }
            let path_obj: *mut Object = msg_send![url, path];
            let c_str: *const std::os::raw::c_char = msg_send![path_obj, UTF8String];
            if c_str.is_null() {
                return Err(PlatformError::Failed("所选文件的路径无法解析".into()));
            }
            Ok(Some(PathBuf::from(
                std::ffi::CStr::from_ptr(c_str)
                    .to_string_lossy()
                    .into_owned(),
            )))
        }
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
                                       // ⚠️ 必须 CString：`&str` 不保证 NUL 结尾，直接 `as_ptr()` 传给
                                       // `stringWithUTF8String:` 会读到越界——堆上的 String 后面凑巧是 0 才侥幸能用，
                                       // 字面量（如 "NSFolder"）后面是别的数据，拼出乱码名，`imageNamed:` 就 nil。
    let c = std::ffi::CString::new(s).ok()?;
    let obj: *mut Object = unsafe { msg_send![cls, stringWithUTF8String: c.as_ptr()] };
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
        draw_ns_image_to_raster(image, px)
    })
}

/// 取一个**扩展名**在系统里的图标（`NSWorkspace.iconForFileType:`），契约同
/// [`file_icon_raster`]：主线程做、只交像素、编码归调用方。
///
/// 给「文件本体已不在、扩展名还在」的场景用——**回收站条目**是典型：原路径多半
/// 已经不存在，`iconForFile:` 对不存在的路径只会给一张通用白纸图标；按扩展名问
/// 拿到的就是访达里那个「.txt = 文本文档」的真图标。`ext` **不带点**（`"txt"`，
/// 带点也会被剥掉）；空扩展名返回 `None`。
///
/// ⚠️ `iconForFileType:` 自 10.13 标了 deprecated（官方建议换 `iconForContentType:`，
/// 那要引 CoreServices + UTType 桥接，收益只是消一条提醒）——它依然工作正常，
/// 继续用，账记在这里。
pub fn ext_icon_raster(ext: &str, px: u32) -> Option<IconRaster> {
    let ext = ext.trim_start_matches('.').to_string();
    if ext.is_empty() {
        return None;
    }
    on_main_thread(move || unsafe {
        let ns_ext = nsstring(&ext)?;
        let ws = workspace().ok()?;
        let image: *mut Object = msg_send![ws, iconForFileType: ns_ext];
        if image.is_null() {
            return None;
        }
        draw_ns_image_to_raster(image, px)
    })
}

/// 取**通用文件夹**的系统图标（`NSImage imageNamed: NSFolder`），契约同
/// [`file_icon_raster`]：主线程做、只交像素、编码归调用方。
///
/// 走 AppKit **资产目录**而不是 `iconForFile:`：不碰任何文件路径，也就不吃 macOS
/// 图标服务的抖动（同一路径连问两次可能一次给图、下一次瞬间 nil）。上层拿它当
/// 「目录行在真图标就位前的占位图」，让目录永远不落到内置描边 SVG 上。
pub fn folder_icon_raster(px: u32) -> Option<IconRaster> {
    on_main_thread(move || unsafe {
        // `NSImageNameFolder` 的字符串值就是 "NSFolder"。分步取，别把 `?` 内联进
        // `msg_send!` 的参数里（宏展开后 `?` 的归属不如展开前直观，容易踩坑）。
        let Some(cls) = Class::get("NSImage") else {
            eprintln!("dbg: no NSImage class");
            return None;
        };
        let Some(name) = nsstring("NSFolder") else {
            eprintln!("dbg: no nsstring");
            return None;
        };
        let image: *mut Object = msg_send![cls, imageNamed: name];
        if image.is_null() {
            return None;
        }
        draw_ns_image_to_raster(image, px)
    })
}

/// 把一张 `NSImage` 重绘到 `px` 见方后交出 RGBA 像素（AppKit 之下的纯 CoreGraphics 段，
/// [`file_icon_raster`] / [`folder_icon_raster`] 共用）。
///
/// * 至少 1px：0 会让 `CGBitmapContextCreate` 拿到 0 长度缓冲。
/// * `CGImageForProposedRect:` 给的是**原始尺寸**的位图（512×512 起），要真画一张
///   小的而不是 `setSize:`（那只改逻辑尺寸）。也不走「lockFocus → TIFF →
///   NSBitmapImageRep」：那条路在 Retina 上按 2x 出图且解出来是 16 位/通道，按 8 位
///   读就错位。CG 这边可以直接缩放到位、位深由我们指定。
///
/// ⚠️ 必须在主线程调（入参是 AppKit 对象；调用方负责已经 `on_main_thread`）。
unsafe fn draw_ns_image_to_raster(image: *mut Object, px: u32) -> Option<IconRaster> {
    let side = px.max(1) as f64;
    let px = side as usize;
    let src = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize {
            width: side,
            height: side,
        },
    };
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

    /// 拖拽源类只注册一次，且 `NSDraggingSource` 的关键方法都挂在实例上。
    ///
    /// 这条**不起真拖拽**（那会抓住用户的指针），只验注册本身——方法漏挂 /
    /// 协议没挂的错，AppKit 要到用户拖出去那一刻才炸，单测能在这一步就拦住。
    #[test]
    fn drag_source_class_registers_with_its_methods() {
        let cls = drag_source_class();
        // 第二次拿必须是同一个类（OnceLock 幂等）。
        assert!(std::ptr::eq(cls, drag_source_class()));
        let check = |name: &str| {
            let sel = Sel::register(name);
            // `responds_to` 走元类查类方法；实例方法要用 instancesRespondToSelector，
            // objc 0.2 没包这一层，直接发消息问。
            let yes: BOOL = unsafe { msg_send![cls, instancesRespondToSelector: sel] };
            assert_eq!(yes, objc::runtime::YES, "MoDragSource 应响应 {name}");
        };
        check("draggingSourceOperationMaskForLocal:");
        check("draggingSession:sourceOperationMaskForDraggingContext:");
        check("draggingSession:endedAtPoint:operation:");

        // 我们对**系统类**发的选择器也要在测试里核一遍：objc 的消息编译期不校验，
        // 拼错选择器（或拆错参数）要到用户拖出去那一刻才 NSInvalidArgumentException
        // 闪退。这次起拖挂掉的就是这类——`setImageContents:` 根本不存在，正确的是
        // setDraggingFrame:contents:。
        let system = |cls_name: &str, name: &str| {
            let Some(cls) = Class::get(cls_name) else {
                panic!("系统类 {cls_name} 不该缺席");
            };
            let sel = Sel::register(name);
            let yes: BOOL = unsafe { msg_send![cls, instancesRespondToSelector: sel] };
            assert_eq!(yes, objc::runtime::YES, "{cls_name} 应响应 {name}");
        };
        system("NSDraggingItem", "setDraggingFrame:contents:");
        system("NSDraggingItem", "initWithPasteboardWriter:");
        system("NSWorkspace", "iconForFileType:");
    }
}
