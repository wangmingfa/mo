use std::io::Write;
use std::path::Path;
#[cfg(target_os = "windows")]
use std::path::PathBuf;

use async_trait::async_trait;
use mo_core::{FileMetadata, MoError, Permissions};

use crate::reader::ReadDirEntry;
use crate::{
    entry_kind_from_path, file_id_for, is_hidden_name, is_hidden_with_metadata, kind_from_metadata,
    to_dir_error, FileSystem,
};

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
            // 一次 `DirEntry::metadata()` 把「类型」和「macOS 隐藏标记位」一起拿
            // 出来：它内部就是 lstat，比按路径再查一次 dentry 便宜（拿不到时退回
            // 按路径判，宁可多一次 syscall 也不要丢条目）。
            let meta = entry.metadata().ok();
            let kind = meta
                .as_ref()
                .map(|m| kind_from_metadata(m, &p))
                .or_else(|| entry_kind_from_path(&p).ok())
                .unwrap_or(mo_core::EntryKind::Other);
            let id = file_id_for(&p);
            let mut r = ReadDirEntry::new(id, name.clone(), kind, p);
            r.hidden = match meta.as_ref() {
                Some(m) => is_hidden_with_metadata(&name, m),
                None => is_hidden_name(&name),
            };
            out.push(r);
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
                hidden: is_hidden(path, &m),
                mode: unix_mode(&m),
            },
        })
    }

    async fn create_dir(&self, path: &Path) -> Result<(), MoError> {
        std::fs::create_dir_all(path).map_err(MoError::Io)
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, MoError> {
        std::fs::read(path).map_err(MoError::Io)
    }

    async fn is_dir(&self, path: &Path) -> bool {
        // 跟随软链（与列目录的语义一致：软链指向目录就当目录）。
        std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
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

/// 一个条目是否「隐藏」。
///
/// 之前这里**写死 `false`**，导致 `.DS_Store`、`.git`、`.ssh` 之类在属性面板里
/// 一律显示「不隐藏」，是 macOS / Unix 上的真 bug。判据：
///
/// * **文件名以 `.` 开头**（dotfile）：所有 unix 系都这么认，跨平台成立；
/// * **macOS 的 `UF_HIDDEN` 属性位**（`chflags hidden` 设的，访达里手动「隐藏」的
///   文件走这条路）：`st_flags & 0x8000`。`0x8000` 是 BSD 的 `UF_HIDDEN`，
///   Linux 的 `st_flags` 恒为 0，所以这条在 Linux 上自动失效、不误伤。
///
/// 属性面板的「隐藏」一栏：与列表过滤共用 [`is_hidden_with_metadata`] 那套判据。
///
/// 两处必须同一套判据，否则会出现「属性面板说不隐藏，列表里却被过滤掉」这种
/// 自相矛盾的显示。
fn is_hidden(path: &Path, m: &std::fs::Metadata) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    is_hidden_with_metadata(&name, m)
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

/// dotfile 必须被报告为隐藏（修「hidden 恒为 false」的真 bug）。
///
/// 直接测 `is_hidden` 这个纯函数（它本就是 private fn，子模块可访问），
/// 绕开 `LocalFileSystem::metadata` 的异步签名——那是个 `Future`，
/// 在同步 `#[test]` 里不能 `.expect()`。
#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;

    /// 指向**同一目标**的两个软链必须有**不同的** FileId（回归：点一个选中两个）。
    ///
    /// `file_id_for` 原来走 stat（跟随软链），两个指向同目标的软链拿到目标的
    /// dev+ino → FileId 撞车 → 选择模型按 FileId 记账，点其中一条两条全亮。
    /// 修后走 lstat，软链以自身 inode 为身份。
    #[test]
    fn symlinks_to_the_same_target_get_distinct_file_ids() {
        let dir = std::env::temp_dir().join(format!("mo-fs-symlink-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");

        let target = dir.join("real-config");
        std::fs::write(&target, b"x").expect("写目标文件");
        let a = dir.join(".bluework-config");
        let b = dir.join(".bluework-ui-config");
        std::os::unix::fs::symlink(&target, &a).expect("建软链 a");
        std::os::unix::fs::symlink(&target, &b).expect("建软链 b");

        let entries = LocalFileSystem.read_dir_blocking(&dir).expect("列目录");
        let id_of = |name: &str| {
            entries
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.id)
                .unwrap_or_else(|| panic!("目录里该有 {name}"))
        };
        let (ida, idb) = (id_of(".bluework-config"), id_of(".bluework-ui-config"));
        assert_ne!(ida, idb, "两个不同 inode 的软链不该共用 FileId");
        assert_ne!(ida, id_of("real-config"), "软链也不该和目标共用 FileId");
        // 断链的软链同样要有稳定 ID（lstat 仍成功），不能退回路径哈希之外的东西。
        std::fs::remove_file(&target).expect("删目标造断链");
        let broken = dir.join(".broken-link");
        std::os::unix::fs::symlink(&target, &broken).expect("建断链软链");
        let entries = LocalFileSystem
            .read_dir_blocking(&dir)
            .expect("断链目录也要能列");
        let broken_entry = entries
            .iter()
            .find(|e| e.name == ".broken-link")
            .expect("断链条目在");
        assert_ne!(broken_entry.id, ida, "断链软链有自己的 inode");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dotfiles_and_flagged_files_are_hidden() {
        let dir = std::env::temp_dir().join("mo-fs-hidden-test");
        std::fs::create_dir_all(&dir).expect("建临时目录");

        // `.DS_Store` 这类 dotfile：在 macOS / Linux 上都算隐藏。
        let dot = dir.join(".ds_store_probe");
        std::fs::write(&dot, b"x").expect("写 dotfile");
        // 普通文件：不该是隐藏。
        let plain = dir.join("visible.txt");
        std::fs::write(&plain, b"x").expect("写普通文件");

        let dot_meta = std::fs::metadata(&dot).expect("读 dotfile 元数据");
        assert!(
            is_hidden(&dot, &dot_meta),
            ".DS_Store 类 dotfile 必须报告隐藏"
        );

        let plain_meta = std::fs::metadata(&plain).expect("读普通文件元数据");
        assert!(!is_hidden(&plain, &plain_meta), "普通文件不应被标记隐藏");

        // macOS 的 `UF_HIDDEN`（`chflags hidden`）：即便名字不以 . 开头也该藏。
        #[cfg(target_os = "macos")]
        {
            let flagged = dir.join("flagged.txt");
            std::fs::write(&flagged, b"x").expect("写待标记文件");
            std::process::Command::new("chflags")
                .args(["hidden", &flagged.to_string_lossy()])
                .status()
                .expect("chflags 应成功");
            let flagged_meta = std::fs::metadata(&flagged).expect("读标记文件元数据");
            assert!(
                is_hidden(&flagged, &flagged_meta),
                "被 chflags hidden 的文件必须报告隐藏"
            );
            std::process::Command::new("chflags")
                .args(["nohidden", &flagged.to_string_lossy()])
                .status()
                .ok();
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 列目录必须把「是否隐藏」跟着条目一起交上来。
    ///
    /// 上层过滤（「显示隐藏文件」开关）只看这个字段。它若丢失，上层就只能拿着
    /// 路径再 stat 一遍——列目录是热路径，两万条的目录就是两万次额外 syscall。
    #[test]
    fn read_dir_reports_hidden_per_entry() {
        let dir = std::env::temp_dir().join(format!("mo-fs-hidden-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        std::fs::write(dir.join(".dot_probe"), b"x").expect("写 dotfile");
        std::fs::write(dir.join("plain.txt"), b"x").expect("写普通文件");
        std::fs::create_dir_all(dir.join(".dot_dir")).expect("写 dot 目录");

        let entries = LocalFileSystem.read_dir_blocking(&dir).expect("读目录");
        let hidden_of = |name: &str| {
            entries
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| panic!("目录里应有 {name}"))
                .hidden
        };
        assert!(hidden_of(".dot_probe"), "dotfile 必须标 hidden");
        assert!(hidden_of(".dot_dir"), "dot 目录同样要标 hidden");
        assert!(!hidden_of("plain.txt"), "普通文件不该标 hidden");

        // 类型判据没被这次改动带偏：`DirEntry::metadata()` 那条路要给出同样的结果。
        let plain = entries.iter().find(|e| e.name == "plain.txt").unwrap();
        assert!(plain.kind.is_file(), "plain.txt 应是文件");
        let dot_dir = entries.iter().find(|e| e.name == ".dot_dir").unwrap();
        assert!(dot_dir.kind.is_dir(), ".dot_dir 应是目录");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
