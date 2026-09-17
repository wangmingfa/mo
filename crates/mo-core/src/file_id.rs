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
}

impl std::fmt::Display for FileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.volume, self.id)
    }
}
