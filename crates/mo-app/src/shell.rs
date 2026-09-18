//! 系统 shell 集成：「打开」与「打开方式」。
//!
//! 文件的**默认打开**走 shell 的 `open` 动词——与资源管理器双击完全同一条
//! 系统路径（含 UAC / 商店应用 / 默认浏览器重定向）。「打开方式」从注册表
//! 枚举该扩展名的候选 ProgID（`OpenWithProgids` + 默认 ProgID + 用户
//! `UserChoice`），执行时用 `ShellExecuteExW` 的 `SEE_MASK_CLASSNAME`
//! 指定 ProgID；「选择其他应用…」调系统的 `openas` 动词弹出系统选择对话框。
//!
//! 非 Windows 退化：默认打开用 `open`（macOS）/ `xdg-open`（Linux）命令，
//! 候选列表为空（菜单里只剩「选择其他应用…」时该入口也隐藏）。
//!
//! ⚠️ unsafe 仅限本文件的 shell FFI 调用（见 workspace lints 的约定）。

/// 「打开方式」里的一个候选应用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenWithApp {
    /// 展示名称（友好名 / 可执行文件名 / ProgID 兜底）。
    pub name: String,
    /// 注册表 ProgID，执行时用。
    pub progid: String,
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

#[cfg(not(target_os = "windows"))]
mod imp {
    use std::path::Path;
    use std::process::Command;

    use super::OpenWithApp;

    pub fn open_default(path: &Path) -> Result<(), String> {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        Command::new(program)
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

#[cfg(not(target_os = "windows"))]
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
    /// 非 Windows 候选列表恒为空。
    #[test]
    fn candidates_do_not_crash() {
        let apps = open_with_candidates(Path::new("C:/no-such-dir/x.txt"));
        #[cfg(target_os = "windows")]
        assert!(!apps.is_empty(), "txt 在常规 Windows 上应有候选应用");
        #[cfg(not(target_os = "windows"))]
        assert!(apps.is_empty());
    }
}
