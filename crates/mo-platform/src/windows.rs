#![allow(unsafe_code)]
//! Windows 原生集成：「在资源管理器中显示」+ 系统回收站（`IFileOperation`）
//! + 卷宗列表与推出（`GetLogicalDrives` / `CM_Request_Device_Eject`）
//! + 系统图标（`SHGetFileInfoW` / `SHDefExtractIconW` → `HICON` → 像素）
//! + PDF 首页渲染（WinRT `Windows.Data.Pdf`）
//! + 文件剪贴板（写 `CF_HDROP`、读 `Preferred DropEffect`）。
//!
//! 回收站这条链路与 macOS 的契约完全一致：**系统搬文件、Mo 拿回落点记账**。
//! 但 Windows 的 `IFileOperation` 没有 macOS
//! `trashItemAtURL:resultingItemURL:` 那种「落点回执」，所以落点靠**反查**：
//! 每个回收条目在卷宗自己的 `<盘>:\$Recycle.Bin\<SID>\` 里是一对
//! `$R<尾名>`（文件本体）+ `$I<尾名>`（元数据，正文里记着原始完整路径），
//! 删除后扫 `$I`、比对记的原始路径，命中后把前缀换成 `$R` 即落点。
//!
//! 卷宗那条链路同理「照系统的规矩来」：列盘用 `GetLogicalDrives` +
//! `GetDriveTypeW`，**推出**走 SetupAPI 把「盘符 → 磁盘设备节点」对上，再
//! `CM_Request_Device_Eject`——与任务栏「安全删除硬件」同一动作，而不是自己
//! `net use /delete`（那只对网络映射盘有意义）。
//!
//! 图标这条链路是「问 shell 图标资源在哪 → 按目标尺寸抽一张 → 画进 GDI 位图取
//! 像素」。GDI 的 DC 不带 alpha，所以画黑、白两遍反解 alpha（见
//! [`premultiplied_from_backdrops`]）——交出去的仍是**预乘** RGBA，与 macOS 那份
//! 同一契约。
//!
//! PDF 这条走系统自带的 WinRT `Windows.Data.Pdf`（Windows 10 起就在机器上，不必随
//! 二进制带一个 pdfium.dll）。渲染输出是 BGRA8 预乘、页面底色透明，所以要换通道并
//! 铺一层白纸（见 [`opaque_rgba_over_white`]）——交出去的还是与 macOS 同一份契约。
//!
//! ⚠️ unsafe 仅限本文件的 shell / COM / 设备管理 / GDI / WinRT 初始化 FFI 调用（与 `mo_app::shell` 同一约定）。

use std::path::{Path, PathBuf};

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Request_Device_EjectW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    HDEVINFO, PNP_VETO_TYPE, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SP_DEVINFO_DATA,
};
// `GlobalFree` 在 windows 0.58 里挂在 `Foundation` 而不是 `Memory` 下（同族的
// `GlobalAlloc` / `GlobalLock` 却在 `Memory`），按 crate 的模块走而不是按头文件走。
use windows::Win32::Foundation::{CloseHandle, GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_MODE, OPEN_EXISTING,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::Ioctl::{
    GUID_DEVINTERFACE_DISK, IOCTL_STORAGE_EJECT_MEDIA, IOCTL_STORAGE_GET_DEVICE_NUMBER,
    STORAGE_DEVICE_NUMBER,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, MapVirtualKeyExW, VkKeyScanExW, MAPVK_VK_TO_CHAR,
};
use windows::Win32::UI::Shell::{
    IFileOperation, IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName,
    SHDefExtractIconW, SHGetFileInfoW, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FOF_NOCONFIRMATION,
    FOF_NOERRORUI, FOF_SILENT, SHFILEINFOW, SHGFI_FLAGS, SHGFI_ICON, SHGFI_ICONLOCATION,
    SHGFI_USEFILEATTRIBUTES,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL, HICON};

use crate::{IconRaster, PlatformError, Volume};

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

// ---- 卷宗：枚举 + 推出 ----

/// `GetDriveTypeW` 的返回值（WinSDK 里是裸宏，`windows` crate 没包成枚举）。
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

/// 本机已挂载的卷宗（侧边栏「位置」区）。
///
/// 与 macOS 那份的分工完全一致：**网络盘不在里面**——Windows 上映射盘
/// （`DRIVE_REMOTE`）归侧边栏「网络」区，两边都列同一个盘只会让用户困惑。
///
/// ⚠️ 这条在**渲染线程**上跑（`AppState::volumes` 带 5s TTL 缓存），所以只挑便宜的
/// 问：`GetLogicalDrives` 一次拿全位图，`GetDriveTypeW` 纯查表。唯一可能慢的是
/// `GetVolumeInformationW`——空光驱 / 空卡槽上它可以阻塞几秒等设备就绪，所以
/// **光盘驱动器不查卷标**（宁可少个名字，也不能让界面每 5 秒卡一下）。
pub fn volumes() -> Vec<Volume> {
    // 位图第 0 位是 A:，依次往上。
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for bit in 0..26u32 {
        if mask & (1 << bit) == 0 {
            continue;
        }
        let letter = (b'A' + bit as u8) as char;
        let root = format!("{letter}:\\");
        let wide = wide_str(&root);
        let kind = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
        // `DRIVE_NO_ROOT_DIR`：位图说它在、问的时候其实已经拔了（枚举与查询之间
        // 被人拔了 U 盘）——跳过，别画一行点不开的条目。
        if matches!(kind, DRIVE_REMOTE | DRIVE_NO_ROOT_DIR) {
            continue;
        }
        let label = if kind == DRIVE_CDROM {
            None
        } else {
            volume_label(&root)
        };
        out.push(Volume {
            name: volume_name(letter, kind, label.as_deref()),
            path: PathBuf::from(root),
            ejectable: is_ejectable(kind),
        });
    }
    out
}

/// 卷标（没起名返回 `None`）。
fn volume_label(root: &str) -> Option<String> {
    let mut buf = [0u16; 64];
    let wide = wide_str(root);
    unsafe {
        GetVolumeInformationW(
            PCWSTR(wide.as_ptr()),
            Some(&mut buf),
            None,
            None,
            None,
            None,
        )
        .ok()
    }?;
    let label = utf16_to_string(&buf);
    // 空卷标 = 这块盘没起名，交给 [`volume_name`] 按类型给个称呼。
    (!label.is_empty()).then_some(label)
}

/// 展示名与资源管理器同形：有卷标是 `卷标 (C:)`，没有是 `本地磁盘 (C:)`。
fn volume_name(letter: char, kind: u32, label: Option<&str>) -> String {
    let generic = match kind {
        DRIVE_REMOVABLE => "可移动磁盘",
        DRIVE_CDROM => "光盘",
        DRIVE_RAMDISK => "内存磁盘",
        _ => "本地磁盘",
    };
    format!("{} ({letter}:)", label.unwrap_or(generic))
}

/// 这块盘给不给「推出」按钮。
///
/// 只有可移动介质与光盘推得动：内置硬盘（`DRIVE_FIXED`）系统本来就不给推出，
/// 摆一个按钮下去只会换来一句「推出失败」——与 macOS 那份同一判据（那边读
/// `NSURLVolumeIsEjectableKey` 等四个属性，这边 `GetDriveTypeW` 一个就够）。
fn is_ejectable(kind: u32) -> bool {
    matches!(kind, DRIVE_REMOVABLE | DRIVE_CDROM)
}

/// 推出一个卷宗（侧边栏「位置」区那个按钮）。
///
/// 三条路按代价从低到高试：
///
/// 1. **光盘**先给 `IOCTL_STORAGE_EJECT_MEDIA`——这是真正的「弹托盘」，物理动作；
/// 2. 通用的走 `CM_Request_Device_Eject`：把盘符对到**磁盘设备节点**上再请求弹出，
///    与任务栏「安全删除硬件」同一动作（会先卸掉该盘所有卷、把设备停掉）。
///    这条路要 `IOCTL_STORAGE_GET_DEVICE_NUMBER` 问出磁盘编号，再去 SetupAPI 的
///    磁盘设备接口里找编号相同的那个节点；
/// 3. 都不是盘符（Mo 自己的挂载点之类）→ [`PlatformError::Unsupported`]，
///    让上层退回它的 `net use /delete`（对网络映射盘那才是正解）。
///
/// ⚠️ 全程**不需要管理员**：两处 `CreateFileW` 都以 0 访问权限打开句柄——
/// 用到的两个 IOCTL 都是 `FILE_ANY_ACCESS`，只为查询，不为读写内容。
pub fn eject(path: &Path) -> Result<(), PlatformError> {
    let Some(letter) = drive_letter(path) else {
        return Err(PlatformError::Unsupported("推出卷宗"));
    };
    let root = format!("{letter}:\\");
    let wide = wide_str(&root);
    let kind = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
    if kind == DRIVE_REMOTE {
        // 映射盘的「推出」= 断开连接，那是 `net use /delete` 的活（上层兜底）。
        return Err(PlatformError::Unsupported("推出网络映射盘"));
    }
    if kind == DRIVE_FIXED {
        // 内置固定盘：`is_ejectable` 已过滤，侧栏根本不给它摆按钮。真被调到了就给
        // `Failed` 而非 `Unsupported`——`Unsupported` 会让上层去跑 `net use /delete`，
        // 那对内置盘是错的动作。
        return Err(PlatformError::Failed(format!(
            "内置磁盘不能推出：{}",
            path.display()
        )));
    }

    // 光盘先试 `IOCTL_STORAGE_EJECT_MEDIA`（那是真正的「弹托盘」，一个物理动作）；
    // 弹不动的（USB 光盒一类）再和别的盘一样走设备节点那条。
    if kind == DRIVE_CDROM && eject_media(letter).is_ok() {
        return Ok(());
    }
    let device = volume_device_number(letter);
    match device {
        Some(number) => eject_device(number, path),
        None if kind == DRIVE_CDROM => Err(PlatformError::Failed(format!(
            "弹出光盘失败：{}",
            path.display()
        ))),
        None => Err(PlatformError::Failed(format!(
            "没能把这块盘对到系统设备上，无法推出：{}",
            path.display()
        ))),
    }
}

/// `E:\` / `E:/` / `E:` → `'E'`（大写）；不是盘符根一律 `None`。
///
/// 只认**卷根**：侧栏推出按钮给的就是卷根，认成一个 `E:\dir` 反而会让人以为
/// 能推出一个子目录。
fn drive_letter(path: &Path) -> Option<char> {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() == 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Some(bytes[0].to_ascii_uppercase() as char);
    }
    let head = s.strip_suffix(['\\', '/'])?;
    let bytes = head.as_bytes();
    if bytes.len() == 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        Some(bytes[0].to_ascii_uppercase() as char)
    } else {
        None
    }
}

/// 以 0 访问权限打开一个设备/卷句柄，只够发 `FILE_ANY_ACCESS` 的查询 IOCTL。
fn open_query_handle(target: &str) -> Option<HANDLE> {
    let wide = wide_str(target);
    unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_MODE(0x00000001 | 0x00000002), // FILE_SHARE_READ | FILE_SHARE_WRITE
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .ok()
    }
}

/// 问出这块卷宗所在**磁盘**的编号（`\\.\E:` → `STORAGE_DEVICE_NUMBER`）。
///
/// `PartitionNumber` 这里没用：推出是**整块磁盘**的动作（一次带走它所有分区），
/// 与资源管理器「安全删除 USB 驱动器」一致。
fn volume_device_number(letter: char) -> Option<STORAGE_DEVICE_NUMBER> {
    query_device_number(&format!("\\\\.\\{letter}:"))
}

/// 对一个设备/卷句柄发 `IOCTL_STORAGE_GET_DEVICE_NUMBER`。
fn query_device_number(target: &str) -> Option<STORAGE_DEVICE_NUMBER> {
    let handle = open_query_handle(target)?;
    let mut number = STORAGE_DEVICE_NUMBER::default();
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut number as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&mut returned),
            None,
        )
        .is_ok()
    };
    // 关掉查询句柄，成败都不影响上面的结论。
    let _ = unsafe { CloseHandle(handle) };
    (ok && returned >= std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32).then_some(number)
}

/// 光盘：`IOCTL_STORAGE_EJECT_MEDIA`（真的把托盘弹出来）。
fn eject_media(letter: char) -> Result<(), PlatformError> {
    let target = format!("\\\\.\\{letter}:");
    let wide = wide_str(&target);
    // 弹出是**动作**不是查询，这条要读写权限（拿不到就是没媒体 / 没权限）。
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0xC000_0000, // GENERIC_READ | GENERIC_WRITE
            FILE_SHARE_MODE(0x00000001 | 0x00000002),
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map_err(|e| failed("打开光驱", Path::new(&target), e))?
    };
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_EJECT_MEDIA,
            None,
            0,
            None,
            0,
            None,
            None,
        )
        .is_ok()
    };
    let _ = unsafe { CloseHandle(handle) };
    if ok {
        Ok(())
    } else {
        Err(PlatformError::Failed(format!("弹出光盘失败：{target}")))
    }
}

/// 把「磁盘编号」对到 SetupAPI 的设备节点上，请求弹出该设备。
///
/// 磁盘设备接口（`GUID_DEVINTERFACE_DISK`）每个对应一块物理磁盘，接口详情里同时
/// 给了**设备节点句柄**（`SP_DEVINFO_DATA.DevInst`）与**路径**：先开路径问编号，
/// 编号对上就拿那个 `DevInst` 去请求弹出。
fn eject_device(number: STORAGE_DEVICE_NUMBER, path: &Path) -> Result<(), PlatformError> {
    let set = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_DISK),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
        .map_err(|e| failed("枚举磁盘设备", path, e))?
    };
    let found = find_disk_devinst(set, number.DeviceNumber);
    // 设备接口集合用完必须还，先关掉再判成不成。
    let _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
    match found {
        Some(dev_inst) => request_eject(dev_inst, path),
        None => Err(PlatformError::Failed(format!(
            "推出失败：系统里找不到这块盘对应的设备（也许已经拔掉了）：{}",
            path.display()
        ))),
    }
}

/// 在设备接口集合里找编号相同的那块盘，返回它的设备节点。
fn find_disk_devinst(set: HDEVINFO, want: u32) -> Option<u32> {
    let mut iface = SP_DEVICE_INTERFACE_DATA {
        cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
        ..Default::default()
    };
    for member in 0.. {
        if unsafe {
            SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVINTERFACE_DISK, member, &mut iface)
        }
        .is_err()
        {
            return None; // 枚举到头了
        }
        let Some((dev_inst, interface_path)) = interface_detail(set, &mut iface) else {
            continue;
        };
        // 接口路径形如 `\\?\IDE#Disk...`，开它问一次编号即可判定是不是这块盘的磁盘。
        if query_device_number(&interface_path).is_some_and(|n| n.DeviceNumber == want) {
            return Some(dev_inst);
        }
    }
    None
}

/// 单个设备接口的 `(设备节点, 路径)`。
///
/// `SetupDiGetDeviceInterfaceDetailW` 是「先问要多大、再给缓冲区」的两段式，而
/// 它的头部结构 `SP_DEVICE_INTERFACE_DETAIL_DATA_W` 只声明了一个 `u16` 的路径数组
/// ——真实路径长在它后面，所以按 `required` 分配字节缓冲、把结构摆在开头。
fn interface_detail(set: HDEVINFO, iface: &mut SP_DEVICE_INTERFACE_DATA) -> Option<(u32, String)> {
    let mut required = 0u32;
    unsafe {
        // 第一次调用**必然**失败（缓冲区不够），要的就是那个 `required`。
        let _ = SetupDiGetDeviceInterfaceDetailW(set, iface, None, 0, Some(&mut required), None);
        if required == 0 {
            return None;
        }
        let mut buf = vec![0u8; required as usize];
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        let mut info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        SetupDiGetDeviceInterfaceDetailW(
            set,
            iface,
            Some(detail),
            required,
            Some(&mut required),
            Some(&mut info),
        )
        .ok()?;
        // 路径从结构的 `DevicePath` 起、NUL 结尾；上界取整个缓冲区，越界读不到东西。
        let limit = required as usize / 2;
        let units = std::slice::from_raw_parts((*detail).DevicePath.as_ptr(), limit);
        Some((info.DevInst, utf16_to_string(units)))
    }
}

/// `CM_Request_Device_EjectW`：真正请求系统弹出设备，带回被谁否决。
///
/// 失败很常见也很有用：用户开着 U 盘里的一个文件、杀软正在扫它，系统就会**否决**
/// 弹出并把「是谁拦的」带回来。这条必须原样递给用户，不然只看到「推出失败」四个字。
fn request_eject(dev_inst: u32, path: &Path) -> Result<(), PlatformError> {
    let mut veto_type = PNP_VETO_TYPE(0);
    // 否决者名字（正被占用的进程 / 设备名），256 个字符够写清是谁拦的。
    let mut veto_name = [0u16; 256];
    let ret = unsafe {
        CM_Request_Device_EjectW(dev_inst, Some(&mut veto_type), Some(&mut veto_name), 0)
    };
    if ret.0 == 0 {
        return Ok(()); // `CR_SUCCESS`
    }
    let who = utf16_to_string(&veto_name);
    // 名字与理由重复时（系统常把原因塞进名字里）只留一个，别写两遍。
    let reason = veto_text(veto_type.0);
    let tail = if who.is_empty() || reason.contains(&who) {
        format!("（{reason}）")
    } else {
        format!("（{reason}：{who}）")
    };
    Err(PlatformError::Failed(format!(
        "推出失败：{}{tail}［系统码 {:#010x}］",
        path.display(),
        ret.0
    )))
}

/// `PNP_VETO_TYPE` 的人话版（数值对照 WinSDK `winerror.h`）。
///
/// 只翻译真正常见的几种，其余给原文编号——猜错理由比不给理由更糟。
fn veto_text(veto: i32) -> &'static str {
    match veto {
        0 => "原因未知",
        2 => "还有程序开着它的文件",
        3 => "有程序正在使用它",
        4 => "有系统服务正在使用它",
        5 => "有句柄没关闭",
        6 => "设备自己拒绝",
        7 => "驱动不支持弹出",
        10 => "这个设备不允许停用",
        12 => "权限不足",
        _ => "系统拒绝停用该设备",
    }
}

/// 从一段 UTF-16 读到第一个 NUL 为止。
///
/// 必须吃**切片**而不是裸指针：`SP_DEVICE_INTERFACE_DETAIL_DATA_W` 声明的路径数组
/// 只有一个元素，按 `PCWSTR` 读到 NUL 会把「缓冲区外」当成合法内存。长度由调用方
/// 按缓冲区算好传进来。
fn utf16_to_string(units: &[u16]) -> String {
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

// ---- 系统图标：向 shell 要一张真图标 ----

/// `FILE_ATTRIBUTE_*`：`SHGetFileInfoW` 按它决定「这一条按哪类文件给图标」。
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;

/// 取 `path` 在系统里的图标，重绘到 `px` 见方后交出**预乘** RGBA 像素。
///
/// 契约与 macOS 那份完全一致（像素交出去、PNG 编码归调用方）。差别是这边**不挑
/// 线程**：`SHGetFileInfoW` 不是 UI 框架，在 blocking 池里直接问就行，不需要像
/// AppKit 那样 `dispatch_sync` 回主队列。
pub fn file_icon_raster(path: &Path, px: u32) -> Option<IconRaster> {
    icon_raster(path, px, FILE_ATTRIBUTE_NORMAL, false)
}

/// 取一个**扩展名**在系统里的图标（不碰磁盘，见 [`icon_raster`] 的 `name_only`）。
///
/// 给「文件本体已不在、扩展名还在」的条目用——回收站的原路径是典型：拿不存在的
/// 路径去问只会得到一张通用白纸图标，还会把整个类型的共享缓存污染掉。
pub fn ext_icon_raster(ext: &str, px: u32) -> Option<IconRaster> {
    let ext = ext.trim_start_matches('.');
    if ext.is_empty() {
        return None;
    }
    // 名字随便起，shell 只看后缀（`SHGFI_USEFILEATTRIBUTES` 下它压根不去碰磁盘）。
    let name = format!("mo.{ext}");
    icon_raster(Path::new(&name), px, FILE_ATTRIBUTE_NORMAL, true)
}

/// 取**通用文件夹**图标。走 shell 的类型解析而不是某个具体目录：不碰任何真实路径，
/// 也就不会因为「那个目录不存在 / 没权限」而拿不到占位图。
pub fn folder_icon_raster(px: u32) -> Option<IconRaster> {
    icon_raster(Path::new("mo"), px, FILE_ATTRIBUTE_DIRECTORY, true)
}

/// 两段式问图标，为的是**别拿到糊的**：
///
/// 1. `SHGFI_ICONLOCATION` 先问出「这个图标躺在哪个文件的第几个资源上」，再拿
///    `SHDefExtractIconW(.., px)` 按**目标尺寸**要一张真图——shell 手上有 256px 的
///    真彩图标，只有走这条路才拿得到；
/// 2. 这条路没成（类型没注册 `DefaultIcon` 一类）才退回 `SHGFI_ICON`，那是 shell
///    直接给的 32px 小图标，放大到 96px 的画廊槽位会糊——但糊的胜过没有。
///
/// ⚠️ 两条路给的 `HICON` 都是「调用者负责销毁」，漏一次就是每屏几十张的泄漏。
fn icon_raster(target: &Path, px: u32, attrs: u32, name_only: bool) -> Option<IconRaster> {
    let _com = ComGuard::init();
    let wide = encode_wide(target);
    let attrs = FILE_FLAGS_AND_ATTRIBUTES(attrs);
    // 「只看名字」要多给一位 `SHGFI_USEFILEATTRIBUTES`（否则 shell 会去碰磁盘）。
    let by_name = if name_only {
        SHGFI_USEFILEATTRIBUTES
    } else {
        SHGFI_FLAGS(0)
    };
    let hicon = located_icon(&wide, attrs, by_name | SHGFI_ICONLOCATION, px)
        .or_else(|| shell_icon(&wide, attrs, by_name | SHGFI_ICON))?;
    let raster = unsafe { hicon_to_raster(hicon, px) };
    unsafe {
        let _ = DestroyIcon(hicon);
    }
    raster
}

/// 第一段：问出图标资源（文件 + 索引），再按 `px` 要一张真图。
fn located_icon(
    target_wide: &[u16],
    attrs: FILE_FLAGS_AND_ATTRIBUTES,
    flags: SHGFI_FLAGS,
    px: u32,
) -> Option<HICON> {
    let mut info = SHFILEINFOW::default();
    let asked = unsafe {
        SHGetFileInfoW(
            PCWSTR(target_wide.as_ptr()),
            attrs,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if asked == 0 {
        return None;
    }
    // `szDisplayName` 这个字段在 SDK 里与 `szPath` 是同一个 union——给了
    // `SHGFI_ICONLOCATION` 时它就是那个装着图标的文件路径。
    let src = utf16_to_string(&info.szDisplayName);
    if src.is_empty() {
        return None;
    }
    let src_wide = wide_str(&src);
    let mut hicon = HICON::default();
    // 256px 是 shell 图标的天花板，再大只会白要一张上采样图。
    let hr = unsafe {
        SHDefExtractIconW(
            PCWSTR(src_wide.as_ptr()),
            info.iIcon,
            0,
            Some(&mut hicon),
            None,
            px.clamp(1, 256),
        )
    };
    (hr.is_ok() && !hicon.is_invalid()).then_some(hicon)
}

/// 第二段：shell 直接给的图标句柄（系统大图标尺寸，一般 32px）。
fn shell_icon(
    target_wide: &[u16],
    attrs: FILE_FLAGS_AND_ATTRIBUTES,
    flags: SHGFI_FLAGS,
) -> Option<HICON> {
    let mut info = SHFILEINFOW::default();
    let asked = unsafe {
        SHGetFileInfoW(
            PCWSTR(target_wide.as_ptr()),
            attrs,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if asked != 0 && !info.hIcon.is_invalid() {
        Some(info.hIcon)
    } else {
        None
    }
}

/// 把一张 `HICON` 画进 32 位位图，交出 `px` 见方的**预乘** RGBA。
///
/// ⚠️ GDI 的设备上下文**没有 alpha 通道**（画进去的第 4 字节恒 0），直接读回来等于
/// 把图标的透明信息全丢了——半透明边缘会变成一圈黑边。所以画**两遍**：一遍黑底、
/// 一遍白底，从两张的差里解出 alpha（推导见 [`premultiplied_from_backdrops`]）。
///
/// 缩放交给 `DrawIconEx`：图标原生尺寸（256 / 48 / 32）与槽位要的 `px` 几乎不会
/// 正好对上，而 GDI 至少会做半色调。
unsafe fn hicon_to_raster(hicon: HICON, px: u32) -> Option<IconRaster> {
    let side = px.max(1) as usize;
    let len = side * side * 4;
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side as i32,
            // 负高 = 自上而下的行序：读出来的字节序和 RGBA 缓冲同向，不用翻行。
            biHeight: -(side as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let dc = CreateCompatibleDC(None);
    if dc.is_invalid() {
        return None;
    }
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let Ok(bmp) = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(dc);
        return None;
    };
    if bits.is_null() {
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(dc);
        return None;
    }
    let old = SelectObject(dc, bmp);

    // 底色 `fill` 铺满，再把图标画上去，取一份 BGRA 快照。
    let on_backdrop = |fill: u8| -> Option<Vec<u8>> {
        unsafe { std::ptr::write_bytes(bits as *mut u8, fill, len) };
        unsafe {
            DrawIconEx(
                dc,
                0,
                0,
                hicon,
                side as i32,
                side as i32,
                0,
                None,
                DI_NORMAL,
            )
            .ok()?
        };
        let mut buf = vec![0u8; len];
        unsafe { std::ptr::copy_nonoverlapping(bits as *const u8, buf.as_mut_ptr(), len) };
        Some(buf)
    };
    // 两遍都包在一层闭包里：中途失败也要先走到下面的收尾（DC 与位图必须还）。
    let taken = (|| {
        let black = on_backdrop(0x00)?;
        let white = on_backdrop(0xFF)?;
        Some((black, white))
    })();

    // 收尾：不管两遍画了几遍，DC 与位图都得还回去。
    SelectObject(dc, old);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(dc);

    let (black, white) = taken?;
    Some(IconRaster {
        width: side as u32,
        height: side as u32,
        rgba: premultiplied_from_backdrops(&black, &white),
    })
}

/// 由「黑底那一张」与「白底那一张」解出**预乘** RGBA（BGRA → RGBA 的换序也在这做）。
///
/// `source-over` 叠在不透明底色 `B` 上：`out = a·C + (1−a)·B`。
///
/// * `B = 0`（黑）：`black = a·C` —— 这一份**本身就是预乘值**，直接当要交的 RGB；
/// * `B = 255`（白）：`white = a·C + 255·(1−a) = black + 255 − 255a`
///   → `a = (black + 255 − white) / 255`。
///
/// 三条通道各算一份 alpha 再取平均：同一个像素本该只有一个 alpha，取平均把量化
/// 误差摊薄（单通道算会在半透明边缘留噪点）。
///
/// 顺带一提：这套推导**不要求 `DrawIconEx` 真按 alpha 混合**——它要是只认图标的
/// 1 位掩码，透明处两张分别还是 0 与 255，解出来的 alpha 照样是 0，只是边缘没了
/// 抗锯齿。
fn premultiplied_from_backdrops(black: &[u8], white: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; black.len()];
    for i in (0..black.len()).step_by(4) {
        // 32 位 BI_RGB 在内存里是小端 BGRA；第 4 字节是 GDI 写的垃圾，不看。
        let sum = (black[i] as i32 - white[i] as i32)
            + (black[i + 1] as i32 - white[i + 1] as i32)
            + (black[i + 2] as i32 - white[i + 2] as i32);
        let alpha = (255 + sum / 3).clamp(0, 255) as u8;
        out[i] = black[i + 2]; // R
        out[i + 1] = black[i + 1]; // G
        out[i + 2] = black[i]; // B
        out[i + 3] = alpha;
    }
    out
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

// ---- PDF：首页渲染（WinRT `Windows.Data.Pdf`）----

// 这一段用的是 0.62 那份 `windows`（别名 `windows_pdf`，理由见 Cargo.toml）：
// WinRT 的 `*Async` 在 0.62 里生成的是「返回 `IAsyncOperation` 的同步函数」，而
// `IAsyncOperation::join()` 就是**阻塞等完**——不需要 tokio、不需要手写完成回调，
// 正好合 blocking 池的胃口。
use windows_pdf::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows_pdf::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBufferAccessMode, BitmapDecoder, BitmapPixelFormat,
};
use windows_pdf::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows_pdf::Win32::System::WinRT::IMemoryBufferByteAccess;
// `IMemoryBufferReference::cast()` 挂在这个 trait 上，不进作用域就调不到。
use windows_pdf::core::Interface;

/// 把当前线程接进 WinRT 运行时（`Windows.Data.Pdf` 是 WinRT 类，没初始化调不动）。
///
/// 只**加**不**减**：`RoInitialize` 幂等（同一线程第二次返回 `S_FALSE`，计数不再涨），
/// 而这里分不清「这次是我加的还是别人早加过」——乱配 `RoUninitialize` 会把别人的
/// 计数打穿。blocking 池的线程活得跟进程一样久，留一层计数没有代价。
///
/// ⚠️ 必须是**多线程套间**（MTA）。STA 线程上 `join()` 等完成回调会互等成死锁：
/// 回调要排进本线程的消息泵，而本线程正卡在 `join()` 上。所以这里不能沿用
/// [`ComGuard`]（那是 STA）。
fn ensure_winrt() {
    use windows_pdf::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    // 返回 `Err` 只可能是这台机器的 WinRT 不可用，或线程已是 STA（RPC_E_CHANGED_MODE，
    // 此时运行时其实已经就绪，照常往下调用即可）——两种都不值得单独处理。
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
}

/// 渲染 PDF 第一页。失败一律 `None`（打不开 / 不是 PDF / 有密码 / 零页 / 尺寸不对）。
pub fn pdf_page_raster(path: &Path, max_edge: u32) -> Option<IconRaster> {
    if max_edge == 0 {
        return None;
    }
    // 整个文件先读进内存再喂流：WinRT 那边拿路径开文件要走 `StorageFile`，对
    // 长路径 / UNC / 云占位文件的脾气与 `std::fs` 不一致，而预览的 PDF 本来就得
    // 整份解析。渲染只花几十毫秒，读盘那点时间在总账里不是主角。
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    ensure_winrt();
    render_first_page(&bytes, max_edge)
}

/// `bytes` 是一份 PDF 正文，长边缩到 `max_edge` 以内渲染第一页（不放大）。
///
/// 全程 `Option`：WinRT 那边每个调用都带 `Result`，但上层（`AppState::preview_pdf_page`）
/// 只问「这张图出得来吗」， HRESULT 到了那里也得丢掉，所以中间不养一条错误链。
fn render_first_page(bytes: &[u8], max_edge: u32) -> Option<IconRaster> {
    let src = InMemoryRandomAccessStream::new().ok()?;
    let writer = DataWriter::CreateDataWriter(&src).ok()?;
    writer.WriteBytes(bytes).ok()?;
    writer.StoreAsync().ok()?.join().ok()?;
    src.Seek(0).ok()?;

    let doc = PdfDocument::LoadFromStreamAsync(&src).ok()?.join().ok()?;
    // 页码 0-based（CoreGraphics 那边是 1-based，别混）。加密文档到 `LoadFromStream`
    // 就已经报错了，走不到这里。
    let page = doc.GetPage(0).ok()?;
    // `Size` 已按页面旋转调整过（`Dimensions().MediaBox()` 是未旋转的纸面框，
    // 横竖页拿去算缩放会把长宽边弄反）。
    let size = page.Size().ok()?;
    let (pw, ph) = (f64::from(size.Width), f64::from(size.Height));
    if !pw.is_finite() || !ph.is_finite() || pw <= 0.0 || ph <= 0.0 {
        return None;
    }
    let scale = (f64::from(max_edge) / pw.max(ph)).min(1.0);
    let w = (pw * scale).round().max(1.0) as u32;
    let h = (ph * scale).round().max(1.0) as u32;

    let out = InMemoryRandomAccessStream::new().ok()?;
    let opts = PdfPageRenderOptions::new().ok()?;
    // 宽高都按同一比例给：WinRT 只在两者之间保持**源**宽高比，给准了就不带形变。
    opts.SetDestinationWidth(w).ok()?;
    opts.SetDestinationHeight(h).ok()?;
    // 系统开了高对比度时不照做——深色模式反色出来的「白纸黑字」在预览窗里是张底片。
    opts.SetIsIgnoringHighContrast(true).ok()?;
    page.RenderWithOptionsToStreamAsync(&out, &opts)
        .ok()?
        .join()
        .ok()?;

    // 流里那张图过一道解码才拿到像素（见 [`decode_bgra`]），尺寸以解码结果为准。
    let (w, h, bgra) = decode_bgra(&out)?;
    Some(IconRaster {
        width: w,
        height: h,
        rgba: opaque_rgba_over_white(&bgra),
    })
}

/// 把渲染产物还原成 BGRA8（预乘）像素。
///
/// ⚠️ `RenderWithOptionsToStreamAsync` 交进流里的**不是裸像素**，而是按
/// `PdfPageRenderOptions.BitmapEncoderId` 编好的一张图（默认 PNG；早年 Windows 10 才
/// 给裸 BGRA8）。所以这里拿系统自带的 `BitmapDecoder` 过一道，再问 `SoftwareBitmap`
/// 要内存缓冲——顺手把像素格式钉成 `Bgra8` + 预乘，免得碰上不带 alpha 的格式。
fn decode_bgra(stream: &InMemoryRandomAccessStream) -> Option<(u32, u32, Vec<u8>)> {
    stream.Seek(0).ok()?;
    let decoder = BitmapDecoder::CreateAsync(stream).ok()?.join().ok()?;
    let bitmap = decoder
        .GetSoftwareBitmapConvertedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied)
        .ok()?
        .join()
        .ok()?;
    let (w, h) = (bitmap.PixelWidth().ok()?, bitmap.PixelHeight().ok()?);
    if w <= 0 || h <= 0 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let buffer = bitmap.LockBuffer(BitmapBufferAccessMode::Read).ok()?;
    let plane = buffer.GetPlaneDescription(0).ok()?;
    // `GetBuffer` 给的指针归这块内存缓冲引用管：拷完之前 `reference` 必须在作用域里
    // 活着（它 drop 时运行时才会把这块缓冲解掉），所以拷贝就写在这儿，不外传。
    let reference = buffer.CreateReference().ok()?;
    let access: IMemoryBufferByteAccess = reference.cast().ok()?;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut capacity: u32 = 0;
    // SAFETY: 两个出参写进上面的局部变量；`plane` 描述的正是这块缓冲。
    unsafe {
        access.GetBuffer(&mut ptr, &mut capacity).ok()?;
    }
    if ptr.is_null() {
        return None;
    }
    // 行距（stride）通常比 `w * 4` 大（按字对齐），而 [`IconRaster`] 要的是紧挨着的行。
    let (start, stride) = (
        plane.StartIndex.max(0) as usize,
        plane.Stride.max(0) as usize,
    );
    let row_bytes = w.checked_mul(4)?;
    if stride < row_bytes {
        return None;
    }
    let need = start.checked_add(stride.checked_mul(h - 1)?.checked_add(row_bytes)?)?;
    if (capacity as usize) < need {
        return None;
    }
    let mut out = vec![0u8; w.checked_mul(h)?.checked_mul(4)?];
    // SAFETY: 上面按 `need ≤ capacity` 逐行核过区间，读的都是这块缓冲内部。
    unsafe {
        let src = std::slice::from_raw_parts(ptr.add(start), capacity as usize - start);
        for y in 0..h {
            let from = y * stride;
            let to = y * row_bytes;
            out[to..to + row_bytes].copy_from_slice(&src[from..from + row_bytes]);
        }
    }
    Some((w as u32, h as u32, out))
}

/// WinRT 的 BGRA8 预乘 → 上层的 RGBA8 预乘**且已铺白纸**。
///
/// 两件事：
///
/// * **换通道**：WinRT 交的是 `B, G, R, A`，[`IconRaster`] 约定 `R, G, B, A`。
/// * **合成到白底**：PDF 页面本身是**透明**的（只画文字笔画），WinRT 如实给出带
///   alpha 的一张图。直接编码 PNG 就是一张黑纸（透明像素在图里看着是黑的），所以
///   按 macOS 那份的做法铺一层白：预乘值合成到白是 `c + (255 − a)`，合成完处处
///   不透明，alpha 拉满。
fn opaque_rgba_over_white(bgra: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; bgra.len()];
    let (src, _) = bgra.as_chunks::<4>();
    let (dst, _) = out.as_chunks_mut::<4>();
    for (px, o) in src.iter().zip(dst) {
        let a = px[3] as i32;
        let paper = 255 - a;
        o[0] = (px[2] as i32 + paper).clamp(0, 255) as u8; // R
        o[1] = (px[1] as i32 + paper).clamp(0, 255) as u8; // G
        o[2] = (px[0] as i32 + paper).clamp(0, 255) as u8; // B
        o[3] = 255;
    }
    out
}

// ---- 文件剪贴板：写 `CF_HDROP`，读 Shell 记的「这批文件是剪切来的吗」 ----

/// 标准剪贴板格式 `CF_HDROP`（**固定号 15**，全桌面的应用都认它）。
///
/// 与上面那个 CLSID 同理：`windows` 0.58 把它放在 `Win32::System::Ole` 里，为了
/// 一个常量把整个 Ole 模块链进来不值，而它是协议常量、不会变。
const CF_HDROP: u32 = 15;

/// `Preferred DropEffect` 的正文位（0x1=复制、0x2=移动、0x4=建快捷方式）。
/// 资源管理器自己就是靠这一位决定 Ctrl+V 是搬走还是留一份。
const DROP_EFFECT_COPY: u32 = 0x1;
const DROP_EFFECT_MOVE: u32 = 0x2;

/// `DROPFILES` 的头部长度，也就是紧跟其后的那份「双 NUL 结尾 UTF-16 路径表」的
/// 起始偏移：`pFiles: u32` + `pt: POINT`(2×i32) + `fNC: BOOL` + `fWide: BOOL`。
///
/// 这里**不按 crate 的结构体来摆**：`windows` 各版本对 `DROPFILES` 是否带
/// `files: [u16; 1]` 尾字段口径不一，而自己铺字节就没有这个问题——头部就是
/// 上面那 20 字节，路径表紧跟其后。
const DROPFILES_HEADER: u32 = 20;

/// 打开系统剪贴板的守卫（`OpenClipboard` / `CloseClipboard` 必须成对——漏一口，
/// 整个桌面的应用都读不到剪贴板，而且症状看起来像「别的程序粘不出来」）。
struct ClipboardGuard;

impl ClipboardGuard {
    /// 抢不到就是错误（别的进程正开着剪贴板）。**不重试**：为一个剪贴板操作把线程
    /// 挂进重试循环不值得，调用方要么报错、要么按默认语义答。
    fn open() -> Option<Self> {
        unsafe { OpenClipboard(None).ok() }.map(|_| Self)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

/// 系统剪贴板当前那批文件是**剪切**来的吗。
///
/// 契约（为什么 macOS 那边直接答 `false`）写在 `lib.rs::clipboard_files_are_cut`。
pub fn clipboard_files_are_cut() -> bool {
    let Some(_clip) = ClipboardGuard::open() else {
        return false;
    };
    let Some(format) = register_drop_effect_format() else {
        return false;
    };
    let Ok(handle) = (unsafe { GetClipboardData(format) }) else {
        // 没有这个格式：不是资源管理器那类「剪切/复制」放上来的。
        return false;
    };
    let global = HGLOBAL(handle.0);
    let ptr = unsafe { GlobalLock(global) };
    if ptr.is_null() {
        return false;
    }
    // 这份全局内存的正文就是一个 DWORD。
    let effect = unsafe { (ptr as *const u32).read_unaligned() };
    let _ = unsafe { GlobalUnlock(global) };
    effect & DROP_EFFECT_MOVE != 0
}

/// 把一批本机文件写进系统剪贴板（`CF_HDROP` + `Preferred DropEffect`）。
///
/// ⚠️ 这条**不进单元测试**：它会把开发者机器上真实的剪贴板覆盖掉（与 `reveal`
/// 拉起资源管理器、`eject` 把用户的盘停掉是同一条红线）。字节布局由
/// `dropfiles_bytes` 的单测钉住，真发出去那一步走真机验证。
pub fn write_file_clipboard(paths: &[PathBuf], cut: bool) -> Result<(), PlatformError> {
    if paths.is_empty() {
        return Err(PlatformError::Failed("没有要写进剪贴板的文件".to_string()));
    }
    let body = dropfiles_bytes(paths);
    let Some(_clip) = ClipboardGuard::open() else {
        return Err(PlatformError::Failed(
            "打不开系统剪贴板（多半是别的程序正占着它）".to_string(),
        ));
    };
    unsafe { EmptyClipboard() }
        .map_err(|e| PlatformError::Failed(format!("清空系统剪贴板失败：{e}")))?;
    set_clipboard_bytes(&body, CF_HDROP)?;
    // 这一位决定别的应用粘出去的是「复制」还是「剪切」；注册不上就只写文件本体，
    // 对方按复制处理——那是可接受的降级，不该让整个复制动作失败。
    if let Some(format) = register_drop_effect_format() {
        let effect = if cut {
            DROP_EFFECT_MOVE
        } else {
            DROP_EFFECT_COPY
        };
        let _ = set_clipboard_bytes(&effect.to_ne_bytes(), format);
    }
    Ok(())
}

/// 组一份 `DROPFILES`：20 字节头 + 每条路径（UTF-16、各自 NUL 结尾）+ 一个额外 NUL。
fn dropfiles_bytes(paths: &[PathBuf]) -> Vec<u8> {
    let mut units: Vec<u16> = Vec::new();
    for p in paths {
        units.extend(encode_wide(p).iter().copied());
    }
    units.push(0);
    let mut out = Vec::with_capacity(DROPFILES_HEADER as usize + units.len() * 2);
    out.extend_from_slice(&DROPFILES_HEADER.to_ne_bytes());
    // pt（屏幕坐标，粘贴时不参考）、fNC（不在非客户区）、fWide（下面是 UTF-16）。
    out.extend_from_slice(&[0u8; 12]);
    out.extend_from_slice(&1u32.to_ne_bytes());
    // 路径表按原样铺字节：Windows 只有小端。
    let raw = unsafe {
        std::slice::from_raw_parts(
            units.as_ptr() as *const u8,
            units.len() * std::mem::size_of::<u16>(),
        )
    };
    out.extend_from_slice(raw);
    out
}

/// `Preferred DropEffect` 这个自定义格式号（注册不上返回 `None`）。
fn register_drop_effect_format() -> Option<u32> {
    let name = wide_str("Preferred DropEffect");
    match unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) } {
        0 => None,
        format => Some(format),
    }
}

/// 以某个剪贴板格式交出一段字节。
///
/// ⚠️ `SetClipboardData` 成功后那块全局内存就**归系统**了（直到下一次
/// `EmptyClipboard`），所以成功分支绝不能 `GlobalFree`；失败分支反过来还是我们的，
/// 不释放就是每失败一次漏一次。
fn set_clipboard_bytes(bytes: &[u8], format: u32) -> Result<(), PlatformError> {
    unsafe {
        let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
            .map_err(|e| PlatformError::Failed(format!("分配剪贴板内存失败：{e}")))?;
        let ptr = GlobalLock(global);
        if ptr.is_null() {
            let _ = GlobalUnlock(global);
            let _ = GlobalFree(global);
            return Err(PlatformError::Failed(format!(
                "锁定剪贴板内存失败：{}",
                std::io::Error::last_os_error()
            )));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        let _ = GlobalUnlock(global);
        match SetClipboardData(format, HANDLE(global.0)) {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = GlobalFree(global);
                Err(PlatformError::Failed(format!("写入剪贴板失败：{e}")))
            }
        }
    }
}

fn encode_wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// [`encode_wide`] 的 `&str` 版（盘符根、`\\.\C:` 这类自己拼出来的路径）。
fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---- 键盘布局：问「这个字符在本布局上是哪个键未加 Shift 的样子」 ----

/// 字符 `ch` 在**当前键盘布局**上的基本键（也就是它所在物理键未加 Shift 打出的字符）。
///
/// 给 `mo_ui::keys` 折 Shift 的印刷变体用（见那边的 `fold_typographic_shift`）：默认键位
/// 按 US 布局写（`cmd+=`、`cmd+.`），而符号键的「Shift 后是什么字符」是**布局的事**——
/// 德语布局上 `:` 才是 `.` 的 Shift 变体，`?` 与 `/` 根本在两个不同的键上。照一张 US 表
/// 折所有布局，就会把德语用户按下的 `:` 折成分号、把 `?` 折到 `/` 上：动作串到别处，
/// 而默认键位反而按不出来。
///
/// 问的是当前**线程**的布局（`GetKeyboardLayout(0)`），所以用户中途换布局 / 切输入法
/// 立刻跟着变；这一步**不缓存**，缓存就会在换布局后继续拿旧表折键（一次系统调用换一次
/// 正确，划算）。
///
/// 答 `None`（= 问不出，调用方退回自己的表）：本布局打不出这个字符；要 AltGr / Ctrl
/// 参与才出得来（那是同一个键的**第三种**字符，不是 Shift 的印刷变体）；或者是死键
/// （`¨` `'` 那类按下去等下一个键的键位）。
pub fn unshifted_key(ch: char) -> Option<char> {
    let Ok(code) = u16::try_from(ch) else {
        return None;
    };
    unsafe {
        let hkl = GetKeyboardLayout(0);
        let scan = VkKeyScanExW(code, hkl);
        if scan < 0 {
            return None;
        }
        let vk = (scan & 0xFF) as u32;
        // 高字节是修饰位：0x1=Shift、0x2=Ctrl、0x4=Alt（合起来 0x6 就是 AltGr）。
        // 只接受「不用 Shift」与「只用 Shift」两种，别的都不是印刷变体。
        if !matches!((scan >> 8) & 0xFF, 0 | 1) {
            return None;
        }
        let mapped = MapVirtualKeyExW(vk, MAPVK_VK_TO_CHAR, hkl);
        // bit15 置位表示这是死键；低 15 位才是字符。0 表示这个键在本布局上没有字符。
        if mapped == 0 || mapped & 0x8000 != 0 {
            return None;
        }
        char::from_u32(mapped & 0x7FFF)
    }
}

/// 错误里必须带上**是哪条路径**（用户有多选删除，分不清位置的报错等于没说）。
fn failed(action: &str, path: &Path, e: windows::core::Error) -> PlatformError {
    PlatformError::Failed(format!("{action}失败：{}（{e}）", path.display()))
}

/// 测试用：把**当前线程**的键盘布局临时换成指定 KLID， Drop 时换回原来那个。
///
/// `ActivateKeyboardLayout` 默认只作用于调用线程，而 [`unshifted_key`] 问的正是
/// `GetKeyboardLayout(0)`（本线程的布局）——所以同一个测试里激活、同一个线程里问，
/// 不会串到并行的其它用例，也不碰用户的输入法。
#[cfg(test)]
struct ActiveLayout {
    previous: windows::Win32::UI::Input::KeyboardAndMouse::HKL,
}

#[cfg(test)]
impl ActiveLayout {
    /// 这台机器上没有这个布局（返回 `None`，调用方跳过该断言）。
    fn new(klid: &str) -> Option<Self> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            ActivateKeyboardLayout, GetKeyboardLayout, LoadKeyboardLayoutW,
            ACTIVATE_KEYBOARD_LAYOUT_FLAGS,
        };
        let wide = wide_str(klid);
        // 先**装载**（0x10 = KLF_REPLACELANG，不激活），这样下一步能先把本线程原来的
        // 布局记下来，Drop 时才还得回去。
        let hkl = unsafe {
            LoadKeyboardLayoutW(PCWSTR(wide.as_ptr()), ACTIVATE_KEYBOARD_LAYOUT_FLAGS(0x10))
        }
        .ok()?;
        let previous = unsafe { GetKeyboardLayout(0) };
        // 激活：flags 为 0 表示只给**调用线程**，不动系统默认，也不影响并行的其它用例。
        unsafe { ActivateKeyboardLayout(hkl, ACTIVATE_KEYBOARD_LAYOUT_FLAGS(0)) }.ok()?;
        Some(Self { previous })
    }
}

#[cfg(test)]
impl Drop for ActiveLayout {
    fn drop(&mut self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            ActivateKeyboardLayout, ACTIVATE_KEYBOARD_LAYOUT_FLAGS,
        };
        let _ = unsafe { ActivateKeyboardLayout(self.previous, ACTIVATE_KEYBOARD_LAYOUT_FLAGS(0)) };
    }
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

    /// `DROPFILES` 的正文怎么摆：20 字节头 + 双 NUL 收尾的 UTF-16 路径表。
    ///
    /// 头部偏一格、或者结尾少一个 NUL，别的应用从剪贴板里就**读不出文件**
    /// （资源管理器会直接灰掉「粘贴」）——这种错在 Mo 自己身上完全看不出来，
    /// 所以钉死字节。
    #[test]
    fn dropfiles_bytes_lays_out_the_header_and_the_wide_list() {
        let bytes = dropfiles_bytes(&[
            PathBuf::from("D:\\a.txt"),
            PathBuf::from("D:\\中文 与 空格.txt"),
        ]);
        assert_eq!(&bytes[0..4], &DROPFILES_HEADER.to_ne_bytes(), "pFiles");
        assert_eq!(&bytes[4..16], &[0u8; 12], "pt 与 fNC 都是 0");
        assert_eq!(
            &bytes[16..20],
            &1u32.to_ne_bytes(),
            "fWide=1：路径表按 UTF-16 摆"
        );

        let tail = &bytes[DROPFILES_HEADER as usize..];
        let units: Vec<u16> = tail
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert!(units.ends_with(&[0, 0]), "路径表必须再空一项收尾");
        let text = String::from_utf16(&units[..units.len() - 2]).expect("写进去的就是 UTF-16");
        let list: Vec<&str> = text.split('\0').collect();
        assert_eq!(
            list,
            vec!["D:\\a.txt", "D:\\中文 与 空格.txt"],
            "每条路径各自 NUL 结尾、顺序不变"
        );
    }

    /// 问的是**当前布局**，不是硬编码：德语（00000407）上符号键的配对与 US 完全不同。
    ///
    /// 这些断言就是 `mo_ui::keys::fold_typographic_shift` 不能只按 US 表折的理由：
    /// US 表里 `:`→`;`、`?`→`/` 两条在德语布局上会把用户按下的键**串到别的动作**上。
    /// 期望值取自 2026-09-26 在本机的实测（同一套 `VkKeyScanExW` + `MapVirtualKeyExW`，
    /// 见 `.tmp/vk.ps1`），不是照文档想当然。
    #[test]
    fn unshifted_key_follows_the_german_layout() {
        let Some(_de) = ActiveLayout::new("00000407") else {
            eprintln!("这台机器装不上德语键盘布局（kbdgr），跳过布局相关断言");
            return;
        };
        // 德语：`.` 这个键未加 Shift 打出 `.`，加了打出 `:`。
        assert_eq!(unshifted_key(':'), Some('.'));
        // `<` 在德语上是**独立键**，`>` 才是它的 Shift 变体（US 表说的是 `>`→`.`）。
        assert_eq!(unshifted_key('>'), Some('<'));
        assert_eq!(unshifted_key('<'), Some('<'));
        // `+` 自己就是基本键（US 表把它当成 Shift+`=`，德语上那样折会把两个键并成一个）。
        assert_eq!(unshifted_key('+'), Some('+'));
        // AltGr 出来的字符不是「同一个键的 Shift 变体」，不答。
        assert_eq!(unshifted_key('{'), None);
        assert_eq!(unshifted_key('@'), None);
        // 布局答了就不再退表：德语上 `"` 是 Shift+`2`（数字），不是 Shift+`'`。
        assert_eq!(unshifted_key('"'), Some('2'));
        // 德语上 `?` 这个键未加 Shift 打出 `ß`。答出的是**非 ASCII** 的 `ß` 而不是 `/`
        // （US 表给的答案），调用方因此不折这一对——`?` 与 `/` 在德语上是两个键。
        assert_eq!(unshifted_key('?'), Some('ß'));
    }

    /// US 布局（00000409，每台 Windows 都带 kbdus）：与 `SYMBOL_PAIRS` 那张表逐条对得上。
    ///
    /// 显式**激活**布局再问，而不是靠本机默认键盘——CI 镜像上装的是什么布局不由我们
    /// 决定，默认布局上的断言就是「在别人机器上随机红」。
    #[test]
    fn unshifted_key_answers_the_us_layout() {
        let Some(_us) = ActiveLayout::new("00000409") else {
            eprintln!("这台机器装不上 US 键盘布局（kbdus），跳过");
            return;
        };
        for (shifted, base) in [
            ('+', '='),
            ('_', '-'),
            ('{', '['),
            ('}', ']'),
            ('<', ','),
            ('>', '.'),
            (':', ';'),
            ('"', '\''),
            ('?', '/'),
            ('|', '\\'),
            ('~', '`'),
        ] {
            assert_eq!(
                unshifted_key(shifted),
                Some(base),
                "`{shifted}` 是 Shift+`{base}`"
            );
            // 基本键这一端也答自己（`cmd+.` 与 `cmd+shift+.` 要折到同一个键组）。
            assert_eq!(unshifted_key(base), Some(base));
        }
        // 数字的 Shift 变体照样答出数字：是不是该折由调用方判（视图模式占着 `cmd+1`）。
        assert_eq!(unshifted_key('!'), Some('1'));
        assert_eq!(unshifted_key('@'), Some('2'));
    }

    /// gpui 的 Windows 后端**不**经过我们的 `VkKeyScanExW`，它自己调 `ToUnicode`
    /// （只看当前线程的布局）算出字符，再交给我们折。这条测试把两端接在一起：
    /// 同一个德语线程上，`ToUnicode(102 键 + Shift)` 交出 `>`，而 `unshifted_key('>')`
    /// 必须答回 `<`——两边各错一步，键位就绑到别的动作上了。
    ///
    /// 调用形状照抄 gpui（`state[VK_SHIFT]=0x80`、buffer 8 个 u16、flags 0x5），
    /// 不是我觉得该怎么调。
    #[test]
    fn to_unicode_and_unshifted_key_agree_on_the_same_layout() {
        fn shifted_char(vk: u32, scan: u32) -> Option<char> {
            use windows::Win32::UI::Input::KeyboardAndMouse::ToUnicode;
            let mut state = [0u8; 256];
            state[0x10] = 0x80; // VK_SHIFT
            let mut buffer = [0u16; 8];
            let len = unsafe { ToUnicode(vk, scan, Some(&state), &mut buffer, 0x5) };
            if len < 1 {
                return None;
            }
            char::from_u32(buffer[0] as u32)
        }

        // VK_OEM_102（102 键）的扫描码，US 与德语都是 0x56。
        const LT102: u32 = 0xE2;
        const SCAN_LT102: u32 = 0x56;

        let Some(_de) = ActiveLayout::new("00000407") else {
            eprintln!("这台机器装不上德语键盘布局（kbdgr），跳过");
            return;
        };
        assert_eq!(shifted_char(LT102, SCAN_LT102), Some('>'));
        assert_eq!(unshifted_key('>'), Some('<'));
        drop(_de);

        let Some(_us) = ActiveLayout::new("00000409") else {
            eprintln!("这台机器装不上 US 键盘布局（kbdus），跳过");
            return;
        };
        assert_eq!(shifted_char(LT102, SCAN_LT102), Some('|'));
        assert_eq!(unshifted_key('|'), Some('\\'));
    }

    /// 卷根上挂 `$Recycle.Bin`（回收站每卷一份，不在子目录里）。
    #[test]
    fn recycle_roots_points_at_the_volume_root() {
        let roots = recycle_roots(Path::new("C:\\Users\\demo\\a.txt"));
        assert!(roots
            .iter()
            .any(|r| r.to_string_lossy().eq_ignore_ascii_case("C:\\$Recycle.Bin")));
    }

    /// 推出只认**卷根**：侧栏给的就是 `E:\`，认成一个子目录会让人以为能推出它。
    /// 认不出来的形态一律 `None` → `Unsupported` → 上层走自己的卸载办法。
    #[test]
    fn eject_only_accepts_a_drive_root() {
        for (p, want) in [
            ("E:\\", Some('E')),
            ("E:/", Some('E')),
            ("e:", Some('E')),
            ("E:\\dir", None),
            ("C:\\Users\\demo", None),
            ("/tmp", None),
            ("\\\\server\\share", None),
        ] {
            assert_eq!(drive_letter(Path::new(p)), want, "{p}");
        }
    }

    /// 展示名与资源管理器同形；`ejectable` 只给可移动介质（内置盘摆按钮=找骂）。
    #[test]
    fn volume_rows_look_like_explorers() {
        assert_eq!(volume_name('C', DRIVE_FIXED, Some("系统盘")), "系统盘 (C:)");
        assert_eq!(volume_name('C', DRIVE_FIXED, None), "本地磁盘 (C:)");
        assert_eq!(volume_name('D', DRIVE_REMOVABLE, None), "可移动磁盘 (D:)");
        assert_eq!(volume_name('E', DRIVE_CDROM, None), "光盘 (E:)");
        assert!(!is_ejectable(DRIVE_FIXED));
        assert!(!is_ejectable(DRIVE_RAMDISK));
        assert!(is_ejectable(DRIVE_REMOVABLE));
        assert!(is_ejectable(DRIVE_CDROM));
    }

    /// 黑底 / 白底两张图解预乘 RGBA：`a = (black + 255 − white) / 255`，RGB 直接取黑底。
    ///
    /// 输入按 32 位 BI_RGB 的内存序（B, G, R, 垃圾）给，所以断言里也能顺手验一遍
    /// BGRA → RGBA 的换序没写反。
    #[test]
    fn backdrops_solve_back_the_alpha() {
        // 一个不透明的纯红像素：两张图一样。
        let black = [0u8, 0, 255, 0];
        let white = [0u8, 0, 255, 0];
        assert_eq!(
            premultiplied_from_backdrops(&black, &white),
            [255, 0, 0, 255]
        );

        // 全透明：黑底还是黑、白底被刷成白。
        let black = [0u8, 0, 0, 0];
        let white = [255u8, 255, 255, 0];
        assert_eq!(premultiplied_from_backdrops(&black, &white), [0, 0, 0, 0]);

        // 五成透明的纯红：`a·C` = (128,0,0)，白底那份 = `a·C + 255(1−a)` ≈ (255,127,127)。
        let black = [0u8, 0, 128, 0];
        let white = [127u8, 127, 255, 0];
        let got = premultiplied_from_backdrops(&black, &white);
        assert_eq!(&got[..3], [128, 0, 0], "交出去的必须是**预乘**值");
        assert!(
            (got[3] as i32 - 128).abs() <= 1,
            "alpha 该解回 ~128，实际 {}",
            got[3]
        );

        // 多个像素各算各的，互不串味。
        let black = [0u8, 0, 255, 0, 0, 255, 0, 0];
        let white = [0u8, 0, 255, 0, 0, 255, 0, 0];
        assert_eq!(
            premultiplied_from_backdrops(&black, &white),
            [255, 0, 0, 255, 0, 255, 0, 255]
        );
    }

    /// 真的向 shell 要一张图标：尺寸、缓冲长度、以及「确实画出了东西」都要对。
    ///
    /// 这条会开 GDI 对象、查注册表的图标关联——都是无副作用的读操作，且**不要求
    /// 界面在跑**（`GetIconInfo` 那一套不需要窗口），所以在 CI / 本地都能跑。
    #[test]
    fn asks_the_shell_for_real_icons() {
        for (what, got) in [
            ("文件夹", folder_icon_raster(32)),
            ("扩展名 txt", ext_icon_raster("txt", 32)),
            ("扩展名 .pdf（带点也要能吃）", ext_icon_raster(".pdf", 32)),
            (
                "真实文件",
                file_icon_raster(&std::env::current_exe().unwrap(), 32),
            ),
        ] {
            let r = got.unwrap_or_else(|| panic!("{what}：shell 没给图标"));
            assert_eq!((r.width, r.height), (32, 32), "{what}");
            assert_eq!(r.rgba.len(), 32 * 32 * 4, "{what}");
            let (pixels, tail) = r.rgba.as_chunks::<4>();
            assert!(tail.is_empty(), "{what}：缓冲长度不是 4 的整数倍");
            let opaque = pixels.iter().filter(|p| p[3] > 200).count();
            let see_through = pixels.iter().filter(|p| p[3] < 50).count();
            assert!(opaque > 32, "{what}：画出了 {} 个不透明像素", opaque);
            // 图标是**带透明边**的：一张四角不透明的方图说明 alpha 那一步解错了。
            assert!(see_through > 0, "{what}：一个透明像素都没有，alpha 解错了");
            // 预乘不变式：`RGB ≤ A`（`a·C` 不可能大于 `a`）。
            for p in pixels {
                assert!(
                    p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3],
                    "{what}：不是预乘值（{:?} > a={}）",
                    &p[..3],
                    p[3]
                );
            }
        }
        // 空扩展名 = 「没这个名字可问」，与「问了没问到」是两回事。
        assert!(ext_icon_raster("", 32).is_none());
        assert!(ext_icon_raster(".", 32).is_none());
    }

    /// 列盘在测试环境里也必须跑得动（渲染线程每 5 秒调一次，不能 panic、不能空转）。
    #[test]
    fn listing_volumes_is_sane() {
        let vs = volumes();
        // 这台机器上至少有一块盘；而且每条都得是卷根 + 有名字。
        assert!(!vs.is_empty(), "列不出任何卷宗：{vs:?}");
        for v in &vs {
            assert!(v.name.contains('('), "名字里该带盘符：{v:?}");
            let s = v.path.to_string_lossy();
            assert!(s.ends_with(":\\") && s.len() == 3, "路径该是卷根：{s}");
        }
        // 网络盘归「网络」区，不该出现在这里。
        for v in &vs {
            let wide = wide_str(&v.path.to_string_lossy());
            assert_ne!(
                unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) },
                DRIVE_REMOTE,
                "映射盘跑进本机卷宗列表了：{v:?}"
            );
        }
    }

    /// 黑、白、半透明三种像素：换通道 + 铺白底一步做完。
    ///
    /// 输入按 WinRT 的 `B, G, R, A`（预乘）给，所以这里也顺手验一遍换序没写反——
    /// 写反了「纯蓝方块」会渲染成红色，肉眼在真机上一定能看出来，但不如一条断言便宜。
    #[test]
    fn bgra_becomes_rgba_on_paper() {
        // 不透明的纯蓝。
        assert_eq!(opaque_rgba_over_white(&[255, 0, 0, 255]), [0, 0, 255, 255]);
        // 不透明的纯红：R 与 B 换位。
        assert_eq!(opaque_rgba_over_white(&[0, 0, 255, 255]), [255, 0, 0, 255]);
        // 全透明（PDF 的纸面）→ 白纸。
        assert_eq!(opaque_rgba_over_white(&[0, 0, 0, 0]), [255, 255, 255, 255]);
        // 五成透明的纯蓝（预乘值 B=128）：合成到白 = `c + (255 − a)`。
        assert_eq!(
            opaque_rgba_over_white(&[128, 0, 0, 128]),
            [127, 127, 255, 255]
        );
        // 多个像素各算各的。
        assert_eq!(
            opaque_rgba_over_white(&[0, 0, 255, 255, 0, 0, 0, 0]),
            [255, 0, 0, 255, 255, 255, 255, 255]
        );
    }

    /// 手搓一份**最小可解析**的 PDF：一页 `w×h` 点，内容流原样塞进去。
    ///
    /// 不引依赖、不下载样本就能给渲染层验货。xref 的字节偏移按拼装过程现算——算错的
    /// 偏移会让解析器走「修复」路径，能不能救回来全看运气，索性一次算准。
    fn minimal_pdf(w: u32, h: u32, content: &str) -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R >>"),
            format!(
                "<< /Length {} >>\nstream\n{content}endstream",
                content.len()
            ),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// 真的渲染一张 PDF 首页：尺寸、底色、方块颜色与位置都要对得上。
    ///
    /// ⚠️ 这条会起 WinRT（`RoInitialize` + `Windows.Data.Pdf` + `BitmapDecoder`），跑在
    /// 测试的子线程上——特意**不**在单线程套间里等完成回调，正是为了确认「blocking 池
    /// 里能自己等完」这件事。
    #[test]
    fn renders_the_first_page_of_a_real_pdf() {
        // 100×200 点的一页，左下角 60×60 的纯蓝方块。
        let pdf = minimal_pdf(100, 200, "0 0 1 rg\n20 20 60 60 re\nf\n");
        let path = std::env::temp_dir().join(format!("mo-pdf-{}.pdf", std::process::id()));
        std::fs::write(&path, &pdf).expect("临时 PDF 写得出去");

        let r = pdf_page_raster(&path, 1024).expect("渲染不出首页：WinRT 这条路没通");
        // `PdfPage::Size` 给的是 **96 DPI** 的像素数：100×200 点 = 133.33×266.67 px。
        // 页面本来就比 max_edge 小 → 不放大，但也绝不缩小。
        assert!(
            (r.width as i64 - 133).abs() <= 2 && (r.height as i64 - 267).abs() <= 2,
            "尺寸该是 96 DPI 下的原样，拿到的是 {:?}",
            (r.width, r.height)
        );
        let px = r.rgba.as_chunks::<4>().0;
        assert_eq!(
            px.len(),
            r.width as usize * r.height as usize,
            "像素数与尺寸对得上"
        );
        assert!(px.iter().all(|p| p[3] == 255), "铺过白纸的图必须处处不透明");
        assert!(
            px.iter().all(|p| !(p[0] > 200 && p[2] < 60)),
            "一个红像素都没有——有就说明通道换序写反了，蓝方块画成了红的"
        );
        let paper = px
            .iter()
            .filter(|p| p[0] > 250 && p[1] > 250 && p[2] > 250)
            .count();
        assert!(paper > 20_000, "方块外都得是白纸面，实际 {paper}");

        // 方块：60×60 点 = 80×80 px，落在位图的左下（PDF 的 y 轴向上）。
        let box_of = |p: &&[u8; 4]| p[2] > 200 && p[0] < 60;
        let idx = px
            .iter()
            .enumerate()
            .filter(|(_, p)| box_of(p))
            .map(|(i, _)| i);
        let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0usize, usize::MAX, 0usize);
        let mut blue = 0;
        for i in idx {
            let (x, y) = (i % r.width as usize, i / r.width as usize);
            (x0, x1) = (x0.min(x), x1.max(x));
            (y0, y1) = (y0.min(y), y1.max(y));
            blue += 1;
        }
        assert!(blue > 5_000, "80×80 的方块该有 ~6400 个像素，实际 {blue}");
        let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
        assert!(
            (70..=90).contains(&bw) && (70..=90).contains(&bh),
            "方块外框该是个 ~80×80 的正方形，实际 {bw}×{bh}"
        );
        assert!(
            x0 < r.width as usize / 3 && y0 > r.height as usize / 3,
            "方块在左下方：左上角 ({x0},{y0})，画布 {}×{}",
            r.width,
            r.height
        );

        // 缩到长边 50：等比 25×50，方块跟着缩成 ~15×15。
        let small = pdf_page_raster(&path, 50).expect("缩放这条也走得通");
        assert_eq!((small.width, small.height), (25, 50), "长边贴住 max_edge");
        let blue_small = small
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[2] > 200 && p[0] < 60)
            .count();
        assert!(blue_small > 100, "缩完仍有方块，实际 {blue_small}");

        std::fs::remove_file(&path).ok();
    }

    /// 喂进去的不是 PDF（或压根是空文件）→ 安静地 `None`，不许 panic。
    ///
    /// 用户随手把扩展名改成 `.pdf` 的非 PDF 文件是常态，第二拍拿 `None` 退回占位文案。
    #[test]
    fn junk_input_is_not_a_pdf() {
        let dir = std::env::temp_dir().join(format!("mo-pdf-junk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录建得出来");
        let not_pdf = dir.join("liar.pdf");
        std::fs::write(&not_pdf, b"just some text, definitely not a PDF").expect("写得动");
        assert!(
            pdf_page_raster(&not_pdf, 512).is_none(),
            "内容不是 PDF 渲染不出来"
        );
        assert!(
            pdf_page_raster(Path::new(&dir), 512).is_none(),
            "目录不是文件"
        );
        assert!(
            pdf_page_raster(&dir.join("missing.pdf"), 512).is_none(),
            "不存在的路径"
        );
        assert!(
            pdf_page_raster(&not_pdf, 0).is_none(),
            "max_edge 为 0 没意义，直接不给渲染"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
