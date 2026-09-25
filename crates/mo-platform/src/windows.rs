#![allow(unsafe_code)]
//! Windows 原生集成：「在资源管理器中显示」+ 系统回收站（`IFileOperation`）。
//!
//! 回收站这条链路与 macOS 的契约完全一致：**系统搬文件、Mo 拿回落点记账**。
//! 但 Windows 的 `IFileOperation` 没有 macOS
//! `trashItemAtURL:resultingItemURL:` 那种「落点回执」，所以落点靠**反查**：
//! 每个回收条目在卷宗自己的 `<盘>:\$Recycle.Bin\<SID>\` 里是一对
//! `$R<尾名>`（文件本体）+ `$I<尾名>`（元数据，正文里记着原始完整路径），
//! 删除后扫 `$I`、比对记的原始路径，命中后把前缀换成 `$R` 即落点。
//!
//! ⚠️ unsafe 仅限本文件的 shell / COM FFI 调用（与 `mo_app::shell` 同一约定）。

use std::path::{Path, PathBuf};

use windows::core::{GUID, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    IFileOperation, IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName,
    FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
};

use crate::PlatformError;

/// `CLSID_FileOperation`（Windows SDK 里的固定值，crate 没导就自己钉一份）。
/// ⚠️ 别和 `IFileOperation` 的接口 IID（`947AAB5F-…`）混了——CoCreateInstance
/// 要的是 coclass 的 CLSID。
const CLSID_FILE_OPERATION: GUID = GUID::from_u128(0x3AD05575_8857_4850_9277_11B85BDB8E09);

/// 在资源管理器里选中 `path`（打开其所在目录并高亮该项）。
pub fn reveal(path: &Path) -> Result<(), PlatformError> {
    if !path.exists() {
        return Err(PlatformError::Failed(format!(
            "路径不存在，无法在资源管理器中显示：{}",
            path.display()
        )));
    }
    let target = path.display().to_string().replace('/', "\\");
    std::process::Command::new("explorer")
        .arg(format!("/select,{target}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| PlatformError::Failed(format!("explorer 起不来：{e}")))
}

/// 线程级 COM 初始化守卫。
///
/// tokio 的 blocking 线程会被复用：第二次进来 `CoInitializeEx` 返回
/// `S_FALSE`（线程早已是 COM 线程，这次调用不拥有初始化计数），**不能**再
/// 配对 `CoUninitialize`——否则把上一次的计数打穿。
struct ComGuard {
    owned: bool,
}

impl ComGuard {
    fn init() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        // `hr.is_ok()` 把 S_OK(0) 与 S_FALSE(1) 都算成功，只有 S_OK 算「我拥有的」。
        Self { owned: hr.0 == 0 }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

/// 把 `path` 送进**系统**回收站，返回它 `$R...` 落点的实际路径。
pub fn recycle_one(path: &Path) -> Result<PathBuf, PlatformError> {
    let full = canonical_full(path)?;
    let _com = ComGuard::init();

    let wide = encode_wide(&full);
    let item: IShellItem = unsafe {
        SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
            .map_err(|e| failed("解析条目", path, e))?
    };
    let op: IFileOperation = unsafe {
        CoCreateInstance(&CLSID_FILE_OPERATION, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| failed("创建系统回收站操作", path, e))?
    };
    unsafe {
        // 全程静默：不弹确认、不弹进度、不弹错误框；FOFX_EARLYFAILURE 让需要
        // 提权的删除**直接失败**而不是弹 UAC（GUI 应用里给出不明提权框）。
        // ⚠️ FOFX_RECYCLEONDELETE 必须**显式**给：实测少了它 PerformOperations
        // 返回 S_OK 但文件被**直接抹掉**（不进回收站）——文档里那句「默认回收」
        // 靠不住，这条是数据安全问题，不许省。
        op.SetOperationFlags(
            FOFX_RECYCLEONDELETE
                | FOF_SILENT
                | FOF_NOCONFIRMATION
                | FOF_NOERRORUI
                | FOFX_EARLYFAILURE,
        )
        .map_err(|e| failed("设置回收站选项", path, e))?;
        op.DeleteItem(&item, None::<&IFileOperationProgressSink>)
            .map_err(|e| failed("送进回收站", path, e))?;
        op.PerformOperations()
            .map_err(|e| failed("送进回收站", path, e))?;
        if op
            .GetAnyOperationsAborted()
            .map_err(|e| failed("送进回收站", path, e))?
            .as_bool()
        {
            return Err(PlatformError::Failed(format!(
                "送进回收站被中断：{}",
                path.display()
            )));
        }
    }
    find_recycled(&full, path)
}

/// 反查落点：删完立刻扫（`$I` 在 PerformOperations 返回前就写好了）。
///
/// 废纸篓可能有几千条旧条目，逐条读 `$I` 正文一遍就是十几秒——所以前几轮
/// **只碰 mtime 够新**的（`$I` 的 mtime 就是删除时刻，DirEntry 的元数据在
/// NTFS 上来自目录枚举流，不额外花 I/O）；仍查不到再放宽成全量扫，兜住
/// 时钟回拨这种边角。
fn find_recycled(full: &Path, original: &Path) -> Result<PathBuf, PlatformError> {
    let roots = recycle_roots(full);
    // 10 分钟富余：机器时钟有小偏差、批量删除各条差几秒，都不该被筛掉。
    let floor = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(600));
    for attempt in 0..8 {
        let fresh_only = attempt < 4;
        for root in &roots {
            if let Some(landing) =
                search_recycle_root(root, full, if fresh_only { floor } else { None })
            {
                return Ok(landing);
            }
        }
        if attempt + 1 < 8 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    Err(PlatformError::Failed(format!(
        "文件已进系统回收站，但没反查到落点，Mo 账本记不上这一条：{}",
        original.display()
    )))
}

/// `full` 所在**卷宗**的回收站目录（回收站每卷一份，外接盘在盘根下）。
fn recycle_roots(full: &Path) -> Vec<PathBuf> {
    // 卷根 = 最顶层祖先（`C:\`、`\\server\share\`）。
    full.ancestors()
        .last()
        .map(|root| vec![root.join("$Recycle.Bin")])
        .unwrap_or_default()
}

/// `mtime_floor`：只碰 mtime 不早于它的 `$I`（`None` = 全量扫，见 [`find_recycled`]）。
fn search_recycle_root(
    root: &Path,
    full: &Path,
    mtime_floor: Option<std::time::SystemTime>,
) -> Option<PathBuf> {
    for sid in std::fs::read_dir(root).ok()?.flatten() {
        let sid_dir = sid.path();
        // 只扫得动**自己**的 SID 目录，别人的没有权限，read_dir 自然失败跳过。
        let Ok(files) = std::fs::read_dir(&sid_dir) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let Some(tail) = name.strip_prefix("$I") else {
                continue;
            };
            if let Some(floor) = mtime_floor {
                let stale = f
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t < floor)
                    .unwrap_or(false);
                if stale {
                    continue;
                }
            }
            let Ok(bytes) = std::fs::read(f.path()) else {
                continue;
            };
            if !original_matches(&bytes, full) {
                continue;
            }
            let landing = sid_dir.join(format!("$R{tail}"));
            if landing.symlink_metadata().is_ok() {
                return Some(landing);
            }
        }
    }
    None
}

/// `$I` 正文从偏移 28 起是一段 UTF-16LE、NUL 结尾的**原始完整路径**
/// （v1/v2 头部字段数不同，这个起点一样；后面的 NT SID 等杂项切在第一个
/// NUL 外）。比对大小写不敏感——NTFS 本来就不区分。
fn original_matches(bytes: &[u8], full: &Path) -> bool {
    let Some(origin) = bytes.get(28..).and_then(|rest| {
        let units: Vec<u16> = rest
            .chunks(2)
            .filter_map(|c| <[u8; 2]>::try_from(c).ok())
            .map(u16::from_le_bytes)
            .collect();
        String::from_utf16_lossy(&units)
            .split('\0')
            .find(|s| !s.is_empty())
            .map(str::to_owned)
    }) else {
        return false;
    };
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_lowercase();
    norm(&origin) == norm(&full.to_string_lossy())
}

/// `\\?\` 前缀形态归一成普通绝对路径：`$I` 里记的是普通形态，带前缀比对不上。
fn canonical_full(path: &Path) -> Result<PathBuf, PlatformError> {
    let canon = std::fs::canonicalize(path)
        .map_err(|e| PlatformError::Failed(format!("{}：{e}", path.display())))?;
    let s = canon.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return Ok(PathBuf::from(format!(r"\\{rest}")));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return Ok(PathBuf::from(rest));
    }
    Ok(canon)
}

fn encode_wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 错误里必须带上**是哪条路径**（用户有多选删除，分不清位置的报错等于没说）。
fn failed(action: &str, path: &Path, e: windows::core::Error) -> PlatformError {
    PlatformError::Failed(format!("{action}失败：{}（{e}）", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$I` 正文（定长头 + UTF-16 路径 + NUL + 尾部变长杂项）能被反查出来。
    #[test]
    fn parses_recycle_metadata_origin() {
        let origin = "C:\\Users\\demo\\Desktop\\a.txt";
        let mut bytes = vec![0u8; 28];
        bytes[0..4].copy_from_slice(&1u32.to_le_bytes()); // version
        bytes[24..28].copy_from_slice(&(origin.encode_utf16().count() as u32).to_le_bytes());
        let body: Vec<u8> = origin
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .chain([0, 0])
            .collect();
        bytes.extend_from_slice(&body);
        // v1 尾部还有 NT SID 等变长段，塞一串垃圾模拟。
        bytes.extend_from_slice(&[7, 8, 9, 0, 0]);

        assert!(original_matches(&bytes, Path::new(origin)));
        // NTFS 本来就不区分大小写，比对必须跟着不敏感。
        assert!(original_matches(
            &bytes,
            Path::new("c:\\USERS\\demo\\desktop\\A.TXT")
        ));
        assert!(!original_matches(
            &bytes,
            Path::new("C:\\Users\\demo\\Desktop\\b.txt")
        ));
    }

    /// 卷根上挂 `$Recycle.Bin`（回收站每卷一份，不在子目录里）。
    #[test]
    fn recycle_roots_points_at_the_volume_root() {
        let roots = recycle_roots(Path::new("C:\\Users\\demo\\a.txt"));
        assert!(roots
            .iter()
            .any(|r| r.to_string_lossy().eq_ignore_ascii_case("C:\\$Recycle.Bin")));
    }
}
