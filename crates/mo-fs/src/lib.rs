//! mo-fs：文件系统抽象层。
//!
//! UI / 应用层只依赖 [`FileSystem`] trait，不直接调用 `std::fs`，
//! 这样以后替换实现（本地 / 远程 / 虚拟 / 测试桩）时上层无需改动。
//!
//! 关键 API：
//!
//! ```text
//! trait FileSystem {
//!     async fn read_dir(&self, path) -> Vec<ReadDirEntry>;
//!     async fn metadata(&self, path) -> FileMetadata;
//!     async fn create_dir(&self, path);
//!     async fn write_file(&self, path, contents);
//!     async fn rename(&self, from, to);
//! }
//! ```
//!
//! 平台实现当前提供 [`LocalFileSystem`]（基于 `std::fs` + `notify`）。

mod local;
mod reader;
mod watcher;

pub use local::LocalFileSystem;
pub use reader::{DirectoryReader, ReadDirEntry};
pub use watcher::{FileSystemWatcher, WatcherEvent};

use async_trait::async_trait;
use mo_core::{DirectoryError, FileId, FileMetadata, MoError};
use std::path::Path;

/// 文件系统抽象。所有异步方法返回 `mo_core::MoError`，便于上层统一处理。
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// 读取目录项（仅 name / kind / path / id，不含完整 metadata）。
    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError>;

    /// **阻塞**读取目录项。
    ///
    /// 目录读取是同步 IO：几万条目可能上百毫秒，在异步 worker 上直接调用会
    /// 卡住整个 runtime。因此 `mo-app` 打开目录时统一走 `spawn_blocking` + 本方法，
    /// 让主线程只承担「写入模型 + 广播」这点工作。
    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError>;

    /// 读取单个文件/目录的元数据。
    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError>;

    /// 读取**整个文件**的内容（跨文件系统的传输用它做下载侧）。
    ///
    /// 默认实现返回错误：只有真能读内容的后端才覆写（本地与三个远程协议），
    /// 省得每个实现都被迫写一遍「不支持」。大文件是整份进内存的——当前传输
    /// 就是「整份读、整份写」，分块流式是后续的事。
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, MoError> {
        Err(MoError::Other(format!(
            "该文件系统不支持读取文件内容：{}",
            path.display()
        )))
    }

    /// 这个路径是不是目录（跨文件系统传输按它分流「递归」还是「读字节」）。
    ///
    /// 默认实现用「能不能列目录」探：本地与 SFTP 直接读 `metadata` 更准，
    /// 这两个后端都有覆写；这里只作兜底。
    async fn is_dir(&self, path: &Path) -> bool {
        self.read_dir(path).await.is_ok()
    }

    /// 创建目录（含父目录）。
    async fn create_dir(&self, path: &Path) -> Result<(), MoError>;

    /// 创建**新文件**并写入内容。
    ///
    /// 目标已存在时必须**失败而不是覆盖**（底层用 `create_new`）：新建文件是
    /// 数据丢失的入口之一，去重是调用方的责任（见 `mo_operations::unique_path`）。
    async fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), MoError>;

    /// 删除文件。
    async fn remove_file(&self, path: &Path) -> Result<(), MoError>;

    /// 删除目录（递归）。
    async fn remove_dir(&self, path: &Path) -> Result<(), MoError>;

    /// 重命名 / 移动（同一文件系统下为原子操作）。
    async fn rename(&self, from: &Path, to: &Path) -> Result<(), MoError>;

    /// 连接是否还活着（**只有远程后端会真的探测**，本地实现不碰网络）。
    ///
    /// 用途：远程会话闲置久了会被服务器单方面掐断（FTP 的 `idle_session_timeout`
    /// 很常见），上层在「闲置一阵子之后再读目录」之前先用它确认连接还在，断了就
    /// 重建——而不是把一句 `Broken pipe (os error 32)` 弹给用户看。
    ///
    /// ⚠️ 会做一次网络往返，必须在 **blocking 上下文**调用（与
    /// [`FileSystem::read_dir_blocking`] 同一个约定）；默认实现直接 `true`。
    fn is_alive(&self) -> bool {
        true
    }
}

/// 从 `std::io::Error` 推导目录错误类型。
pub(crate) fn to_dir_error(e: std::io::Error) -> MoError {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => MoError::Directory(DirectoryError::NotFound),
        ErrorKind::PermissionDenied => MoError::Directory(DirectoryError::PermissionDenied),
        _ => MoError::Io(e),
    }
}

/// 在 unix 上用 `st_dev` + `st_ino` 构造稳定 `FileId`；其他平台退回基于路径的占位 ID。
///
/// 必须 **lstat**（`symlink_metadata`）而不是 stat：符号链接是目录里**独立的一条**，
/// 身份应是它自身的 inode。走 stat 的话，指向同一目标的两个软链会拿到目标的
/// dev+ino → FileId 撞车 → 按 FileId 记账的选择/缓存把两条目当成同一个
/// （实测：点 `.bluework-config` 会把 `.bluework-ui-config` 一起选中）。断链的
/// 软链 stat 会失败退回路径哈希，lstat 则总能给出稳定 ID——这里也更稳。
#[cfg(unix)]
pub(crate) fn file_id_for(path: &Path) -> FileId {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        use std::os::unix::fs::MetadataExt;
        return FileId::new(meta.dev(), meta.ino() as u128);
    }
    FileId::synthetic(path)
}

#[cfg(not(unix))]
pub(crate) fn file_id_for(path: &Path) -> FileId {
    FileId::synthetic(path)
}

/// 为单个路径构造轻量条目信息。
///
/// 供 watcher 的 `Created` 事件做增量插入：外部新建文件时，
/// 不需要重新读取整个目录，只补一条条目即可。
pub fn entry_at(path: &Path) -> Option<ReadDirEntry> {
    let name = path.file_name()?.to_string_lossy().to_string();
    let kind = entry_kind_from_path(path).ok()?;
    Some(ReadDirEntry::new(
        file_id_for(path),
        name,
        kind,
        path.to_path_buf(),
    ))
}

/// 根据路径判断条目类型（正确处理符号链接）。
pub(crate) fn entry_kind_from_path(path: &Path) -> std::io::Result<mo_core::EntryKind> {
    let sym = std::fs::symlink_metadata(path)?;
    Ok(kind_from_metadata(&sym, path))
}

/// 从一份 **lstat** 结果判断条目类型（符号链接仍要跟随一次，见 [`entry_kind_from_path`]）。
///
/// 与 [`entry_kind_from_path`] 分成两个函数是为了让 `read_dir` 能复用手上那次
/// `DirEntry::metadata()` 的结果，省掉按路径再查一次 dentry。
pub(crate) fn kind_from_metadata(m: &std::fs::Metadata, path: &Path) -> mo_core::EntryKind {
    if m.is_symlink() {
        return match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => mo_core::EntryKind::Directory,
            Ok(_) => mo_core::EntryKind::File,
            Err(_) => mo_core::EntryKind::Symlink,
        };
    }
    if m.is_dir() {
        mo_core::EntryKind::Directory
    } else if m.is_file() {
        mo_core::EntryKind::File
    } else {
        mo_core::EntryKind::Other
    }
}

/// 条目名是否「隐藏」：只看名字，零 IO。
///
/// `.` 开头这一条在 unix 系与 Windows 上都**照用**——两边都用 `.` 开头的目录放
/// 配置，Mo 把它当跨平台一致的规则。**注意这不是资源管理器的规则**：Explorer 只
/// 看属性位，`.cargo` 在它眼里完全不隐藏。所以 Windows 上 Mo 会比 Explorer 多藏
/// 一批点开头目录，这是有意的取舍，不是漏判（属性位那条见
/// [`is_hidden_with_metadata`]）。
pub fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

/// 在 [`is_hidden_name`] 之上叠加「文件系统标记位」判据（macOS 的 `UF_HIDDEN`、
/// Windows 的 `FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM`）。
///
/// 标记位藏起来的条目名字不以 `.` 开头，只按名字判会漏：macOS 上是 `chflags
/// hidden` 设的 `~/Library`，Windows 上是 `desktop.ini`（Hidden|System）、
/// `AppData`（Hidden）、`ntuser.dat`（Hidden）这一批——**Windows 的隐藏根本不是
/// 靠名字**，不读属性位就等于没有隐藏文件判据。而标记位只有 stat 才拿得到，所以
/// 做成「上层手上已有 metadata 时的补充判据」——`read_dir` 里那次
/// `DirEntry::metadata()` 是免费的。
pub fn is_hidden_with_metadata(name: &str, m: &std::fs::Metadata) -> bool {
    if is_hidden_name(name) {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        // `UF_HIDDEN`：BSD 的隐藏位，Linux 的 `st_flags` 恒为 0（且该字段不存在）。
        m.st_flags() & 0x8000 != 0
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        hidden_by_attributes(m.file_attributes())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (name, m);
        false
    }
}

/// Windows 上算「隐藏」的两个属性位：`FILE_ATTRIBUTE_HIDDEN` (0x2) 与
/// `FILE_ATTRIBUTE_SYSTEM` (0x4)。
///
/// 单独抽出来是为了能不带 IO 地测：`Metadata` 造不出来，而属性位是纯整数判断。
/// 资源管理器的「隐藏」只认这两位；`SYSTEM` 一并算上是因为 `desktop.ini`、
/// `boot.ini` 这类是 Hidden|System，只判 HIDDEN 也能中，但单 System 的条目
/// （某些卷根）Explorer 同样不显示。
#[cfg(target_os = "windows")]
fn hidden_by_attributes(attributes: u32) -> bool {
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    attributes & (HIDDEN | SYSTEM) != 0
}
