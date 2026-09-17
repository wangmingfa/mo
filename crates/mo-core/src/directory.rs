use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::entry::Entry;
use crate::file_id::FileId;
use crate::view::{DirectoryView, SortKey};

/// 目录的唯一 ID（复用 `FileId`）。
pub type DirectoryId = FileId;

/// 一个目录的视图模型。
///
/// 不要简单地用 `Vec<PathBuf>` 表示一个目录——把 `id / path / entries / loading / error`
/// 一起建模，才能支撑渐进式加载、watcher 增量更新与缓存。
#[derive(Debug, Clone)]
pub struct Directory {
    pub id: DirectoryId,
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    /// 排序 / 过滤视图：只存下标，不复制条目。
    pub view: DirectoryView,
    /// `FileId → entries 下标` 的索引。
    ///
    /// 元数据 / 缩略图是**逐条**回填的，若每次都线性扫描，
    /// 一万条目就是一亿次比较（O(n²)）。索引让回填变成 O(1)。
    /// 直接改 `entries` 会让索引失效，增删请走 [`Self::push_entry`] / [`Self::remove_entry`]。
    index: HashMap<FileId, usize>,
    pub loading: bool,
    pub error: Option<DirectoryError>,
}

impl Directory {
    pub fn new(id: DirectoryId, path: PathBuf) -> Self {
        Self {
            id,
            path,
            entries: Vec::new(),
            view: DirectoryView::new(),
            index: HashMap::new(),
            loading: true,
            error: None,
        }
    }

    pub fn entry_index(&self, id: FileId) -> Option<usize> {
        self.index.get(&id).copied()
    }

    /// 按 id 取可变引用（O(1)）。
    pub fn entry_mut(&mut self, id: FileId) -> Option<&mut Entry> {
        let i = *self.index.get(&id)?;
        self.entries.get_mut(i)
    }

    /// 按路径取可变引用（watcher 事件用，O(n) 但事件量很小）。
    pub fn entry_mut_by_path(&mut self, path: &Path) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.path == path)
    }

    /// 整体替换条目列表（打开目录时用），并重建索引与视图。
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.rebuild_index();
        self.rebuild_view();
    }

    /// 追加一个条目，并维护索引与视图。
    pub fn push_entry(&mut self, entry: Entry) {
        self.index.insert(entry.id, self.entries.len());
        self.entries.push(entry);
        self.rebuild_view();
    }

    /// 移除一个条目（按路径），并重建索引与视图。
    pub fn remove_entry(&mut self, path: &Path) -> bool {
        let Some(pos) = self.entries.iter().position(|e| e.path == path) else {
            return false;
        };
        self.entries.remove(pos);
        self.rebuild_index();
        self.rebuild_view();
        true
    }

    /// 重建 id → 下标索引。
    pub fn rebuild_index(&mut self) {
        self.index = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id, i))
            .collect();
    }

    /// 当前可见的条目（已加载、未出错）。
    pub fn visible_entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 条目变动（增删改）后重建排序 / 过滤索引。
    pub fn rebuild_view(&mut self) {
        self.view.rebuild(&self.entries);
    }

    /// 设置过滤词（`None` 或空白表示不过滤）。
    pub fn set_filter(&mut self, query: Option<String>) {
        self.view.set_filter(query, &self.entries);
    }

    /// 设置排序方式。
    pub fn set_sort(&mut self, key: SortKey) {
        self.view.set_sort(key, &self.entries);
    }

    /// 可见条目数量（虚拟化列表用它作 `item_count`）。
    pub fn visible_count(&self) -> usize {
        self.view.len()
    }

    /// 第 `i` 个可见条目。
    ///
    /// 虚拟化列表用它取行，过滤态下依然只渲染可见区，且不克隆整份列表。
    pub fn visible_entry(&self, i: usize) -> Option<&Entry> {
        self.view.index_at(i).and_then(|idx| self.entries.get(idx))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryError {
    NotFound,
    NotADirectory,
    PermissionDenied,
    IoError(String),
}
