use mo_core::{EntryKind, FileId};
use std::path::PathBuf;

/// `read_dir` 结果的单个条目：立即可得的轻量信息。
#[derive(Debug, Clone)]
pub struct ReadDirEntry {
    pub id: FileId,
    pub name: String,
    pub kind: EntryKind,
    pub path: PathBuf,
    /// 是否「隐藏」（`.` 开头；macOS 上还包括 `chflags hidden` 的条目）。
    ///
    /// 判据跟着条目一起交上来，而不是让上层拿着路径再去 stat 一遍：列目录是
    /// **热路径**（进目录 / 刷新 / 前进后退 / 列视图切列都会重读），一个两万条
    /// 的目录多两万次 syscall 就是肉眼可见的停顿。用户在界面上切「显示隐藏文件」
    /// 时过滤的是这个字段，不需要重新读盘之外的任何 IO。
    pub hidden: bool,
}

impl ReadDirEntry {
    pub fn new(id: FileId, name: String, kind: EntryKind, path: PathBuf) -> Self {
        let hidden = crate::is_hidden_name(&name);
        Self {
            id,
            name,
            kind,
            path,
            hidden,
        }
    }
}

/// 目录读取器：把 `read_dir` 的结果抽象为可迭代集合。
///
/// 当前一次性返回；未来可改为增量流式，配合渐进式加载
/// （先显示文件名，再后台加载 metadata / 图标 / 缩略图）。
#[derive(Debug, Clone)]
pub struct DirectoryReader {
    entries: Vec<ReadDirEntry>,
}

impl DirectoryReader {
    pub fn new(entries: Vec<ReadDirEntry>) -> Self {
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ReadDirEntry> {
        self.entries.iter()
    }

    pub fn into_entries(self) -> Vec<ReadDirEntry> {
        self.entries
    }
}
