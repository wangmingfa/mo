use std::collections::HashSet;

use crate::file_id::FileId;

/// 选择模型，独立于具体的 `FileItem` 视图。
///
/// 选择状态放在模型里（而不是 `FileItem { selected: bool }`），这样在列表重排、
/// 刷新、watcher 更新后，选择不容易丢失，并且天然支持：
/// 单选 / 多选 / Shift 连选 / Ctrl(Cmd) 多选 / 全选 / 反选 / 批量操作。
#[derive(Debug, Clone, Default)]
pub struct SelectionModel {
    selected: HashSet<FileId>,
    anchor: Option<FileId>,
    focused: Option<FileId>,
}

impl SelectionModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_selected(&self, id: &FileId) -> bool {
        self.selected.contains(id)
    }

    pub fn selected_ids(&self) -> &HashSet<FileId> {
        &self.selected
    }

    pub fn count(&self) -> usize {
        self.selected.len()
    }

    pub fn focused(&self) -> Option<FileId> {
        self.focused
    }

    pub fn set_focused(&mut self, id: FileId) {
        self.focused = Some(id);
    }

    /// 清空选择。
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.focused = None;
    }

    /// 单选（替换当前选择）。
    pub fn select(&mut self, id: FileId) {
        self.selected.clear();
        self.selected.insert(id);
        self.anchor = Some(id);
        self.focused = Some(id);
    }

    /// 切换某个条目的选择（Ctrl / Cmd 多选）。
    pub fn toggle(&mut self, id: FileId) {
        if !self.selected.insert(id) {
            self.selected.remove(&id);
        }
        self.anchor.get_or_insert(id);
        self.focused = Some(id);
    }

    /// 选择一段（Shift 连选），基于 `ordered` 中的索引。
    pub fn select_range(&mut self, ordered: &[FileId], from: usize, to: usize) {
        let (a, b) = if from <= to { (from, to) } else { (to, from) };
        for id in ordered.get(a..=b).unwrap_or(&[]) {
            self.selected.insert(*id);
        }
        self.focused = ordered.get(b).copied();
    }

    /// 全选。
    pub fn select_all(&mut self, ids: &[FileId]) {
        self.selected = ids.iter().copied().collect();
        self.anchor = ids.first().copied();
        self.focused = ids.last().copied();
    }
}
