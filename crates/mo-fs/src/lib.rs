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
#[cfg(unix)]
pub(crate) fn file_id_for(path: &Path) -> FileId {
    if let Ok(meta) = std::fs::metadata(path) {
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
    if sym.is_symlink() {
        return match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => Ok(mo_core::EntryKind::Directory),
            Ok(_) => Ok(mo_core::EntryKind::File),
            Err(_) => Ok(mo_core::EntryKind::Symlink),
        };
    }
    if sym.is_dir() {
        Ok(mo_core::EntryKind::Directory)
    } else if sym.is_file() {
        Ok(mo_core::EntryKind::File)
    } else {
        Ok(mo_core::EntryKind::Other)
    }
}
