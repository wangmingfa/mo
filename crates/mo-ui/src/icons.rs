//! 内联 SVG 图标（Feather/Lucide 风格，24×24、描边 2、圆角端点）。
//!
//! 用 [`gpui_kit::svg`] 的 `.data()` 直接渲染字节，不依赖 AssetSource；
//! 颜色跟随元素的 `text_color`（gpui 用文字色作为 SVG 描边色）。

use std::path::Path;

use gpui_kit::{px, svg, Rgba, Styled, Svg};
use mo_core::{Entry, EntryKind};

use crate::panel::ViewMode;

pub const ARROW_LEFT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="19" y1="12" x2="5" y2="12"/><polyline points="12 19 5 12 12 5"/></svg>"##;

pub const ARROW_RIGHT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="5" y1="12" x2="19" y2="12"/><polyline points="12 5 19 12 12 19"/></svg>"##;

pub const ARROW_UP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="19" x2="12" y2="5"/><polyline points="19 12 12 5 5 12"/></svg>"##;

pub const ARROW_DOWN: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="5" x2="12" y2="19"/><polyline points="5 12 12 19 19 12"/></svg>"##;

pub const ROTATE_CW: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg>"##;

pub const CHEVRON_RIGHT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"/></svg>"##;

/// 扫帚（Lucide「brush」形，斜握的刷帚）：任务浮层右上角「一键清除已完成」。
pub const BROOM: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9.06 11.9 8.07-8.06a2.85 2.85 0 1 1 4.03 4.03l-8.06 8.08"/><path d="M7.07 14.94c-1.66 0-3 1.35-3 3.02 0 1.33-2.5 1.52-2 2.02 1.08 1.1 2.49 2.02 4 2.02 2.2 0 4-1.8 4-4.04a3.01 3.01 0 0 0-3-3.02z"/></svg>"##;

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

/// 硬盘 / 盘符图标（「此电脑」里 C: 这类虚拟目录条目用）。
pub const HARD_DRIVE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="22" y1="12" x2="2" y2="12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/><line x1="6" y1="16" x2="6.01" y2="16"/><line x1="10" y1="16" x2="10.01" y2="16"/></svg>"##;

/// 远程连接（地球）：标签页上的「这个标签在看远端而不是本机」徽标，也是
/// [`scheme_icon`] 认不出协议时的兜底。
///
/// 这里要表达的是「当前位置在网络上」；「这是哪一种连接」由协议图标
/// （[`SMB`] / [`FTP`] / [`WEBDAV`]）负责，别混用。
pub const GLOBE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></svg>"##;

/// 加号：侧边栏「远程」标题右侧的「连接到服务器…」入口。
pub const PLUS: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="5" y1="12" x2="19" y2="12"/><line x1="12" y1="5" x2="12" y2="19"/></svg>"##;

/// 电源 / 断开：侧边栏每条远程连接右侧的「断开连接」按钮。
///
/// 用电源符号而不是叉号：断开的是**连接**，不是删掉这条记录——叉号会被读成
/// 「从列表里移除」，而这里真正的语义是「把这条连接关掉」。
pub const POWER: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18.36 6.64a9 9 0 1 1-12.73 0"/><line x1="12" y1="2" x2="12" y2="12"/></svg>"##;

/// 放大镜：命令面板 / 应用选择器的搜索行（替代此前正文里的 🔍 emoji，
/// emoji 在 Windows 上字距与配色都不受主题控制）。
pub const SEARCH: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>"##;

// ---- 远程协议图标（与上面同源风格：24×24、描边 2、单色描边）----
//
// 一个协议一个形状，靠**结构**区分而不是颜色（列表里是单色的）。认不出的协议
// 回落到 [`GLOBE`]，不留空白位。

/// SMB / 网络共享：三台节点连成一张网（共享文件夹挂在网络上）。
pub const SMB: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="16" y="16" width="6" height="6" rx="1"/><rect x="2" y="16" width="6" height="6" rx="1"/><rect x="9" y="2" width="6" height="6" rx="1"/><path d="M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3"/><path d="M12 12V8"/></svg>"##;

/// FTP / SFTP：托盘上一上一下两支箭头——协议本体就是「双向搬运文件」。
pub const FTP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 15v3a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-3"/><path d="M9 3v9"/><polyline points="6 9 9 12 12 9"/><path d="M15 12V3"/><polyline points="12 6 15 3 18 6"/></svg>"##;

/// WebDAV：一朵云（DAV over HTTP(S)，实际用的基本都是坚果云 / Nextcloud 这类网盘）。
pub const WEBDAV: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 10h-1.26A8 8 0 1 0 9 20h9a5 5 0 0 0 0-10z"/></svg>"##;

/// 按协议名挑图标（`scheme` 不带 `://` 与主机部分）。
///
/// scheme 正常由 `RemoteUrl::parse` 归一成了小写，但配置是明文 JSON、可能被手改过，
/// 这里再挡一手大小写。认不出的协议（以及空串）回落到 [`GLOBE`]，不画空位。
pub fn scheme_icon(scheme: &str) -> &'static [u8] {
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "smb" | "cifs" | "samba" => SMB,
        "ftp" | "ftps" | "sftp" | "ssh" => FTP,
        "webdav" | "dav" | "davs" => WEBDAV,
        _ => GLOBE,
    }
}

/// 按远程地址挑协议图标（`endpoint` 形如 `scheme://host[:port]`）。
///
/// 已经握着 `RemoteUrl` 的地方直接用 [`scheme_icon`]（scheme 早就解析好了，别再
/// 切一次字符串）；这条给只有地址串的入口用——配置里的已记住服务器就是。
pub fn protocol_icon(endpoint: &str) -> &'static [u8] {
    scheme_icon(endpoint.split_once("://").map_or("", |(s, _)| s))
}

pub const QA_DESKTOP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="20" height="14" rx="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>"##;

pub const QA_DOWNLOAD: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>"##;

// ── 视图模式按钮组（工具栏里平铺的那一排）───────────────────────────
//
// 四枚对应 [`ViewMode`] 的四个变体，顺序与 `⌘1..⌘4` 一致（见 `ViewMode::ALL`）。
// 前三个取自 Lucide 的 `list` / `layout-grid` / `gallery-thumbnails`，列视图用
// `columns-3`——Finder 工具栏那组也是这个选型。

/// 列表视图（Lucide `list`）：行首小点 + 三条横线。
pub const VIEW_LIST: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h.01"/><path d="M3 12h.01"/><path d="M3 18h.01"/><path d="M8 6h13"/><path d="M8 12h13"/><path d="M8 18h13"/></svg>"##;

/// 网格视图（Lucide `layout-grid`）：四个等大方块。
pub const VIEW_GRID: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="7" height="7" x="3" y="3" rx="1"/><rect width="7" height="7" x="14" y="3" rx="1"/><rect width="7" height="7" x="14" y="14" rx="1"/><rect width="7" height="7" x="3" y="14" rx="1"/></svg>"##;

/// 画廊视图（Lucide `gallery-thumbnails`）：大预览 + 底部一排缩略图标记。
pub const VIEW_GALLERY: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="14" x="3" y="3" rx="2"/><path d="M4 21h1"/><path d="M9 21h1"/><path d="M14 21h1"/><path d="M19 21h1"/></svg>"##;

/// 列视图（Lucide `columns-3`）：三条竖栏。
pub const VIEW_COLUMNS: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="M9 3v18"/><path d="M15 3v18"/></svg>"##;

/// 视图模式 → 工具栏图标。
///
/// 放这里和 [`quick_access_icon`] 同理：图标表集中一处，加视图模式时只改本函数 +
/// [`ViewMode::ALL`]，不用去翻工具栏。
pub fn view_mode_icon(mode: ViewMode) -> &'static [u8] {
    match mode {
        ViewMode::List => VIEW_LIST,
        ViewMode::Grid => VIEW_GRID,
        ViewMode::Gallery => VIEW_GALLERY,
        ViewMode::Columns => VIEW_COLUMNS,
    }
}

/// 根据条目种类 + 文件名挑选图标（`Entry` / `LightEntry` 都可直接拆出这两个字段）。
pub fn icon_for_kind_and_name(kind: EntryKind, name: &str) -> &'static [u8] {
    match kind {
        // 「此电脑」里的盘符条目（C: / D: …）用硬盘图标，与文件夹区分。
        EntryKind::Directory if is_drive_label(name) => HARD_DRIVE,
        EntryKind::Directory => FOLDER,
        EntryKind::Symlink => SYMLINK,
        EntryKind::Other => FILE,
        EntryKind::File => file_ext_icon(name),
    }
}

/// 是否是「C:」这类盘符标签（1 个字母 + 冒号）。
fn is_drive_label(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
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
    } else if label.contains("影片") || label.contains("视频") {
        FILE_VIDEO
    } else {
        FOLDER
    }
}

/// 回收站（Lucide `trash-2`）：侧栏「回收站」入口用。
///
/// 不放进 [`quick_access_icon`]：那套按**路径**高亮、点击走 `open_local`，
/// 回收站是模态面板，语义不同，单独一行。
pub const TRASH: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/></svg>"##;

#[cfg(test)]
mod tests {
    use super::*;

    /// 协议图标：一个协议一个形状，别名归到同一个，认不出的回落到地球。
    #[test]
    fn protocol_icon_maps_scheme_aliases_and_falls_back() {
        assert_eq!(protocol_icon("smb://172.25.48.48"), SMB);
        assert_eq!(protocol_icon("cifs://nas/share"), SMB);
        assert_eq!(protocol_icon("ftp://example.com:2121"), FTP);
        assert_eq!(protocol_icon("sftp://example.com"), FTP);
        assert_eq!(protocol_icon("webdav://cloud.example.com"), WEBDAV);
        assert_eq!(protocol_icon("dav://cloud.example.com"), WEBDAV);
        assert_eq!(protocol_icon("davs://cloud.example.com"), WEBDAV);
        // 配置是明文 JSON，可能被手改过：大小写与畸形地址都不该 panic，也不该空着。
        assert_eq!(protocol_icon("SMB://x"), SMB);
        assert_eq!(protocol_icon("nfs://x"), GLOBE);
        assert_eq!(protocol_icon("没有协议前缀"), GLOBE);
        assert_eq!(protocol_icon(""), GLOBE);
    }

    /// 已经握着 `RemoteUrl` 的入口（侧边栏）直接按 scheme 取图标，
    /// 规则必须与「只有地址串」的那条入口一致——别各定一套映射。
    #[test]
    fn scheme_icon_agrees_with_the_endpoint_form() {
        assert_eq!(scheme_icon("smb"), SMB);
        assert_eq!(scheme_icon("SFTP"), FTP);
        assert_eq!(scheme_icon("davs"), WEBDAV);
        assert_eq!(scheme_icon("nfs"), GLOBE);
        assert_eq!(scheme_icon(""), GLOBE);
        for ep in ["smb://x", "sftp://x", "webdav://x", "nfs://x", ""] {
            let scheme = ep.split_once("://").map_or("", |(s, _)| s);
            assert_eq!(
                protocol_icon(ep),
                scheme_icon(scheme),
                "{ep}：两条入口的映射不一致"
            );
        }
    }
}
