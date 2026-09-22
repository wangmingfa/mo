//! 系统 shell 集成：「打开」与「打开方式」。
//!
//! 文件的**默认打开**走 shell 的 `open` 动词——与资源管理器双击完全同一条
//! 系统路径（含 UAC / 商店应用 / 默认浏览器重定向）。「打开方式」从注册表
//! 枚举该扩展名的候选 ProgID（`OpenWithProgids` + 默认 ProgID + 用户
//! `UserChoice`），执行时用 `ShellExecuteExW` 的 `SEE_MASK_CLASSNAME`
//! 指定 ProgID；「选择其他应用…」调系统的 `openas` 动词弹出系统选择对话框。
//!
//! macOS：默认打开用 `open`（它就是 LaunchServices 的命令行前端，行为与访达
//! 双击一致，且不需要主线程）；候选应用走 LaunchServices 的
//! `LSCopyApplicationURLsForURL`（系统才知道哪个 App 声明了能开这个 UTI）。
//! 「选择其他应用…」macOS **没有**系统对话框（Windows 的 `openas` 动词没有对应
//! 物），所以走 Mo 自己的应用选择器——数据源见 [`installed_apps`]。
//!
//! Linux 退化：默认打开 `xdg-open`，候选列表为空。
//!
//! ⚠️ unsafe 仅限本文件的 shell FFI 调用（见 workspace lints 的约定）。

/// 「打开方式」里的一个候选应用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenWithApp {
    /// 展示名称（友好名 / 可执行文件名 / App 名）。
    pub name: String,
    /// 打开时用的凭据：Windows 是注册表 ProgID，macOS 是 `.app` 的路径。
    pub progid: String,
}

/// Mo 自己的「打开方式」选择器的数据源：系统里装了哪些应用。
///
/// * macOS：扫 `/Applications`（含 Utilities）与 `~/Applications` 下的 `.app`；
/// * 其他平台：空——UI 据此退回系统的「打开方式」对话框（Windows 的 `openas`）。
///
/// 扫目录是**毫秒级**的，但仍然是 IO：调用方要放 blocking 线程。
pub fn installed_apps() -> Vec<OpenWithApp> {
    #[cfg(target_os = "macos")]
    {
        imp::installed_apps()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "windows")]
mod imp {
    #![allow(unsafe_code)]
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_CLASSNAME, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::OpenWithApp;

    /// UTF-16 + NUL 结尾（ShellExecute 系列要求）。
    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
    }

    /// 用系统默认应用打开（等价资源管理器双击）。
    pub fn open_default(path: &Path) -> Result<(), String> {
        let file = wide(path);
        let verb = wide("open");
        let ret = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                0, // hwnd：无需父窗口（不需要 UAC 弹窗归属）
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(), // 参数
                std::ptr::null(), // 工作目录
                SW_SHOWNORMAL,
            )
        };
        // ShellExecute 约定：返回值 > 32 为成功。
        if ret as usize > 32 {
            Ok(())
        } else {
            Err(format!("ShellExecute 失败（代码 {ret}）"))
        }
    }

    /// 用指定 ProgID 打开（「打开方式」选中某应用）。
    pub fn open_with_progid(path: &Path, progid: &str) -> Result<(), String> {
        let file = wide(path);
        let class = wide(progid);
        let verb = wide("open");
        // ⚠️ 宽字符串必须先绑定再调用：结构体只存指针，内联 wide(...).as_ptr()
        // 的临时值活不过表达式结束。
        let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_CLASSNAME;
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = file.as_ptr();
        sei.lpClass = class.as_ptr();
        sei.nShow = SW_SHOWNORMAL;
        let ok = unsafe { ShellExecuteExW(&mut sei) };
        if ok != 0 {
            Ok(())
        } else {
            Err(format!("用「{progid}」打开失败"))
        }
    }

    /// 弹出系统的「打开方式」选择对话框（`openas` 动词）。
    pub fn open_with_dialog(path: &Path) -> Result<(), String> {
        let file = wide(path);
        let verb = wide("openas");
        let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_INVOKEIDLIST;
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = file.as_ptr();
        sei.nShow = SW_SHOWNORMAL;
        let ok = unsafe { ShellExecuteExW(&mut sei) };
        if ok != 0 {
            Ok(())
        } else {
            Err("打开「打开方式」对话框失败".to_string())
        }
    }

    /// 枚举该文件「打开方式」的候选应用（读注册表，可能毫秒级，调用方放 blocking 线程）。
    pub fn open_with_candidates(path: &Path) -> Vec<OpenWithApp> {
        use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};
        use winreg::RegKey;

        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return Vec::new();
        };
        let ext = format!(".{}", ext.to_ascii_lowercase());

        let mut progids: Vec<String> = Vec::new();
        let mut push = |id: String| {
            if !id.is_empty() && !progids.contains(&id) {
                progids.push(id);
            }
        };

        // 1) 用户默认（UserChoice 优先级最高，排最前）。
        let file_exts = format!(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{}",
            ext
        );
        if let Ok(k) =
            RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!(r"{file_exts}\UserChoice"))
        {
            if let Ok(id) = k.get_value::<String, _>("ProgId") {
                push(id);
            }
        }
        // 2) 扩展名的默认 ProgID。
        // 3) `OpenWithProgids` 的值名列表（系统「打开方式」菜单同源）。
        if let Ok(k) = RegKey::predef(HKEY_CLASSES_ROOT).open_subkey(&ext) {
            if let Ok(id) = k.get_value::<String, _>("") {
                push(id);
            }
            if let Ok(owp) = k.open_subkey("OpenWithProgids") {
                for (name, _) in owp.enum_values().flatten() {
                    push(name);
                }
            }
        }

        progids
            .into_iter()
            .filter_map(|progid| {
                let name = display_name(&progid)?;
                Some(OpenWithApp { name, progid })
            })
            .collect()
    }

    /// ProgID 的展示名：优先 shell\open\command 里的可执行文件名
    /// （FriendlyTypeName 的间接字符串 `@…,-<id>` 需要资源解析，退化处理）。
    fn display_name(progid: &str) -> Option<String> {
        use winreg::enums::HKEY_CLASSES_ROOT;
        use winreg::RegKey;

        let k = RegKey::predef(HKEY_CLASSES_ROOT).open_subkey(progid).ok()?;
        if let Ok(cmd) = k
            .open_subkey(r"shell\open\command")
            .and_then(|c| c.get_value::<String, _>(""))
        {
            if let Some(name) = exe_display(&cmd) {
                return Some(name);
            }
        }
        // 兜底：ProgID 本身（如 "Applications\notepad.exe" → notepad）。
        let tail = progid.rsplit('\\').next().unwrap_or(progid);
        (!tail.is_empty()).then(|| tail.to_string())
    }

    /// 从命令行串提取可执行文件名：`"C:\…\app.exe" "%1"` → `app`。
    pub(super) fn exe_display(command: &str) -> Option<String> {
        let exe = if command.starts_with('"') {
            command.split('"').nth(1)?
        } else {
            command.split_whitespace().next()?
        };
        Path::new(exe)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
    }
}

#[cfg(target_os = "windows")]
pub use imp::{open_default, open_with_candidates, open_with_dialog, open_with_progid};

#[cfg(target_os = "macos")]
mod imp {
    #![allow(unsafe_code)]
    use std::collections::HashSet;
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::OpenWithApp;

    // ---- LaunchServices：系统知道「哪些 App 能打开这个文件」 ----
    //
    // ⚠️ CoreFoundation / CoreServices 必须显式链接，否则符号找不到（与
    // `devlog/macos-platform.md` §7 同一个坑：框架是懒加载的，没人 link 就没人
    // 把它们载进进程）。这里是 C API，没有 ObjC 消息发送可用。

    /// `kLSRolesAll`：不限角色（Editor / Viewer / Shell 都算）。
    const LS_ROLES_ALL: u32 = 0xFFFF_FFFF;
    /// `kCFStringEncodingUTF8`。
    const ENCODING_UTF8: u32 = 0x0800_0100;
    /// `kCFURLPOSIXPathStyle`。
    const POSIX_PATH: i32 = 0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        #[link_name = "CFStringCreateWithFileSystemRepresentation"]
        fn cf_string_new(alloc: *const c_void, path: *const c_char) -> *const c_void;
        #[link_name = "CFURLCreateWithFileSystemPath"]
        fn cf_url_new(
            alloc: *const c_void,
            path: *const c_void,
            path_style: i32,
            is_directory: u8,
        ) -> *const c_void;
        #[link_name = "CFURLCopyFileSystemPath"]
        fn cf_url_path(url: *const c_void, path_style: i32) -> *const c_void;
        #[link_name = "CFArrayGetCount"]
        fn cf_array_len(array: *const c_void) -> isize;
        #[link_name = "CFArrayGetValueAtIndex"]
        fn cf_array_at(array: *const c_void, idx: isize) -> *const c_void;
        #[link_name = "CFStringGetLength"]
        fn cf_string_len(s: *const c_void) -> isize;
        #[link_name = "CFStringGetCString"]
        fn cf_string_copy(
            s: *const c_void,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        #[link_name = "CFRelease"]
        fn cf_release(cf: *const c_void);
    }

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        /// 返回 `CFArrayRef`（元素是 `CFURLRef`），Create Rule——调用方 CFRelease。
        #[link_name = "LSCopyApplicationURLsForURL"]
        fn ls_apps_for_url(url: *const c_void, role: u32) -> *const c_void;
    }

    /// 路径 → `CFURLRef`（Create Rule）。
    unsafe fn cf_url(path: &Path) -> Option<*const c_void> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let s = cf_string_new(std::ptr::null(), c_path.as_ptr());
        if s.is_null() {
            return None;
        }
        let url = cf_url_new(std::ptr::null(), s, POSIX_PATH, u8::from(path.is_dir()));
        cf_release(s);
        (!url.is_null()).then_some(url)
    }

    /// `CFStringRef` → Rust 字符串（UTF-8）。
    unsafe fn cf_string_to_utf8(s: *const c_void) -> Option<String> {
        let len = cf_string_len(s);
        if len <= 0 {
            return None;
        }
        // UTF-8 下一个字符最多 4 字节，多留 1 字节给结尾的 NUL。
        let cap = len * 4 + 1;
        let mut buf = vec![0u8; cap as usize];
        let ok = cf_string_copy(s, buf.as_mut_ptr().cast::<c_char>(), cap, ENCODING_UTF8);
        if ok == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8(buf[..end].to_vec()).ok()
    }

    /// `CFURLRef` → POSIX 路径。
    unsafe fn cf_url_to_path(url: *const c_void) -> Option<String> {
        let s = cf_url_path(url, POSIX_PATH);
        if s.is_null() {
            return None;
        }
        let out = cf_string_to_utf8(s);
        cf_release(s);
        out
    }

    /// `open` 就是 LaunchServices 的命令行前端：与访达双击同一条路径，
    /// 且不需要主线程（`NSWorkspace` 那条要 `dispatch_sync` 回主队列）。
    pub fn open_default(path: &Path) -> Result<(), String> {
        Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// 用指定 App 打开（`progid` 在 macOS 上就是 `.app` 的路径）。
    pub fn open_with_progid(path: &Path, progid: &str) -> Result<(), String> {
        Command::new("open")
            .arg("-a")
            .arg(progid)
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// macOS **没有**系统「打开方式」对话框（Windows 的 `openas` 动词无对应物）。
    /// 这里如实报错，UI 走 Mo 自己的应用选择器（[`super::installed_apps`]）。
    pub fn open_with_dialog(_path: &Path) -> Result<(), String> {
        Err("macOS 没有系统的「打开方式」对话框".to_string())
    }

    /// 枚举能打开 `path` 的应用（LaunchServices，毫秒级但仍是 IO——调用方放 blocking）。
    pub fn open_with_candidates(path: &Path) -> Vec<OpenWithApp> {
        let Some(url) = (unsafe { cf_url(path) }) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        unsafe {
            let apps = ls_apps_for_url(url, LS_ROLES_ALL);
            if !apps.is_null() {
                for i in 0..cf_array_len(apps) {
                    let item = cf_array_at(apps, i);
                    if item.is_null() {
                        continue;
                    }
                    if let Some(p) = cf_url_to_path(item) {
                        out.push(OpenWithApp {
                            name: app_name(&p),
                            progid: p,
                        });
                    }
                }
                cf_release(apps);
            }
            cf_release(url);
        }
        out
    }

    /// `/Applications/Foo.app` → `Foo`（去掉 `.app`，找不到就用整段路径）。
    fn app_name(path: &str) -> String {
        Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string())
    }

    /// 系统里装了哪些 `.app`——Mo 自己的「打开方式」选择器用。
    ///
    /// 只扫几个固定目录的一层（访达的「其他…」也是这个量级）：
    /// `/Applications`、`/Applications/Utilities`、`/System/Applications`、
    /// `/System/Applications/Utilities`、`~/Applications`。
    pub fn installed_apps() -> Vec<OpenWithApp> {
        let mut roots = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/Applications/Utilities"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/System/Applications/Utilities"),
        ];
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join("Applications"));
        }

        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut out = Vec::new();
        for root in roots {
            let Ok(read) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in read.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("app") {
                    continue;
                }
                if !seen.insert(path.clone()) {
                    continue;
                }
                let name = app_name(&path.to_string_lossy());
                // 以点开头的不是给人用的（Helper / 隐藏 bundle），列出来只会碍事。
                if name.starts_with('.') {
                    continue;
                }
                out.push(OpenWithApp {
                    name,
                    progid: path.to_string_lossy().into_owned(),
                });
            }
        }
        // 中文 / 英文混排时按名字排序更可扫（大小写不敏感，与访达一致）。
        out.sort_by_key(|a| a.name.to_lowercase());
        out
    }
}

#[cfg(target_os = "macos")]
pub use imp::{open_default, open_with_candidates, open_with_dialog, open_with_progid};

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod imp {
    use std::path::Path;
    use std::process::Command;

    use super::OpenWithApp;

    pub fn open_default(path: &Path) -> Result<(), String> {
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn open_with_progid(path: &Path, progid: &str) -> Result<(), String> {
        let _ = progid;
        open_default(path)
    }

    pub fn open_with_dialog(path: &Path) -> Result<(), String> {
        open_default(path)
    }

    pub fn open_with_candidates(_path: &Path) -> Vec<OpenWithApp> {
        Vec::new()
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use imp::{open_default, open_with_candidates, open_with_dialog, open_with_progid};

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// 命令行串 → 可执行文件名（带引号 / 不带引号两种形态）。
    #[cfg(target_os = "windows")]
    #[test]
    fn exe_display_extracts_friendly_name() {
        use imp::exe_display;
        assert_eq!(
            exe_display(r#""C:\Program Files\Notepad++\notepad++.exe" "%1""#).as_deref(),
            Some("notepad++")
        );
        assert_eq!(
            exe_display(r"C:\Windows\system32\notepad.exe %1").as_deref(),
            Some("notepad")
        );
        assert_eq!(exe_display(""), None);
    }

    /// Windows 上至少能枚举出 txt 的候选（注册表必有记事本系）；
    /// Linux 候选列表恒为空；macOS 见下面两条。
    #[test]
    fn candidates_do_not_crash() {
        let _ = open_with_candidates(Path::new("C:/no-such-dir/x.txt"));
        #[cfg(target_os = "windows")]
        assert!(
            !open_with_candidates(Path::new("C:/no-such-dir/x.txt")).is_empty(),
            "txt 在常规 Windows 上应有候选应用"
        );
        #[cfg(target_os = "linux")]
        assert!(open_with_candidates(Path::new("/tmp/x.txt")).is_empty());
    }

    /// macOS：真建一个 `.txt`，LaunchServices 必须给出候选（至少文本编辑），
    /// 且每条的「凭据」都是 `.app` 的路径（`open -a` 只认这个）。
    #[cfg(target_os = "macos")]
    #[test]
    fn a_real_text_file_has_open_with_candidates() {
        let dir = std::env::temp_dir().join("mo-open-with-candidates");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("probe.txt");
        std::fs::write(&file, "hello").expect("写临时文件");

        let apps = open_with_candidates(&file);
        assert!(!apps.is_empty(), "txt 在 macOS 上一定有候选应用");
        for a in &apps {
            assert!(
                a.progid.ends_with(".app"),
                "候选凭据应是 .app 路径，实际是 {}",
                a.progid
            );
            assert!(!a.name.is_empty(), "候选不该没有名字");
        }

        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// macOS：Mo 自己的选择器要有数据（扫得出系统里的 App），且名字唯一可辨。
    #[cfg(target_os = "macos")]
    #[test]
    fn installed_apps_are_named_and_unique() {
        let apps = installed_apps();
        assert!(!apps.is_empty(), "系统里总该装了几个 App");
        let mut paths = std::collections::HashSet::new();
        for a in &apps {
            assert!(a.progid.ends_with(".app"));
            assert!(!a.name.is_empty());
            assert!(paths.insert(a.progid.clone()), "同一个 App 不该列两遍");
        }
    }

    /// macOS 没有系统「打开方式」对话框——这里不许假装成功
    ///（`open_with_dialog` 退回 `open_default` 会让那个菜单项变成「用默认应用打开」，
    /// 用户点了却没得选）。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_refuses_to_fake_a_system_dialog() {
        let err = open_with_dialog(Path::new("/tmp/whatever.txt"))
            .expect_err("macOS 没有系统对话框，必须如实报错");
        assert!(err.contains("macOS"));
    }

    /// 非 macOS 给不了应用列表（UI 据此退回系统对话框）。
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn installed_apps_are_empty_without_macos() {
        assert!(installed_apps().is_empty());
    }
}
