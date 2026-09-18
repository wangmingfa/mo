use std::time::SystemTime;

/// 文件的元数据快照。
///
/// 与 `read_dir` 解耦：目录读取后只拿到 `Entry`（name/kind/path），
/// 真正的 `metadata` 由 `mo-metadata`（或当前阶段由 `mo-app` 的后台调度器）异步加载。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileMetadata {
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub permissions: Permissions,
}

/// 权限与可见性摘要（完整权限编辑在后续阶段由 `mo-platform` 承载）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Permissions {
    pub readonly: bool,
    pub hidden: bool,
    /// unix 权限位（低 9 位，如 0o644）；非 unix 平台恒为 0。
    pub mode: u32,
}
