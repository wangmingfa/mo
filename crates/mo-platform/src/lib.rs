//! mo-platform：桌面平台的**原生集成**（回收站、在文件管理器里显示、推出卷宗）。
//!
//! 这些东西每个平台做法都不一样，而且都不是「纯逻辑」——它们要调系统的
//! AppKit / GVfs / Shell32。单独一个 crate 的好处：
//!
//! * `mo-core` / `mo-operations` 保持**纯逻辑可单测**（`mo_operations::delete` 至今
//!   写着「回收站由 mo-platform 后续提供」，就是留给这里的）；
//! * 平台分支集中在一处，上层只问「这个平台支不支持」；
//! * 没有对应实现的平台给 [`PlatformError::Unsupported`]，上层退回自己的做法，
//!   而不是静默失败。
//!
//! ## 为什么「回收站」必须走系统
//!
//! 自己往 `~/.Trash` 里搬文件在 macOS 上只对**启动卷**成立：外接卷宗的废纸篓在卷
//! 宗根目录下（`.Trashes/<uid>`），而且访达的「清空废纸篓」、系统「关于本机」里的
//! 容量统计都只认系统那份。用 `NSWorkspace recycleURLs:` 才是正解。

mod macos;

use std::path::{Path, PathBuf};

/// 平台原生操作的失败。
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// 这个平台没有对应实现（或系统 API 不可用）：上层应当退回自己的做法。
    #[error("当前平台不支持这个操作：{0}")]
    Unsupported(&'static str),
    /// 系统 API 调了，但没能做成（原因原样带回来给用户看）。
    #[error("{0}")]
    Failed(String),
}

/// 这个平台支持「把文件送进**系统**回收站」吗。
pub fn supports_trash() -> bool {
    cfg!(target_os = "macos")
}

/// 把 `paths` 送进系统回收站（可撤销 —— 由系统的废纸篓负责）。
///
/// 返回 `Err(PlatformError::Unsupported)` 时上层应当退回自己的回收站实现
/// （`mo_operations` 那套目录），不要静默吞掉。
pub fn recycle(paths: &[PathBuf]) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        macos::recycle(paths)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = paths;
        Err(PlatformError::Unsupported("回收站"))
    }
}

/// 这个平台支持「在系统文件管理器里定位一个文件」吗。
pub fn supports_reveal() -> bool {
    cfg!(target_os = "macos")
}

/// 「在系统文件管理器中显示」在这个平台上的**叫法**。
///
/// 菜单上写「在文件管理器中显示」在 macOS 上是错的——那里叫访达。UI 层拿这个
/// 名字当菜单标签，别自己按平台写死。
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "在访达中显示"
    } else {
        "在文件管理器中显示"
    }
}

/// 在系统的文件管理器里**显示** `path`（macOS 上就是「在访达中显示」）。
pub fn reveal(path: &Path) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        macos::reveal(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(PlatformError::Unsupported("在文件管理器中显示"))
    }
}

/// 推出 / 卸载一个卷宗（外接盘、网络盘）。
///
/// 路径是**挂载点**（`/Volumes/share`、`/mnt/usb`…），不是卷宗设备文件。
pub fn eject(path: &Path) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        macos::eject(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(PlatformError::Unsupported("推出卷宗"))
    }
}

/// 一块**已挂载的卷宗**（访达侧边栏「位置」里那种）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// 展示名（macOS 就是 `/Volumes` 下的目录名）。
    pub name: String,
    /// 挂载点（`open` 它就是进这块盘）。
    pub path: PathBuf,
    /// 这块盘能不能「推出」。
    ///
    /// **内置硬盘（启动盘）不能**——访达也不给它画推出按钮。会推的只三类：可弹出
    /// 介质（USB / SD / 光盘）、可移动卷（磁盘映像）、非本地卷（网络盘）。UI 据此
    /// 决定要不要在行尾摆那个按钮；对一个推不动的盘摆按钮，点下去只会得到一句
    /// 「推出失败」。
    ///
    /// 判据拿不到时（非 macOS / 不在主线程）保守给 `false`。
    pub ejectable: bool,
}

/// 本平台能不能列出卷宗（侧边栏「位置」区据此决定要不要摆出来）。
pub fn supports_volumes() -> bool {
    cfg!(target_os = "macos")
}

/// 这个平台能不能给「一个具体文件」画出**系统**图标（访达里那种真实图标）。
pub fn supports_file_icons() -> bool {
    cfg!(target_os = "macos")
}

/// 取 `path` 在**系统**里的图标，返回一块**重绘到固定尺寸**的 RGBA 像素。
///
/// 与内置的 Lucide 单色 SVG 图标不同，系统图标是**彩色光栅图**（`.app` 显示真实
/// App 图标、文档显示所属 App 的图标）。上层把它编码成 PNG 文件、用 `img()` 加载，
/// 别试图拿它当 SVG 描边（它没有「文字色」概念）。
///
/// ## 为什么交出去的是**像素**而不是 PNG
///
/// 这一段只能主线程做（`iconForFile:` 是 AppKit，`on_main_thread` 会
/// `dispatch_sync` 回主队列——**活就是主线程干的**），而「像素 → PNG」的编码占了
/// 整段耗时的 **70%**（实测 0.62ms / 0.9ms）。编码没有理由留在主线程：把它交给
/// 调用方，在上层自己选定的后台线程里做，主线程单价就掉到 ~0.26ms。
///
/// 所以这里的契约是：**只做必须主线程的活**，交出像素，编码归调用方
/// （见 `mo_thumbnails::encode_rgba_png` / `unpremultiply_rgba`）。
///
/// 拿不到（路径不存在 / 平台不支持 / 系统没给）返回 `None`，上层退回内置 SVG。
pub fn file_icon_raster(path: &Path) -> Option<IconRaster> {
    #[cfg(target_os = "macos")]
    {
        macos::file_icon_raster(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// 一张图标位图的**原始像素**：RGBA8、**预乘 alpha**。
///
/// 它是「系统图标」这条链路上主线程与后台之间的交接物：主线程负责取回它，
/// 后台负责把它编码成 PNG。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconRaster {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` 字节，通道序 `R, G, B, A`。
    ///
    /// ⚠️ **预乘**：AppKit 把图画进位图时总会把 RGB 按 alpha 缩一遍（半透明像素的
    /// 颜色被压暗）。直接当直通 alpha 的 RGBA 编码，抗锯齿边缘会发暗。
    /// 编码前先还原（`mo_thumbnails::unpremultiply_rgba`）。
    pub rgba: Vec<u8>,
}

/// 当前是不是 OS 主线程。
///
/// `file_icon_raster` 这类 AppKit 调用必须走主线程，否则 `on_main_thread` 会
/// `dispatch_sync` 回主队列——而 `cargo test` 里主队列不 drain 会死锁。渲染在真机的
/// 主线程上跑，这条返回 `true`、系统图标正常出；测试把渲染跑在子线程，返回 `false`，
/// 上层据此跳过平台调用、回退内置 SVG。非 macOS 没有主队列概念，恒为 `true`（那边
/// `file_icon_raster` 直接返 `None`，不会死锁）。
pub fn is_main_thread() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_main_thread()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// 标记「主线程的 run loop 已经跑起来」——真实应用启动时调一次。
///
/// 这条是给**后台任务**用的：它们不在主线程上，但可以把自己的 AppKit 调用
/// `dispatch_sync` 回主队列，前提是主线程真的在跑 run loop 并会去 drain 那个队列。
/// 应用在 `mo_ui::run()` 里（拿到 `NSApplication` 之后）标记；`cargo test` 永远不标，
/// 于是后台任务一律走 `appkit_usable() == false` 的保守分支，不会把测试挂死。
///
/// 幂等，且标记之后不会撤销。
pub fn mark_main_loop_ready() {
    #[cfg(target_os = "macos")]
    macos::mark_main_loop_ready();
}

/// 现在能不能安全地调 AppKit。
///
/// 「调用方就在主线程」或「主 run loop 已在跑（可以 `dispatch_sync` 回主队列）」
/// 任一成立即为真。**后台任务在动 AppKit 之前必须问这一条**——只问
/// [`is_main_thread`] 是不够的：后台线程永远答 `false`，会把该做的事也一并跳过。
pub fn appkit_usable() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::appkit_usable()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 已挂载的**本机**卷宗：外接磁盘、光驱、DMG、Time Machine 盘……
///
/// **网络盘不在里面**——它们归侧边栏的「网络」区（`mo_remote::mount::mounted_shares`），
/// 两边都列同一个盘只会让用户困惑。判据是文件系统类型（`statfs`），不是路径形状。
///
/// 每块盘还带一个 [`Volume::ejectable`]（能不能推出），UI 据此决定要不要画推出
/// 按钮——内置硬盘推不动，不该给。macOS 上这步要读卷宗属性（Foundation），
/// 所以本函数应当在主线程调用；非 macOS 一律 `false`。
///
/// 没实现的平台返回空。
pub fn volumes() -> Vec<Volume> {
    #[cfg(target_os = "macos")]
    {
        macos::volumes()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不支持的平台必须给 `Unsupported`，别让上层把「没实现」当成「做成了」。
    #[test]
    fn unsupported_platforms_say_so() {
        #[cfg(not(target_os = "macos"))]
        {
            assert!(matches!(
                reveal(Path::new("/tmp")),
                Err(PlatformError::Unsupported(_))
            ));
            assert!(matches!(
                eject(Path::new("/tmp")),
                Err(PlatformError::Unsupported(_))
            ));
            assert!(!supports_trash());
        }
        #[cfg(target_os = "macos")]
        {
            assert!(supports_trash());
        }
    }

    /// 「支不支持」与**菜单上的叫法**必须一致：macOS 叫访达，别的平台写了访达就是
    /// 错的（反过来更有害：平台没实现却把菜单项摆出来，点了只会报「不支持」）。
    #[test]
    fn reveal_label_matches_the_platform() {
        let here_is_macos = cfg!(target_os = "macos");
        assert_eq!(supports_reveal(), here_is_macos);
        assert_eq!(supports_trash(), here_is_macos);
        assert_eq!(supports_volumes(), here_is_macos);
        assert_eq!(supports_file_icons(), here_is_macos);
        if here_is_macos {
            assert_eq!(reveal_label(), "在访达中显示");
        } else {
            assert_eq!(reveal_label(), "在文件管理器中显示");
        }
    }

    /// 真把它送进**系统**废纸篓：原处必须消失。
    ///
    /// ⚠️ 这条走的是真实 AppKit——之所以敢放进单测，是因为回收站那条路用的是
    /// `NSFileManager`（同步、不要求主线程）。`reveal` / `eject` 不行：它们
    /// `dispatch_sync` 回主队列，而测试进程的主线程并不 drain 主队列，会直接挂死
    /// （见 `devlog/macos-platform.md` §9）。
    ///
    /// 副作用是把一个临时文件丢进用户自己的废纸篓，可接受。
    #[cfg(target_os = "macos")]
    #[test]
    fn recycling_takes_the_file_out_of_place() {
        let dir = std::env::temp_dir().join("mo-platform-recycle-test");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"x").expect("写临时文件");

        recycle(std::slice::from_ref(&victim)).expect("应当能进系统废纸篓");
        assert!(!victim.exists(), "进了废纸篓，原处就不该还在");

        // 第二次必须**报错**（而不是静默「成功」）——用户会以为又删了一份。
        let err = recycle(std::slice::from_ref(&victim)).expect_err("同一个文件第二次应当失败");
        assert!(
            err.to_string().contains("victim.txt"),
            "错误里要说清是哪一条失败：{err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
