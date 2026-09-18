//! 内联 SVG 图标（Feather/Lucide 风格，24×24、描边 2、圆角端点）。
//!
//! 用 [`gpui_kit::svg`] 的 `.data()` 直接渲染字节，不依赖 AssetSource；
//! 颜色跟随元素的 `text_color`（gpui 用文字色作为 SVG 描边色）。

use std::path::Path;

use gpui_kit::{px, svg, Rgba, Styled, Svg};
use mo_core::{Entry, EntryKind};

pub const ARROW_LEFT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="19" y1="12" x2="5" y2="12"/><polyline points="12 19 5 12 12 5"/></svg>"##;

pub const ARROW_RIGHT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="5" y1="12" x2="19" y2="12"/><polyline points="12 5 19 12 12 19"/></svg>"##;

pub const ARROW_UP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="19" x2="12" y2="5"/><polyline points="19 12 12 5 5 12"/></svg>"##;

pub const ARROW_DOWN: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="5" x2="12" y2="19"/><polyline points="5 12 12 19 19 12"/></svg>"##;

pub const ROTATE_CW: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg>"##;

pub const HOUSE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/></svg>"##;

pub const CHEVRON_RIGHT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"/></svg>"##;

pub const PENCIL: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z"/></svg>"##;

// ---- 窗口控制按钮图标（Win11 风格，24×24、描边 1.5、更细更克制）----
// 顶栏右侧的最小化 / 最大化 / 还原 / 关闭。刻意用细描边贴近原生观感。

pub const WIN_MINIMIZE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><line x1="5" y1="12" x2="19" y2="12"/></svg>"##;

pub const WIN_MAXIMIZE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="5.5" y="5.5" width="13" height="13" rx="1.5"/></svg>"##;

pub const WIN_RESTORE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="8" y="4.5" width="11.5" height="11.5" rx="1.5"/><path d="M16 8H5.5a1 1 0 0 0-1 1V18a1 1 0 0 0 1 1h9"/></svg>"##;

pub const WIN_CLOSE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><line x1="6" y1="6" x2="18" y2="18"/><line x1="18" y1="6" x2="6" y2="18"/></svg>"##;

/// 渲染一个图标：`size` 为边长（px），`color` 为描边色。
pub fn icon(data: &'static [u8], size: f32, color: Rgba) -> Svg {
    svg().data(data).w(px(size)).h(px(size)).text_color(color)
}

// ============================================================================
// 文件类型图标（统一 Lucide / Feather 风格：24×24、描边 2、圆角端点、单色描边）。
//
// 全部 `fill="none" stroke="currentColor"`，颜色跟随调用方传入的 `color`，
// 因此列表选中（蓝底）时传白、常态传文字色即可，天然统一、不会花花绿绿。
// 个别图标用 `fill="currentColor" stroke="none"` 的实心小块做视觉区分
// （如 PDF 的标题条、视频的播放三角），仍是单色，不引入第二种颜色。
// ============================================================================

pub const FOLDER: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9l-.82-1.2A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z"/></svg>"##;

pub const FILE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>"##;

pub const FILE_TEXT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="9" y1="13" x2="15" y2="13"/><line x1="9" y1="17" x2="15" y2="17"/></svg>"##;

pub const FILE_WORD: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><circle cx="9" cy="12" r="0.9" fill="currentColor" stroke="none"/><line x1="11" y1="12" x2="15" y2="12"/><line x1="9" y1="16" x2="15" y2="16"/></svg>"##;

pub const FILE_PDF: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><rect x="9" y="13" width="6" height="2.4" rx="0.6" fill="currentColor" stroke="none"/><line x1="9" y1="18" x2="15" y2="18"/></svg>"##;

pub const FILE_EXCEL: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><rect x="9" y="13" width="6" height="6" rx="1"/><line x1="12" y1="13" x2="12" y2="19"/><line x1="9" y1="16" x2="15" y2="16"/></svg>"##;

pub const FILE_PPT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="9" y1="19" x2="9" y2="15"/><line x1="13" y1="19" x2="13" y2="11"/><line x1="17" y1="19" x2="17" y2="13"/></svg>"##;

pub const FILE_IMAGE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21"/></svg>"##;

pub const FILE_VIDEO: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><path d="m10 11 5 3-5 3z" fill="currentColor" stroke="none"/></svg>"##;

pub const FILE_AUDIO: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18V5l11-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="17" cy="16" r="3"/></svg>"##;

pub const FILE_ARCHIVE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="20" height="5" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8M10 12h4"/></svg>"##;

pub const FILE_CODE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m16 18 6-6-6-6M8 6l-6 6 6 6"/></svg>"##;

pub const SYMLINK: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><path d="M12 21l6-6M18 15v5h-5"/></svg>"##;

// ---- 快捷访问图标（与上面同源风格）----

pub const QA_HOME: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/></svg>"##;

pub const QA_DESKTOP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>"##;

pub const QA_DOWNLOAD: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>"##;

/// 根据条目种类 + 文件名挑选图标（`Entry` / `LightEntry` 都可直接拆出这两个字段）。
pub fn icon_for_kind_and_name(kind: EntryKind, name: &str) -> &'static [u8] {
    match kind {
        EntryKind::Directory => FOLDER,
        EntryKind::Symlink => SYMLINK,
        EntryKind::Other => FILE,
        EntryKind::File => file_ext_icon(name),
    }
}

/// 根据条目挑选图标（便捷封装，透传 `Entry`）。
pub fn entry_icon(entry: &Entry) -> &'static [u8] {
    icon_for_kind_and_name(entry.kind, &entry.name)
}

/// 根据文件名扩展名挑选文件图标（无扩展名 / 不认识 → 通用文件图标）。
pub fn file_ext_icon(name: &str) -> &'static [u8] {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("bmp") | Some("webp")
        | Some("heic") | Some("heif") | Some("tiff") | Some("tif") | Some("svg") | Some("ico") => {
            FILE_IMAGE
        }
        Some("mp4") | Some("mkv") | Some("mov") | Some("avi") | Some("webm") | Some("flv")
        | Some("wmv") | Some("m4v") | Some("mpg") | Some("mpeg") => FILE_VIDEO,
        Some("mp3") | Some("wav") | Some("ogg") | Some("flac") | Some("aac") | Some("m4a")
        | Some("wma") | Some("opus") => FILE_AUDIO,
        Some("pdf") => FILE_PDF,
        Some("doc") | Some("docx") | Some("rtf") | Some("odt") | Some("pages") => FILE_WORD,
        Some("xls") | Some("xlsx") | Some("csv") | Some("ods") | Some("numbers") => FILE_EXCEL,
        Some("ppt") | Some("pptx") | Some("odp") | Some("key") => FILE_PPT,
        Some("txt") | Some("md") | Some("markdown") | Some("log") | Some("json") | Some("xml")
        | Some("yml") | Some("yaml") | Some("toml") | Some("ini") | Some("conf") | Some("cfg") => {
            FILE_TEXT
        }
        Some("rs") | Some("py") | Some("js") | Some("ts") | Some("jsx") | Some("tsx")
        | Some("go") | Some("java") | Some("c") | Some("cc") | Some("cpp") | Some("h")
        | Some("hpp") | Some("cs") | Some("php") | Some("rb") | Some("sh") | Some("bash")
        | Some("html") | Some("htm") | Some("css") | Some("scss") | Some("less") | Some("vue")
        | Some("sql") | Some("dart") | Some("swift") | Some("kt") | Some("scala")
        | Some("gitignore") => FILE_CODE,
        Some("zip") | Some("rar") | Some("7z") | Some("tar") | Some("gz") | Some("bz2")
        | Some("xz") | Some("tgz") | Some("z") | Some("iso") | Some("dmg") => FILE_ARCHIVE,
        _ => FILE,
    }
}

/// 根据快捷访问标签挑图标（标签是 `quick_locations` 返回的中文名）。
pub fn quick_access_icon(label: &str) -> &'static [u8] {
    if label.contains("主目录") {
        QA_HOME
    } else if label.contains("桌面") {
        QA_DESKTOP
    } else if label.contains("文档") {
        FILE
    } else if label.contains("下载") {
        QA_DOWNLOAD
    } else if label.contains("图片") {
        FILE_IMAGE
    } else {
        FOLDER
    }
}
