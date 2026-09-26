#![allow(unsafe_code)]
//! Windows 原生集成：「在资源管理器中显示」+ 系统回收站（`IFileOperation`）
//! + 卷宗列表与推出（`GetLogicalDrives` / `CM_Request_Device_Eject`）。
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
//! ⚠️ unsafe 仅限本文件的 shell / COM / 设备管理 FFI 调用（与 `mo_app::shell` 同一约定）。

use std::path::{Path, PathBuf};

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Request_Device_EjectW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    HDEVINFO, PNP_VETO_TYPE, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SP_DEVINFO_DATA,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_MODE,
    OPEN_EXISTING,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Ioctl::{
    GUID_DEVINTERFACE_DISK, IOCTL_STORAGE_EJECT_MEDIA, IOCTL_STORAGE_GET_DEVICE_NUMBER,
    STORAGE_DEVICE_NUMBER,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::UI::Shell::{
    IFileOperation, IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName,
    FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
};

use crate::{PlatformError, Volume};

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

/// [`encode_wide`] 的 `&str` 版（盘符根、`\\.\C:` 这类自己拼出来的路径）。
fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
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
}
