use std::path::PathBuf;

use crate::file_id::FileId;
use crate::metadata::FileMetadata;

/// 条目类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

impl EntryKind {
    pub fn is_dir(&self) -> bool {
        matches!(self, EntryKind::Directory)
    }
    pub fn is_file(&self) -> bool {
        matches!(self, EntryKind::File)
    }
}

/// 元数据的加载状态。
///
/// 打开目录后立即得到 `name / kind / path`；`metadata` / `thumbnail` 在后台异步加载。
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MetadataState {
    Loading,
    Loaded(FileMetadata),
    Failed(String),
}

/// 缩略图的加载状态。
///
/// 缩略图由 mo-thumbnails 在后台解码生成，落盘到缓存目录；
/// 这里只持有**缓存文件的路径**，避免让领域层依赖任何 UI 框架的图片类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailState {
    /// 还没请求过。
    Idle,
    /// 正在后台生成。
    Loading,
    /// 已生成，值是磁盘缓存中的图片路径。
    Loaded(PathBuf),
    /// 生成失败（解码失败 / IO 错误）。
    Failed,
    /// 该类型不支持缩略图（如文件夹、文本）。
    NotApplicable,
}

impl ThumbnailState {
    pub fn is_available(&self) -> bool {
        matches!(self, ThumbnailState::Loaded(_))
    }
}

/// 目录中的一个条目（文件或文件夹）。
///
/// 设计上把"立即可得"与"后台加载"的字段分开：
///
/// ```text
/// Entry
///  ├── name       → 立即得到
///  ├── kind       → 立即得到
///  ├── path       → 立即得到
///  ├── metadata   → 后台加载
///  ├── thumbnail  → 后台加载
///  └── preview    → 按需加载（后续阶段）
/// ```
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: FileId,
    pub name: String,
    pub kind: EntryKind,
    pub path: PathBuf,
    pub metadata: MetadataState,
    pub thumbnail: ThumbnailState,
}

impl Entry {
    pub fn new(id: FileId, name: String, kind: EntryKind, path: PathBuf) -> Self {
        Self {
            id,
            name,
            kind,
            path,
            metadata: MetadataState::Loading,
            thumbnail: ThumbnailState::Idle,
        }
    }

    /// 文件大小（元数据尚未加载时返回 0，供排序使用）。
    pub fn size_or_zero(&self) -> u64 {
        match &self.metadata {
            MetadataState::Loaded(m) => m.size,
            _ => 0,
        }
    }

    /// 修改时间的秒数（元数据尚未加载时返回 0，供排序使用）。
    pub fn modified_or_zero(&self) -> u64 {
        match &self.metadata {
            MetadataState::Loaded(m) => m
                .modified
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            _ => 0,
        }
    }

    /// 小写后缀（用于按类型排序），无后缀返回空串。
    pub fn extension(&self) -> String {
        image_extension(&self.name).unwrap_or_default()
    }

    /// 该条目是否值得生成缩略图（目前只处理图片）。
    pub fn supports_thumbnail(&self) -> bool {
        if !self.kind.is_file() {
            return false;
        }
        matches!(
            image_extension(&self.name).as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff")
        )
    }
}

/// 取文件名后缀的小写形式（不含点）。
pub fn image_extension(name: &str) -> Option<String> {
    let ext = name.rsplit_once('.').map(|(_, e)| e)?;
    if ext.is_empty() || ext.len() > 5 {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// 轻量条目：只有 UI 渲染一列所需要的最小信息（列视图用）。
///
/// 与 [`Entry`] 的区别：不带 `FileId` / 元数据 / 缩略图状态——
/// 列视图直接读盘拿名字和类型即可，没必要塞进主目录模型。
#[derive(Debug, Clone)]
pub struct LightEntry {
    pub name: String,
    pub kind: EntryKind,
    pub path: PathBuf,
}
