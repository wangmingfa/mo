use std::path::Path;

use async_trait::async_trait;
use mo_core::{FileMetadata, MoError, Permissions};

use crate::reader::ReadDirEntry;
use crate::{entry_kind_from_path, file_id_for, to_dir_error, FileSystem};

/// 基于 `std::fs` 的本地文件系统实现。
#[derive(Debug, Default)]
pub struct LocalFileSystem;

#[async_trait]
impl FileSystem for LocalFileSystem {
    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.read_dir_blocking(path)
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        let mut out = Vec::new();
        let mut rd = std::fs::read_dir(path).map_err(to_dir_error)?;
        while let Some(entry) = rd.next().transpose().map_err(to_dir_error)? {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry_kind_from_path(&p).unwrap_or(mo_core::EntryKind::Other);
            let id = file_id_for(&p);
            out.push(ReadDirEntry::new(id, name, kind, p));
        }
        // 目录在前，再按名称排序。
        out.sort_by(|a, b| {
            b.kind
                .is_dir()
                .cmp(&a.kind.is_dir())
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(out)
    }

    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError> {
        let m = std::fs::metadata(path).map_err(to_dir_error)?;
        Ok(FileMetadata {
            size: m.len(),
            modified: m.modified().ok(),
            created: m.created().ok(),
            permissions: Permissions {
                readonly: m.permissions().readonly(),
                hidden: false,
                mode: unix_mode(&m),
            },
        })
    }

    async fn create_dir(&self, path: &Path) -> Result<(), MoError> {
        std::fs::create_dir_all(path).map_err(MoError::Io)
    }

    async fn remove_file(&self, path: &Path) -> Result<(), MoError> {
        std::fs::remove_file(path).map_err(MoError::Io)
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), MoError> {
        std::fs::remove_dir_all(path).map_err(MoError::Io)
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), MoError> {
        std::fs::rename(from, to).map_err(MoError::Io)
    }
}

/// 取 unix 权限位（低 9 位）；非 unix 平台返回 0。
fn unix_mode(m: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        0
    }
}
