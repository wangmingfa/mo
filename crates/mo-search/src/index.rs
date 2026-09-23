use std::path::{Path, PathBuf};

use mo_core::EntryKind;
use rusqlite::{params, Connection};

/// 一条搜索命中。
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// 修改时间（秒），索引阶段可能为空。
    pub modified: Option<u64>,
}

/// 索引错误（目前只有 SQLite 层）。
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("sqlite error: {0}")]
    Sql(#[from] rusqlite::Error),
}

/// 文件名 / 路径索引，基于 SQLite。
///
/// 只索引「搜索需要」的字段：小写文件名、整路径、大小、修改时间、是否目录。
/// 全局搜索据此做子串匹配与相关度排序，而不必每次重新遍历整个文件系统。
///
/// 与 [`mo_core::DirectoryView`] 的区别：目录内的「输入即过滤」不需要索引，
/// 直接对当前目录条目做内存子串匹配即可；这里的索引是为**跨目录 / 全局**搜索准备的。
pub struct FileIndex {
    conn: Connection,
}

impl FileIndex {
    /// 打开磁盘上的索引库（不存在则建表）。
    pub fn open(path: &Path) -> Result<Self, SearchError> {
        let conn = Connection::open(path)?;
        // 两个「应用实例」短暂并存时（多标签页 / 测试里先后建两个 AppState），
        // 对端可能正握着写事务；没有 busy 超时的话 open / 建表会立刻拿到
        // SQLITE_BUSY，上层只能退回内存索引——表现为「重开应用后索引归零」。
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        let idx = Self { conn };
        idx.init()?;
        Ok(idx)
    }

    /// 内存索引（测试与一次性搜索用）。
    pub fn open_in_memory() -> Result<Self, SearchError> {
        let conn = Connection::open_in_memory()?;
        let idx = Self { conn };
        idx.init()?;
        Ok(idx)
    }

    fn init(&self) -> Result<(), SearchError> {
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                name_lower TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified INTEGER NOT NULL,
                is_dir INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_name ON files(name_lower);
            CREATE INDEX IF NOT EXISTS idx_path ON files(path);
            CREATE TABLE IF NOT EXISTS indexed_roots (
                root TEXT PRIMARY KEY,
                at INTEGER NOT NULL
            );",
        )?;
        Ok(())
    }

    /// 插入或更新一条记录（路径唯一，冲突即覆盖）。
    pub fn upsert(
        &mut self,
        path: &Path,
        name: &str,
        size: u64,
        modified: Option<u64>,
        is_dir: bool,
    ) -> Result<(), SearchError> {
        let p = path.to_string_lossy().to_string();
        let name_lower = name.to_lowercase();
        self.conn.execute(
            "INSERT INTO files (path, name, name_lower, size, modified, is_dir)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
               name=excluded.name, name_lower=excluded.name_lower, size=excluded.size,
               modified=excluded.modified, is_dir=excluded.is_dir",
            params![
                p,
                name,
                name_lower,
                size,
                modified.unwrap_or(0) as i64,
                is_dir as i32
            ],
        )?;
        Ok(())
    }

    /// 删除一条记录（按路径）。返回删掉的行数。
    pub fn remove(&mut self, path: &Path) -> Result<usize, SearchError> {
        let p = path.to_string_lossy().to_string();
        let n = self
            .conn
            .execute("DELETE FROM files WHERE path = ?1", params![p])?;
        Ok(n)
    }

    /// 重命名：更新路径与小写名（保持索引与文件系统一致）。
    pub fn rename(&mut self, from: &Path, to: &Path) -> Result<(), SearchError> {
        let from_s = from.to_string_lossy().to_string();
        let to_s = to.to_string_lossy().to_string();
        let name_lower = to
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        self.conn.execute(
            "UPDATE files SET path=?1, name_lower=?2 WHERE path=?3",
            params![to_s, name_lower, from_s],
        )?;
        Ok(())
    }

    /// 记下「某个根已经爬到什么时刻」。
    ///
    /// 增量索引靠它：下一次自举只重爬**过期**的根，而不是每次启动都把整个主目录
    /// 再走一遍（几十万条目的遍历，跑一次就是几分钟）。
    pub fn mark_root(&mut self, root: &Path, at: i64) -> Result<(), SearchError> {
        let r = root.to_string_lossy().to_string();
        self.conn.execute(
            "INSERT INTO indexed_roots (root, at) VALUES (?1, ?2)
             ON CONFLICT(root) DO UPDATE SET at=excluded.at",
            params![r, at],
        )?;
        Ok(())
    }

    /// 某个根上次爬完的时刻（`None` = 从没爬过）。
    pub fn last_indexed(&self, root: &Path) -> Option<i64> {
        let r = root.to_string_lossy().to_string();
        self.conn
            .query_row(
                "SELECT at FROM indexed_roots WHERE root = ?1",
                params![r],
                |row| row.get::<_, i64>(0),
            )
            .ok()
    }

    /// 全部已登记的根与上次爬完的时刻（越久没刷的排在前面）。
    pub fn indexed_roots(&self) -> Vec<(String, i64)> {
        let mut stmt = match self
            .conn
            .prepare("SELECT root, at FROM indexed_roots ORDER BY at ASC")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 删掉某个前缀下的全部条目。
    ///
    /// 删目录时用的：`crawl` 只按路径 upsert 单条，目录被移走后它下面那些记录会
    /// 变成孤儿——搜索还能搜到，点开却「文件不存在」。
    pub fn remove_under(&mut self, prefix: &Path) -> Result<usize, SearchError> {
        let p = format!("{}/%", prefix.to_string_lossy());
        let n = self
            .conn
            .execute("DELETE FROM files WHERE path LIKE ?1", params![p])?;
        // 目录自己那条也一起走（watcher 的 Removed 对文件同样适用）。
        let self_row = self.remove(prefix)?;
        Ok(n + self_row)
    }

    /// 库中的文件总数。
    pub fn count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    /// 搜索：对 `name_lower` 与 `path` 做子串匹配（大小写不敏感），
    /// 按相关度排序后返回最多 `limit` 条。
    ///
    /// 排序规则：文件名以查询串开头的最相关，其次文件名包含，再次路径包含；
    /// 同级优先路径更短（更靠近根）的结果。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let like = format!("%{q}%");
        let starts = format!("{q}%");

        let mut stmt = self.conn.prepare(
            "SELECT path, name, size, modified, is_dir FROM files
             WHERE name_lower LIKE ?1 OR path LIKE ?1
             ORDER BY
               CASE WHEN name_lower LIKE ?2 THEN 0 ELSE 1 END,
               length(path) ASC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(params![like, starts, limit as i64], |r| {
            let path: String = r.get(0)?;
            let name: String = r.get(1)?;
            let size: i64 = r.get(2)?;
            let modified: i64 = r.get(3)?;
            let is_dir: i32 = r.get(4)?;
            Ok(SearchHit {
                path: PathBuf::from(path),
                name,
                kind: if is_dir != 0 {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                size: size as u64,
                modified: if modified > 0 {
                    Some(modified as u64)
                } else {
                    None
                },
            })
        })?;

        let mut hits = Vec::new();
        for row in rows {
            hits.push(row?);
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx() -> FileIndex {
        FileIndex::open_in_memory().unwrap()
    }

    #[test]
    fn upsert_and_search_by_name() {
        let mut i = idx();
        i.upsert(
            std::path::Path::new("/a/Report.md"),
            "Report.md",
            10,
            None,
            false,
        )
        .unwrap();
        i.upsert(
            std::path::Path::new("/a/report-final.md"),
            "report-final.md",
            20,
            None,
            false,
        )
        .unwrap();
        i.upsert(
            std::path::Path::new("/a/notes.txt"),
            "notes.txt",
            5,
            None,
            false,
        )
        .unwrap();
        i.upsert(std::path::Path::new("/a/docs"), "docs", 0, None, true)
            .unwrap();

        assert_eq!(i.count(), 4);

        let hits = i.search("report", 50).unwrap();
        // 两个 report 命中，且 Report.md（文件名以 report 开头）排在 report-final.md 之前。
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "Report.md");
        assert_eq!(hits[1].name, "report-final.md");
    }

    #[test]
    fn search_is_case_insensitive_and_path_aware() {
        let mut i = idx();
        i.upsert(
            std::path::Path::new("/deep/nested/Secret.md"),
            "Secret.md",
            1,
            None,
            false,
        )
        .unwrap();
        i.upsert(
            std::path::Path::new("/top/alpha.txt"),
            "alpha.txt",
            1,
            None,
            false,
        )
        .unwrap();

        // 小写查询也能命中大写文件名。
        let hits = i.search("SECRET", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Secret.md");

        // 路径子串也能命中。
        let by_path = i.search("nested", 10).unwrap();
        assert_eq!(by_path.len(), 1);
        assert!(by_path[0].path.ends_with("Secret.md"));
    }

    #[test]
    fn remove_and_rename_keep_index_consistent() {
        let mut i = idx();
        i.upsert(std::path::Path::new("/x/old.md"), "old.md", 1, None, false)
            .unwrap();
        i.remove(std::path::Path::new("/x/old.md")).unwrap();
        assert_eq!(i.count(), 0);

        i.upsert(std::path::Path::new("/x/a.md"), "a.md", 1, None, false)
            .unwrap();
        i.rename(
            std::path::Path::new("/x/a.md"),
            std::path::Path::new("/x/b.md"),
        )
        .unwrap();
        let hits = i.search("b.md", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, std::path::Path::new("/x/b.md"));
    }

    /// 「某个根爬到什么时候」要能记能查：增量索引靠它避免每次启动都重爬整棵树。
    #[test]
    fn roots_are_recorded_and_read_back() {
        let mut i = idx();
        assert!(i.last_indexed(Path::new("/home")).is_none());

        i.mark_root(Path::new("/home"), 100).unwrap();
        assert_eq!(i.last_indexed(Path::new("/home")), Some(100));

        // 再记一次覆盖旧值（不是插重复行）。
        i.mark_root(Path::new("/home"), 200).unwrap();
        assert_eq!(i.last_indexed(Path::new("/home")), Some(200));

        i.mark_root(Path::new("/data"), 50).unwrap();
        let roots = i.indexed_roots();
        assert_eq!(roots.len(), 2);
        // 「最久没刷的排在最前」：自举时按这个顺序重爬。
        assert_eq!(roots[0].0, "/data");
    }

    /// 删目录要连子树一起删：只删目录自己那条的话，它下面的记录会变成孤儿
    /// （搜索还能搜到，点开却「文件不存在」）。
    #[test]
    fn remove_under_takes_the_whole_subtree() {
        let mut i = idx();
        i.upsert(Path::new("/x/proj"), "proj", 0, None, true)
            .unwrap();
        i.upsert(Path::new("/x/proj/a.md"), "a.md", 1, None, false)
            .unwrap();
        i.upsert(Path::new("/x/proj/deep/b.md"), "b.md", 1, None, false)
            .unwrap();
        i.upsert(Path::new("/x/other.md"), "other.md", 1, None, false)
            .unwrap();
        assert_eq!(i.count(), 4);

        let n = i.remove_under(Path::new("/x/proj")).unwrap();
        assert_eq!(n, 3, "目录自己 + 两条子记录");

        assert!(i.search("a.md", 10).unwrap().is_empty());
        assert!(i.search("b.md", 10).unwrap().is_empty());
        // 别误伤同级其它文件。
        assert_eq!(i.search("other.md", 10).unwrap().len(), 1);
        assert_eq!(i.count(), 1);
    }

    #[test]
    fn empty_query_returns_nothing() {
        let mut i = idx();
        i.upsert(std::path::Path::new("/x/a.md"), "a.md", 1, None, false)
            .unwrap();
        assert!(i.search("", 10).unwrap().is_empty());
        assert!(i.search("   ", 10).unwrap().is_empty());
    }
}
