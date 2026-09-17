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

    /// Shift 连选的起点。
    pub fn anchor(&self) -> Option<FileId> {
        self.anchor
    }

    /// 用快照整体替换选择集（UI 从 `AppState` 同步选择时用）。
    ///
    /// 只替换 `selected`，不动 anchor / focused——它们描述的是键盘光标语义，
    /// 不属于「哪些条目被选中」这份事实。
    pub fn set_from(&mut self, ids: &[FileId]) {
        self.selected = ids.iter().copied().collect();
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

#[cfg(test)]
mod tests {
    use crate::file_id::FileId;
    use crate::selection::SelectionModel;

    fn id(gen: u64) -> FileId {
        FileId::new(gen, 1)
    }

    #[test]
    fn set_from_replaces_selection_but_keeps_cursor_semantics() {
        let mut m = SelectionModel::new();
        m.select(id(1));
        m.toggle(id(2));

        // 快照里只剩 2：选择集被替换，anchor / focused 不被清除——
        // 它们描述键盘光标语义，不属于「哪些条目被选中」这份事实。
        m.set_from(&[id(2)]);
        assert!(m.is_selected(&id(2)));
        assert!(!m.is_selected(&id(1)));
        assert_eq!(m.count(), 1);
        assert_eq!(m.anchor(), Some(id(1)));
        assert_eq!(m.focused(), Some(id(2)));
    }

    #[test]
    fn anchor_survives_select_and_toggle() {
        let mut m = SelectionModel::new();
        assert_eq!(m.anchor(), None);
        m.select(id(3));
        assert_eq!(m.anchor(), Some(id(3)));
        m.toggle(id(4));
        // toggle 不覆盖已有 anchor（get_or_insert 语义）。
        assert_eq!(m.anchor(), Some(id(3)));
    }
}
