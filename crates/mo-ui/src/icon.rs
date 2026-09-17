//! 应用图标：把 `assets/icon.png` 在运行时设为 macOS Dock 图标。
//!
//! Mo 平时用 `cargo run` 直接跑裸二进制，没有 `.app` 包、也没有 Info.plist，
//! Dock 里只会显示通用可执行文件图标。这里在启动时把嵌入的 PNG
//! 喂给 `NSApplication.setApplicationIconImage`，让 Dock / ⌘Tab 显示 Mo 自己的图标。
//!
//! * 图标在编译期通过 `include_bytes!` 嵌入，发布单文件二进制时无需携带资源；
//! * 非 macOS 平台是 no-op；
//! * 重新生成图标：`python3 scripts/make_icon.py`（见脚本注释）。

/// 1024×1024 带透明圆角的应用图标（由 `scripts/make_icon.py` 生成）。
#[cfg(target_os = "macos")]
const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon.png");

/// 设置应用图标。幂等，启动时调用一次即可。
pub fn set_dock_icon() {
    #[cfg(target_os = "macos")]
    unsafe {
        set_dock_icon_macos()
    }
}

#[cfg(target_os = "macos")]
unsafe fn set_dock_icon_macos() {
    use objc::{class, msg_send, runtime::Object, sel, sel_impl};

    // PNG 字节 → NSData → NSImage → NSApplication.setApplicationIconImage。
    // 全程走 objc 运行时消息发送，不需要链接 AppKit 的符号。
    let data: *mut Object = msg_send![
        class!(NSData),
        dataWithBytes: ICON_PNG.as_ptr()
        length: ICON_PNG.len()
    ];
    if data.is_null() {
        tracing::warn!("应用图标：NSData 创建失败，跳过");
        return;
    }
    let image: *mut Object = msg_send![class!(NSImage), alloc];
    let image: *mut Object = msg_send![image, initWithData: data];
    let _: () = msg_send![data, release];
    if image.is_null() {
        tracing::warn!("应用图标：NSImage 解码失败，跳过");
        return;
    }
    let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
    if app.is_null() {
        // NSApplication 尚未创建（不应发生：我们在 application().run 之后调用）。
        tracing::warn!("应用图标：NSApplication 未就绪，跳过");
        let _: () = msg_send![image, release];
        return;
    }
    let _: () = msg_send![app, setApplicationIconImage: image];
    let _: () = msg_send![image, release]; // setApplicationIconImage 内部已 retain。
}
