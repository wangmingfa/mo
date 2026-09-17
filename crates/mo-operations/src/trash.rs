//! 回收站：把被删除的文件 / 目录移入隔离目录，并记录可还原的映射。
//!
//! 设计要点：
//! * 每个被回收项放进 `<root>/<uuid>/<原名>`，按 uuid 隔离，避免同名冲突。
//! * 索引（`<root>/index.json`）持久化 `TrashEntry`，进程重启后仍能还原 / 清空。
//! * 还原时按「原路径」回找最近一条记录，因此撤销「删除」无需持有 entry 生命周期。

use std::path::{Path, PathBuf};

use parking_lot::Mutex as PlMutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fs_util::move_path;

/// 一条回收站记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrashEntry {
    /// 隔离目录名（uuid），同时也是还原时的主键。
    pub id: String,
    /// 被删除前的原始路径（还原目标）。
    pub original: PathBuf,
    /// 当前在回收站里的实际路径。
    pub trashed: PathBuf,
    /// 是否为目录。
    pub is_dir: bool,
    /// 入站时刻（Unix 秒）。
    pub at: u64,
}

/// 回收站操作错误。
#[derive(Debug, thiserror::Error)]
pub enum TrashError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 回收站操作结果。
pub type Result<T> = std::result::Result<T, TrashError>;

/// 文件回收站。
///
/// 内部用 `PlMutex` 保护索引，方法均为 `&self` 可变，可安全共享 `Arc<Trash>`。
pub struct Trash {
    root: PathBuf,
    entries: PlMutex<Vec<TrashEntry>>,
}

impl Trash {
    /// 打开（或创建）回收站根目录，并加载持久化索引。
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        let entries = load_index(&root).unwrap_or_default();
        Ok(Self {
            root,
            entries: PlMutex::new(entries),
        })
    }

    /// 回收站根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    fn persist(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&*self.entries.lock())?;
        std::fs::write(self.index_path(), data)?;
        Ok(())
    }

    /// 把 `path` 移入回收站（保留原文件名，按 id 隔离），返回记录。
    ///
    /// 入站使用 `move_path`（rename 快路径，跨设备自动复制 + 删除源）。
    pub fn trash(&self, path: &Path) -> Result<TrashEntry> {
        if !path.exists() {
            return Err(TrashError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("文件不存在：{}", path.display()),
            )));
        }
        let id = Uuid::new_v4().to_string();
        let is_dir = path.is_dir();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| id.clone());
        let trashed = self.root.join(&id).join(&name);
        move_path(path, &trashed)?;

        let entry = TrashEntry {
            id,
            original: path.to_path_buf(),
            trashed,
            is_dir,
            at: now_secs(),
        };
        self.entries.lock().push(entry.clone());
        self.persist()?;
        Ok(entry)
    }

    /// 把某条记录还原回原路径（必要时创建原父目录），并从索引移除。
    pub fn restore(&self, entry: &TrashEntry) -> Result<()> {
        if let Some(parent) = entry.original.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        move_path(&entry.trashed, &entry.original)?;
        // 清理 uuid 隔离目录（可能残留空父）。
        if let Some(id_dir) = entry.trashed.parent() {
            let _ = std::fs::remove_dir_all(id_dir);
        }
        self.remove_entry(&entry.id)
    }

    /// 按原路径还原最近一条记录（用于撤销「删除」）。
    pub fn restore_by_original(&self, original: &Path) -> Result<TrashEntry> {
        let found = {
            let g = self.entries.lock();
            g.iter().rev().find(|e| e.original == original).cloned()
        };
        match found {
            Some(e) => {
                self.restore(&e)?;
                Ok(e)
            }
            None => Err(TrashError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("回收站中没有：{}", original.display()),
            ))),
        }
    }

    /// 当前回收站中的所有记录（最新在前）。
    pub fn list(&self) -> Vec<TrashEntry> {
        self.entries.lock().iter().rev().cloned().collect()
    }

    /// 当前回收站中的条目数。
    pub fn count(&self) -> usize {
        self.entries.lock().len()
    }

    /// 永久删除某一条记录（不还原，直接抹掉文件与索引）。
    pub fn purge(&self, entry: &TrashEntry) -> Result<()> {
        // 删除 uuid 隔离目录，连同其中的被回收文件 / 目录。
        if let Some(id_dir) = entry.trashed.parent() {
            let _ = std::fs::remove_dir_all(id_dir);
        }
        // 兜底（trashed 无父目录等罕见情况）。
        let _ = std::fs::remove_file(&entry.trashed);
        self.remove_entry(&entry.id)
    }

    /// 清空回收站（删除所有被回收的文件 + 索引）。
    pub fn empty(&self) -> Result<()> {
        let ids: Vec<String> = self.entries.lock().iter().map(|e| e.id.clone()).collect();
        for id in ids {
            let _ = std::fs::remove_dir_all(self.root.join(&id));
        }
        self.entries.lock().clear();
        self.persist()
    }

    fn remove_entry(&self, id: &str) -> Result<()> {
        self.entries.lock().retain(|e| e.id != id);
        self.persist()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_index(root: &Path) -> Result<Vec<TrashEntry>> {
    let p = root.join("index.json");
    if !p.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(p)?;
    if data.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&data)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_trash(tag: &str) -> (PathBuf, Trash) {
        let dir = std::env::temp_dir().join(format!(
            "mo-trash-test-{}-{}-{}",
            tag,
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let t = Trash::new(dir.join("trash")).unwrap();
        (dir, t)
    }

    fn file(p: &Path, content: &[u8]) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(content).unwrap();
    }

    #[test]
    fn trash_moves_file_out_of_place() {
        let (root, t) = tmp_trash("a");
        let src = root.join("note.txt");
        file(&src, b"hello");
        let entry = t.trash(&src).unwrap();
        assert!(!src.exists(), "原路径应已消失");
        assert!(entry.trashed.exists(), "回收站里应有文件");
        assert_eq!(entry.original, src);
        t.empty().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_brings_file_back() {
        let (root, t) = tmp_trash("b");
        let src = root.join("note.txt");
        file(&src, b"hello");
        let entry = t.trash(&src).unwrap();
        assert!(!src.exists());
        t.restore(&entry).unwrap();
        assert!(src.exists(), "还原后原路径应恢复");
        assert!(!entry.trashed.exists(), "回收站里应已清理");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_by_original_finds_recent() {
        let (root, t) = tmp_trash("c");
        let src = root.join("doc.md");
        file(&src, b"x");
        t.trash(&src).unwrap();
        assert!(!src.exists());
        let e = t.restore_by_original(&src).unwrap();
        assert_eq!(e.original, src);
        assert!(src.exists(), "按原路径还原应成功");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_and_empty() {
        let (root, t) = tmp_trash("d");
        let a = root.join("a.txt");
        let b = root.join("sub").join("b.txt");
        file(&a, b"a");
        file(&b, b"b");
        t.trash(&a).unwrap();
        t.trash(&b).unwrap();
        assert_eq!(t.list().len(), 2, "应有 2 条记录");
        t.empty().unwrap();
        assert_eq!(t.list().len(), 0, "清空后应为空");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn purge_removes_single_entry_permanently() {
        let (root, t) = tmp_trash("f");
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        file(&a, b"a");
        file(&b, b"b");
        t.trash(&a).unwrap();
        let eb = t.trash(&b).unwrap();
        assert_eq!(t.count(), 2);
        t.purge(&eb).unwrap();
        assert_eq!(t.count(), 1, "永久删除一条后应只剩 1 条");
        assert!(!eb.trashed.exists(), "被永久删除的文件应不存在");
        assert!(!a.exists() && t.list()[0].original == a, "另一条应保留");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trashing_directory_preserves_contents() {
        let (root, t) = tmp_trash("e");
        let src = root.join("folder");
        file(&src.join("inner.txt"), b"deep");
        let entry = t.trash(&src).unwrap();
        assert!(entry.is_dir);
        assert!(entry.trashed.join("inner.txt").exists(), "目录内容应随回收站保留");
        t.restore(&entry).unwrap();
        assert!(src.join("inner.txt").exists(), "还原后目录内容应回来");
        let _ = std::fs::remove_dir_all(&root);
    }
}
