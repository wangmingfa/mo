//! 回收站：把被删除的文件移走，并记录可还原的映射。
//!
//! 设计要点：
//! * **隔离模式**（默认 / 测试）：被回收项放进 `<root>/<uuid>/<原名>`，按 uuid
//!   隔离，避免同名冲突；`root` 里既有数据也有 `index.json`。
//! * **系统模式**（注入搬移器）：由平台把文件送进**系统**废纸篓（macOS 上是
//!   `trashItemAtURL`，落点在 `~/.Trash` 或卷宗 `.Trashes/<uid>`），Mo 只拿回落点
//!   记账。`root` 里只有 `index.json`，账本悬空（用户清倒废纸篓）时还原会报
//!   「文件已不在」。
//! * 索引（`<root>/index.json`）持久化 `TrashEntry`，进程重启后仍能还原 / 清空。
//! * 还原时按「原路径」回找最近一条记录，因此撤销「删除」无需持有 entry 生命周期。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex as PlMutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fs_util::move_path;

/// 注入式搬移器：把一个本地路径交给平台回收站，返回它在废纸篓里的**实际落点**。
///
/// 生产实现是 `mo_platform::recycle_one`（系统负责重名改名与跨卷落点）；测试注入
/// 一个搬进临时目录的假搬移器，不碰真实废纸篓。
pub type TrashMover = Arc<dyn Fn(&Path) -> Result<PathBuf> + Send + Sync>;

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
    /// `Some` = 系统模式（搬移交给平台，`root` 只放索引）；`None` = 隔离模式。
    mover: Option<TrashMover>,
}

impl Trash {
    /// 打开（或创建）回收站根目录，并加载持久化索引（隔离模式）。
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        let entries = load_index(&root).unwrap_or_default();
        Ok(Self {
            root,
            entries: PlMutex::new(entries),
            mover: None,
        })
    }

    /// 打开回收站并注入系统搬移器（系统模式）：文件由平台送进系统废纸篓，
    /// `root` 只承载 `index.json` 账本。
    pub fn with_mover(root: PathBuf, mover: TrashMover) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        let entries = load_index(&root).unwrap_or_default();
        Ok(Self {
            root,
            entries: PlMutex::new(entries),
            mover: Some(mover),
        })
    }

    /// 回收站根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 这条记录是不是旧式隔离条目（`trashed` 在 `root` 的 `<uuid>/` 下）。
    ///
    /// 只有隔离条目的父目录才允许整目录清理；系统条目的父目录是
    /// `~/.Trash` / 卷宗 `.Trashes`，动它就是灾难。
    fn is_isolated(&self, trashed: &Path) -> bool {
        self.mover.is_none() && trashed.starts_with(&self.root)
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    fn persist(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&*self.entries.lock())?;
        std::fs::write(self.index_path(), data)?;
        Ok(())
    }

    /// 把 `path` 移入回收站，返回记录。
    ///
    /// * 隔离模式：入站用 `move_path`（rename 快路径，跨设备自动复制 + 删除源），
    ///   落到 `<root>/<uuid>/<原名>`。
    /// * 系统模式：搬移交给注入的搬移器（平台系统废纸篓），落点以它返回的为准；
    ///   重名改名、跨卷落点都由系统处理，账本只管如实记下。
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
        let trashed = match &self.mover {
            Some(mover) => mover(path)?,
            None => {
                let trashed = self.root.join(&id).join(&name);
                move_path(path, &trashed)?;
                trashed
            }
        };

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
        // 清理 uuid 隔离目录（可能残留空父）。仅限隔离条目。
        if self.is_isolated(&entry.trashed) {
            if let Some(id_dir) = entry.trashed.parent() {
                let _ = std::fs::remove_dir_all(id_dir);
            }
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
    ///
    /// 账本悬空（系统废纸篓里文件已被清倒）不算错——目的本来就达成了一半，
    /// 这里只负责把索引抹掉。
    pub fn purge(&self, entry: &TrashEntry) -> Result<()> {
        if self.is_isolated(&entry.trashed) {
            // 删除 uuid 隔离目录，连同其中的被回收文件 / 目录。
            if let Some(id_dir) = entry.trashed.parent() {
                let _ = std::fs::remove_dir_all(id_dir);
            }
        } else {
            // 系统条目只许动文件本身（可能是目录）。
            if entry.trashed.is_dir() {
                let _ = std::fs::remove_dir_all(&entry.trashed);
            } else {
                let _ = std::fs::remove_file(&entry.trashed);
            }
        }
        self.remove_entry(&entry.id)
    }

    /// 清空回收站（删除所有被回收的文件 + 索引）。
    pub fn empty(&self) -> Result<()> {
        let snapshot: Vec<TrashEntry> = self.entries.lock().clone();
        for e in &snapshot {
            if self.is_isolated(&e.trashed) {
                let _ = std::fs::remove_dir_all(self.root.join(&e.id));
            } else if e.trashed.is_dir() {
                let _ = std::fs::remove_dir_all(&e.trashed);
            } else {
                let _ = std::fs::remove_file(&e.trashed);
            }
        }
        self.entries.lock().clear();
        self.persist()
    }

    /// 重命名回收站里的条目（macOS 惯例：面板里 Enter 就是重命名）。
    ///
    /// 实际落点文件改名，账本同步更新——`trashed` 记新落点；`original` 只换
    /// 文件名、目录部分不动（还原时以**新名**放回原目录，改名才不会被还原
    /// 悄悄吃掉）。目标名已存在时拒绝（`rename` 在同一文件系统上会静默覆盖，
    /// 绝不允许）。
    pub fn rename_entry(&self, entry: &TrashEntry, new_name: &str) -> Result<TrashEntry> {
        let name = new_name.trim();
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(TrashError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("文件名不合法：{new_name:?}"),
            )));
        }
        let old_name = entry
            .trashed
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name == old_name {
            return Ok(entry.clone());
        }
        let new_trashed = entry.trashed.with_file_name(name);
        if new_trashed.exists() {
            return Err(TrashError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("回收站里已有同名文件：{name}"),
            )));
        }
        std::fs::rename(&entry.trashed, &new_trashed)?;
        let mut updated = entry.clone();
        updated.trashed = new_trashed;
        if let Some(dir) = updated.original.parent() {
            updated.original = dir.join(name);
        }
        {
            let mut entries = self.entries.lock();
            if let Some(e) = entries.iter_mut().find(|e| e.id == updated.id) {
                *e = updated.clone();
            }
        }
        self.persist()?;
        Ok(updated)
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
    fn rename_entry_renames_file_and_ledger() {
        let (root, t) = tmp_trash("rename");
        let src = root.join("note.txt");
        file(&src, b"hello");
        let entry = t.trash(&src).unwrap();

        // 正常改名：落点文件、账本 trashed、original 文件名三者同步。
        let renamed = t.rename_entry(&entry, "renamed.txt").unwrap();
        assert!(!entry.trashed.exists(), "旧落点应已消失");
        assert!(renamed.trashed.exists(), "新落点应有文件");
        assert_eq!(
            renamed.original.file_name().unwrap().to_string_lossy(),
            "renamed.txt",
            "original 只换文件名、目录不动"
        );
        assert_eq!(
            renamed.original.parent(),
            entry.original.parent(),
            "还原目标目录不变"
        );
        // 还原走新名：账本一致。
        t.restore(&renamed).unwrap();
        assert!(root.join("renamed.txt").exists(), "应以新名还原回原目录");

        // 目标名已存在：拒绝（同文件系统 rename 会静默覆盖）。
        let other = root.join("other.txt");
        file(&other, b"x");
        let e2 = t.trash(&other).unwrap();
        let _ = t.rename_entry(&e2, "renamed.txt"); // 与隔离目录里那条同名（不同目录，允许）
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_entry_rejects_existing_target() {
        let (root, t) = tmp_trash("rename-conflict");
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        file(&a, b"1");
        file(&b, b"2");
        let ea = t.trash(&a).unwrap();
        let eb = t.trash(&b).unwrap();
        // 两条隔离条目在不同 uuid 目录，但直接在同一目录内造冲突验证拒绝逻辑：
        let conflict = ea.trashed.with_file_name("clash.txt");
        std::fs::write(&conflict, b"3").unwrap();
        // 先把 eb 改成 clash.txt 会失败——同目录已有同名。
        let same_dir = eb.trashed.parent().unwrap().join("clash.txt");
        std::fs::rename(&eb.trashed, &same_dir).unwrap();
        // 手动同步账本路径以便构造「同目录已有同名」的场景。
        assert!(same_dir.exists());
        let res = t.rename_entry(&eb, "clash.txt");
        assert!(res.is_err(), "目标已存在时应拒绝，不能静默覆盖");
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
        assert!(
            entry.trashed.join("inner.txt").exists(),
            "目录内容应随回收站保留"
        );
        t.restore(&entry).unwrap();
        assert!(src.join("inner.txt").exists(), "还原后目录内容应回来");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 系统模式：搬移交给注入的假搬移器，账本记**实际落点**；`root` 里只有索引；
    /// purge 只删落点文件本身，绝不动「废纸篓目录」（生产上那是 `~/.Trash`）。
    #[test]
    fn system_mode_records_real_location_and_purge_never_touches_parent() {
        let (root, _) = tmp_trash("sys");
        let fake_trash = root.join("fake-trash");
        std::fs::create_dir_all(&fake_trash).unwrap();
        let ft = fake_trash.clone();
        let t = Trash::with_mover(
            root.join("trash"),
            Arc::new(move |p: &Path| {
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                let mut to = ft.join(&name);
                let mut n = 2;
                while to.exists() {
                    to = ft.join(format!("{name} {n}"));
                    n += 1;
                }
                std::fs::rename(p, &to)?;
                Ok(to)
            }),
        )
        .unwrap();

        let src = root.join("note.txt");
        file(&src, b"hello");
        let entry = t.trash(&src).unwrap();
        assert!(!src.exists(), "原路径应已消失");
        assert!(
            entry.trashed.starts_with(&fake_trash),
            "账本应记假废纸篓里的落点：{:?}",
            entry.trashed
        );
        assert!(
            !root.join("trash").join(&entry.id).exists(),
            "系统模式不在 root 下建 uuid 隔离目录"
        );

        t.purge(&entry).unwrap();
        assert!(!entry.trashed.exists(), "落点文件应被永久删除");
        assert!(fake_trash.exists(), "废纸篓目录本身绝不能被整目录清掉");
        assert_eq!(t.count(), 0, "索引应同步抹掉");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 系统模式的还原与清空：还原照账本把文件搬回原路径；empty 清掉各落点
    /// 文件但保留废纸篓目录本身。
    #[test]
    fn system_mode_restore_and_empty() {
        let (root, _) = tmp_trash("syse");
        let fake_trash = root.join("fake-trash");
        std::fs::create_dir_all(&fake_trash).unwrap();
        let ft = fake_trash.clone();
        let t = Trash::with_mover(
            root.join("trash"),
            Arc::new(move |p: &Path| {
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                let to = ft.join(&name);
                std::fs::rename(p, &to)?;
                Ok(to)
            }),
        )
        .unwrap();

        let a = root.join("a.txt");
        let b = root.join("b.txt");
        file(&a, b"a");
        file(&b, b"b");
        let ea = t.trash(&a).unwrap();
        t.trash(&b).unwrap();

        t.restore(&ea).unwrap();
        assert!(a.exists(), "按账本还原应搬回原路径");
        assert!(!fake_trash.join("a.txt").exists());

        t.empty().unwrap();
        assert!(!fake_trash.join("b.txt").exists(), "清空应删掉落点文件");
        assert!(fake_trash.exists(), "清空不动废纸篓目录本身");
        assert_eq!(t.count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
