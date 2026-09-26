use std::path::PathBuf;
use std::sync::Arc;

use crate::bitmap::Bitmap;
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
/// 缩略图由 mo-thumbnails 在后台解码（磁盘缓存命中则读缓存，未命中解码原图），
/// 最终以**解码好的内存位图**（[`Bitmap`]，BGRA）交付——UI 直接用它同步上屏，
/// 不必再走「磁盘路径 → 异步读盘解码」那一遭（`img(path)` 缓存未命中时那一格
/// 什么都不画，切目录时的图标闪烁正是这么来的）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailState {
    /// 还没请求过。
    Idle,
    /// 正在后台生成。
    Loading,
    /// 已生成，值是解码好的内存位图。
    Loaded(Arc<Bitmap>),
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

    /// 给用户看的名字（见 [`display_name`]）。
    pub fn display_name(&self) -> &str {
        display_name(&self.name)
    }
}

/// 给用户看的名字：Windows 上快捷方式不露 `.lnk`。
///
/// 资源管理器把 `.lnk` 当作「这是一条链接」的记号、而不是文件类型后缀，从不显示
/// 它——一屏幕 `Atlas.lnk / PotPlayer.lnk / …` 读起来全是噪音。**只管渲染**：真实
/// 名字（`Entry::name` / `path`）一个字都不动，改名、删除、打开、搜索走的都是原名，
/// 所以这里剥掉的后缀不会让用户以为文件叫那个名字。
///
/// 只有 Windows 有这回事：macOS 的快捷方式是 `.app` 包或 Finder alias（`.alias`
/// 后缀在访达里同样是隐藏的，但 Mo 里那类条目按包/目录走，不在这条判据里）。
pub fn display_name(name: &str) -> &str {
    #[cfg(target_os = "windows")]
    {
        if let Some((stem, ext)) = name.rsplit_once('.') {
            if !stem.is_empty() && ext.eq_ignore_ascii_case("lnk") {
                return stem;
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = name;
    name
}

/// 把「编辑框里的名字」还原成真实文件名——[`display_name`] 的反向操作。
///
/// 资源管理器的改名框把 `.lnk` 划在可编辑区之外：框里显示 `Atlas`，改成 `Atlas2`
/// 之后文件仍叫 `Atlas2.lnk`。Mo 的改名框照这个来，所以提交时要补回后缀。三条边界：
///
/// * **只有原名确实是快捷方式才补**——普通文件 `notes.txt` 改成 `notes` 就是真的
///   去掉后缀（那是用户自己的意图，别拦）；
/// * 用户自己把后缀写全了（`Atlas2.lnk`）就不重复补；
/// * 补的是**原名那份后缀的写法**（原名 `Atlas.LNK` → `Atlas2.LNK`），跟资源管理器
///   一致，不擅自统一成小写。
#[cfg(target_os = "windows")]
pub fn name_after_edit(edited: &str, old_real: &str) -> String {
    let Some((_, old_ext)) = old_real.rsplit_once('.') else {
        return edited.to_string();
    };
    if old_ext.eq_ignore_ascii_case("lnk") {
        // 编辑结果已经带 `.lnk`（大小写任一）→ 原样用，别再补一遍。
        let already = edited
            .rsplit_once('.')
            .is_some_and(|(_, e)| e.eq_ignore_ascii_case("lnk"));
        if !already {
            // `old_real.len() - old_ext.len() - 1` = 那个点的下标；点是 ASCII，
            // 落在字符边界上。
            return format!(
                "{edited}{}",
                &old_real[old_real.len() - old_ext.len() - 1..]
            );
        }
    }
    edited.to_string()
}

/// 非 Windows：没有「隐藏的后缀」这回事，编辑框里是什么就是什么。
#[cfg(not(target_os = "windows"))]
pub fn name_after_edit(edited: &str, _old_real: &str) -> String {
    edited.to_string()
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

#[cfg(test)]
mod tests {
    use super::{display_name, name_after_edit};

    /// 快捷方式的后缀只在 Windows 上藏起来。
    #[test]
    fn shortcut_suffix_is_hidden_only_on_windows() {
        #[cfg(target_os = "windows")]
        {
            assert_eq!(display_name("Atlas.lnk"), "Atlas");
            assert_eq!(
                display_name("迅雷.LNK"),
                "迅雷",
                "大小写与非 ASCII 名字都要认"
            );
            assert_eq!(display_name(".lnk"), ".lnk", "本体名字为空就不算快捷方式");
            assert_eq!(display_name("notes.txt"), "notes.txt", "普通文件不动");
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(display_name("Atlas.lnk"), "Atlas.lnk");
        }
    }

    /// 改名框只编辑本体名，提交时后缀要补回去（[`display_name`] 的反向）。
    #[test]
    fn editing_a_shortcut_keeps_the_link_suffix() {
        #[cfg(target_os = "windows")]
        {
            assert_eq!(name_after_edit("Atlas2", "Atlas.lnk"), "Atlas2.lnk");
            assert_eq!(
                name_after_edit("Atlas.lnk", "Atlas.lnk"),
                "Atlas.lnk",
                "写全了别补两遍"
            );
            assert_eq!(
                name_after_edit("Atlas2.LNK", "Atlas.lnk"),
                "Atlas2.LNK",
                "用户写的大写尊重之"
            );
            assert_eq!(
                name_after_edit("Atlas2", "Atlas.LNK"),
                "Atlas2.LNK",
                "补的是原名那份写法"
            );
            // 普通文件不在这条规则里：去掉后缀是用户自己的意图。
            assert_eq!(name_after_edit("notes", "notes.txt"), "notes");
            assert_eq!(name_after_edit("LICENSE", "LICENSE"), "LICENSE");
            // 中文名 / 只输入了空串（空串由调用方挡掉，这里只求不炸）。
            assert_eq!(name_after_edit("迅雷", "迅雷.lnk"), "迅雷.lnk");
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(name_after_edit("Atlas2", "Atlas.lnk"), "Atlas2");
        }
    }
}
