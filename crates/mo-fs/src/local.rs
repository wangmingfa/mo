use std::io::Write;
use std::path::Path;
#[cfg(target_os = "windows")]
use std::path::PathBuf;

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
        // 「此电脑」虚拟根：**空路径**是保留哨兵，代表盘符列表（见 `list_drives`）。
        // 真实文件系统里不可能出现空路径目录，不会与用户数据冲突。
        if path.as_os_str().is_empty() {
            return list_drives(path);
        }
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

    async fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), MoError> {
        // 用 `create_new` 而不是 `std::fs::write`：后者会**静默覆盖**已存在的文件。
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(MoError::Io)?;
        f.write_all(contents).map_err(MoError::Io)
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

/// 「此电脑」虚拟目录：枚举本机盘符（Windows）。
///
/// 标准库没有逻辑盘符 API，这里直接探测 `A:\`..`Z:\` 中实际存在的
/// 目录根（26 次 stat，成本可忽略）。名称是「C:」这类盘符标签，
/// 路径是真实盘符根，后续进入 / 监听 / 元数据都走既有真实路径链路。
#[cfg(target_os = "windows")]
fn list_drives(_path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
    let mut out = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if !root.is_dir() {
            continue;
        }
        let name = format!("{}:", letter as char);
        let id = file_id_for(&root);
        out.push(ReadDirEntry::new(
            id,
            name,
            mo_core::EntryKind::Directory,
            root,
        ));
    }
    // 探测顺序天然按字母序。
    Ok(out)
}

/// 非 Windows 没有「此电脑」入口（面包屑不会生成空路径段），防御性报错。
#[cfg(not(target_os = "windows"))]
fn list_drives(_path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
    Err(MoError::Other("此电脑视图仅在 Windows 上可用".into()))
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    /// 空路径哨兵必须列出真实存在的盘符：至少有 C:，且每条都是目录、
    /// 指向真实存在的盘符根。
    #[test]
    fn empty_path_lists_existing_drives() {
        let entries = LocalFileSystem
            .read_dir_blocking(Path::new(""))
            .expect("「此电脑」枚举不应失败");
        assert!(!entries.is_empty(), "本机至少应有一个可用盘符");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, {
            let mut sorted = names.clone();
            sorted.sort();
            sorted
        });
        for e in &entries {
            assert!(e.kind.is_dir(), "{e:?} 应是目录");
            assert!(e.path.is_dir(), "{e:?} 指向的盘符根应真实存在");
        }
        assert!(
            entries.iter().any(|e| e.name == "C:"),
            "常规 Windows 环境必有 C:：{names:?}"
        );
    }

    /// 盘符根目录照常走真实读取链路，不受哨兵影响。
    #[test]
    fn drive_root_still_reads_normally() {
        LocalFileSystem
            .read_dir_blocking(Path::new("C:\\"))
            .expect("读取 C:\\ 不应失败");
    }
}
