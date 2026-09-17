use mo_core::{EntryKind, FileId};
use std::path::PathBuf;

/// `read_dir` 结果的单个条目：立即可得的轻量信息。
#[derive(Debug, Clone)]
pub struct ReadDirEntry {
    pub id: FileId,
    pub name: String,
    pub kind: EntryKind,
    pub path: PathBuf,
}

impl ReadDirEntry {
    pub fn new(id: FileId, name: String, kind: EntryKind, path: PathBuf) -> Self {
        Self {
            id,
            name,
            kind,
            path,
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
