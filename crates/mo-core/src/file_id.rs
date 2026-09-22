use std::hash::{Hash, Hasher};
use std::path::Path;

/// 平台无关的文件唯一标识。
///
/// **不要用 `Path` 作为文件唯一 ID**：rename / move 会改变路径，但文件本身不变。
/// 内部状态应始终以 `FileId -> Entry -> Path` 的方向组织，这样之后的
/// rename、move、watcher、selection、cache、thumbnail、操作历史都会更容易处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileId {
    /// 卷 / 设备标识（unix 上为 `st_dev`，跨平台可填 0）。
    pub volume: u64,
    /// 文件系统内的稳定 ID（unix 上为 `st_ino`）。
    pub id: u128,
}

impl FileId {
    pub fn new(volume: u64, id: u128) -> Self {
        Self { volume, id }
    }

    /// 尚无法获取稳定 ID 时的兜底（例如尚未 stat 的占位项）。
    ///
    /// 注意：这是基于路径哈希的占位 ID，rename 后会变化，仅用于初始化阶段，
    /// 真正稳定 ID 应由 `mo-fs` 在读取目录时填充。
    pub fn synthetic(path: &Path) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut h);
        let lo = h.finish();
        path.hash(&mut h);
        let hi = h.finish();
        Self {
            volume: 0,
            id: ((hi as u128) << 64) | (lo as u128),
        }
    }

    /// 拿这个 ID 当**文件名**时用的键（缩略图 / 预览缓存的落盘文件名）。
    ///
    /// ⚠️ 不能用 [`Display`]：它的分隔符是 `:`，而冒号在 Windows 上是文件名非法
    /// 字符（NTFS 拿它表示盘符与备用数据流），`CreateFile` 直接回
    /// `ERROR_INVALID_PARAMETER`（os error 87）——缩略图缓存因此在 Windows 上整个
    /// 写不下去。这里只用十进制数字加一个 `-`，三个平台都能当文件名。
    /// 两段都是纯数字，`-` 分隔不存在歧义（不会有两个 ID 撞成同一个键）。
    pub fn cache_key(&self) -> String {
        format!("{}-{}", self.volume, self.id)
    }
}

impl std::fmt::Display for FileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.volume, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cache_key` 必须能直接当文件名：Windows 的非法字符一个都不许出现。
    ///
    /// 这条断言的由来：缓存文件名原来用 `Display`（`volume:id`），冒号在 Windows
    /// 上非法，`CreateFile` 回 `ERROR_INVALID_PARAMETER`（os error 87），
    /// 缩略图与预览降采样在 Windows 上整个写不下去。
    #[test]
    fn cache_key_is_usable_as_a_file_name() {
        const ILLEGAL: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
        let ids = [
            FileId::new(0, 0),
            FileId::new(1, 7),
            FileId::new(u64::MAX, u128::MAX),
            FileId::synthetic(Path::new("/tmp/照片.png")),
        ];
        for id in ids {
            let key = id.cache_key();
            assert!(!key.is_empty(), "键不该是空的");
            for c in ILLEGAL {
                assert!(!key.contains(c), "cache_key {key:?} 含非法文件名字符 {c:?}");
            }
            assert!(
                !key.chars().any(|c| (c as u32) < 32) && !key.ends_with('.') && !key.ends_with(' '),
                "cache_key {key:?} 含控制字符或以点 / 空格结尾（Windows 也不接受）"
            );
            // 对照组：`Display` 带冒号，正是它在 Windows 撑出 os error 87。
            assert!(id.to_string().contains(':'), "Display 形态变了？");
        }
    }

    /// 两段都是纯数字，`-` 分隔不会让两个不同 ID 撞成同一个键。
    #[test]
    fn cache_key_distinguishes_ids_and_is_stable() {
        assert_eq!(FileId::new(1, 23).cache_key(), "1-23");
        assert_ne!(
            FileId::new(1, 23).cache_key(),
            FileId::new(12, 3).cache_key(),
            "数字里插个分隔符就撞键，缓存会互相覆盖"
        );
        assert_eq!(
            FileId::new(1, 23).cache_key(),
            FileId::new(1, 23).cache_key(),
            "同一个 ID 每次应当给出同一个键"
        );
    }
}
