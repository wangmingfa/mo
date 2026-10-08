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
            );
            -- FTS5 trigram 虚表：对 name_lower / path 做大小写不敏感的子串索引，
            -- 取代原来 `name_lower LIKE '%词%' OR path LIKE '%词%'` 的前导 % 全表扫
            -- （58 万行实测 ~150ms/次）。trigram 分词器对短语做中缀子串匹配，
            -- 正好覆盖「文件名中间 / 路径任意段」这类前缀索引做不到的命中。
            -- content='files' 把虚表绑到实体表，rowid 直接复用 files.id，
            -- 数据只存一份，同步交给下面三个触发器。
            CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
                name, path,
                content='files',
                content_rowid='id',
                tokenize='trigram'
            );
            CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
                INSERT INTO files_fts(rowid, name, path) VALUES (new.id, new.name_lower, new.path);
            END;
            CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
                INSERT INTO files_fts(files_fts, rowid, name, path)
                VALUES('delete', old.id, old.name_lower, old.path);
            END;
            CREATE TRIGGER IF NOT EXISTS files_au AFTER UPDATE ON files BEGIN
                INSERT INTO files_fts(files_fts, rowid, name, path)
                VALUES('delete', old.id, old.name_lower, old.path);
                INSERT INTO files_fts(rowid, name, path) VALUES (new.id, new.name_lower, new.path);
            END;",
        )?;

        // 旧版索引库（升级前没有 files_fts）一次性回填：把现有 files 灌进 FTS 虚表。
        // 全新库 files 为空 → 跳过；升级后 files_fts 已非空 → 也跳过；只在「有数据却没 FTS」
        // 这一刻跑一次（58 万行约一两秒，之后不再触发）。回填后所有 upsert/remove/rename
        // 都走上面的触发器，fts 与 files 始终一致。
        let needs_backfill: bool = self
            .conn
            .query_row(
                "SELECT (SELECT count(*) FROM files) > 0 AND (SELECT count(*) FROM files_fts) = 0",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false);
        if needs_backfill {
            self.conn.execute(
                "INSERT INTO files_fts(rowid, name, path) SELECT id, name_lower, path FROM files",
                [],
            )?;
        }
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
                // rusqlite 0.40 起不再为 u64 实现 ToSql（有损映射被移除），显式收窄。
                size as i64,
                modified.unwrap_or(0) as i64,
                is_dir as i32
            ],
        )?;
        Ok(())
    }

    /// 批量写入（一个事务 + 预编译语句复用）。
    ///
    /// 爬取路径专用：逐条 `upsert` 每条都是一个独立事务，几万条目就是几万次提交，
    /// 既慢、又把索引锁攥在手里更久。批量后墙钟快一个量级，锁也是**拿一批放一次**。
    pub fn upsert_batch(&mut self, entries: &[crate::CrawledEntry]) -> Result<(), SearchError> {
        if entries.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO files (path, name, name_lower, size, modified, is_dir)
                 VALUES (?1, ?2, ?3, 0, 0, ?4)
                 ON CONFLICT(path) DO UPDATE SET
                   name=excluded.name, name_lower=excluded.name_lower",
            )?;
            for e in entries {
                stmt.execute(rusqlite::params![
                    e.path.to_string_lossy(),
                    e.name,
                    e.name.to_lowercase(),
                    e.is_dir as i32,
                ])?;
            }
        }
        tx.commit()?;
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
    ///
    /// 走 path 的**字节范围**扫描，不用 `LIKE '前缀/%'`，三条理由（2026-10-06，§45）。
    ///
    /// 一是快：LIKE 的前缀通配用不上索引（LIKE 默认大小写不敏感），58 万行的库上
    /// 实测一次 **147ms 全表扫**，这期间索引锁被攥着，而 `AppState` 那边每个被移走
    /// 的条目都要来一次——拖动四个文件移动就是四次。同一段时间里主线程还要问状态栏
    /// 那个「已索引」，排队等这把锁，就是用户报的「卡住几秒钟」。范围写法走
    /// `idx_path`，同一台机器上未命中 **33µs**。
    ///
    /// 二是准：LIKE 把 `_` 与 `%` 当通配符，删 `a_b.txt` 会连邻居 `axb.txt` 的子树
    /// 一起抹掉。
    ///
    /// 三是 Windows：落库的是反斜杠路径，旧写法拼的 `{前缀}/%` 一个孩子都匹配不上
    /// ——本机删目录等于只删了目录自己那条，整棵子树留在索引里当孤儿。
    pub fn remove_under(&mut self, prefix: &Path) -> Result<usize, SearchError> {
        let p = prefix.to_string_lossy();
        let mut n = 0;
        // '/' 是 0x2F、'\\' 是 0x5C，各自右开在上界取「下一个字节」（0x30 '0'、0x5D ']'）：
        // 任何以 `前缀/` 开头的串都落在 [`前缀/`, `前缀0`) 这段连续区间里，反之这段里
        // 也只有它的孩子（BINARY 排序按 UTF-8 字节逐位比）。两种分隔符各扫一遍，
        // 于是同一个函数在 macOS 与 Windows 上都成立。
        for (lo, hi) in [
            (format!("{p}/"), format!("{p}0")),
            (format!("{p}\\"), format!("{p}]")),
        ] {
            n += self.conn.execute(
                "DELETE FROM files WHERE path >= ?1 AND path < ?2",
                params![lo, hi],
            )?;
        }
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

    /// 搜索：对 `name_lower` 与 `path` 做大小写不敏感的子串匹配，
    /// 按相关度排序后返回最多 `limit` 条。
    ///
    /// 查找走 `files_fts` 这张 FTS5 trigram 虚表（`init` 里建好 + 三个触发器同步），
    /// `MATCH` 直接落到 trigram 索引上，不再是前导 `%` 的全表扫——58 万行的库上
    /// 从实测 ~150ms/次降到索引查找的量级。触发器的存在意味着 `upsert` / `remove` /
    /// `rename` / `remove_under` 走实体表时 fts 自动跟着改，search 这边不用关心。
    ///
    /// 排序规则：文件名以查询串开头的最相关，其次文件名包含，再次路径包含；
    /// 同级优先路径更短（更靠近根）的结果——这条 ORDER BY 只作用在 fts 已收窄的
    /// 候选集上，量很小，不影响查找本身的开销。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        // 把查询包成带双引号的 FTS5 短语，规避括号 / `*` 等 FTS 语法字符在查询里炸开；
        // 内部的双引号按 FTS 约定翻倍转义。trigram 对短语做大小写不敏感的子串匹配，
        // 对应原来的 `name_lower LIKE '%q%' OR path LIKE '%q%'`。
        let match_q = format!("\"{}\"", q.replace('"', "\"\""));
        let starts = format!("{q}%");

        let mut stmt = self.conn.prepare(
            "SELECT f.path, f.name, f.size, f.modified, f.is_dir
             FROM files_fts
             JOIN files f ON f.id = files_fts.rowid
             WHERE files_fts MATCH ?1
             ORDER BY
               CASE WHEN f.name_lower LIKE ?2 THEN 0 ELSE 1 END,
               length(f.path) ASC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(params![match_q, starts, limit as i64], |r| {
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

    /// trigram 做的是真·中缀子串匹配：子串躲在文件名中段（不在开头）也要能命中，
    /// 这正是换掉 `LIKE '%词%'` 全表扫之后仍要保住的能力。
    #[test]
    fn search_finds_substring_not_just_prefix() {
        let mut i = idx();
        i.upsert(
            std::path::Path::new("/a/photograph.png"),
            "photograph.png",
            1,
            None,
            false,
        )
        .unwrap();
        i.upsert(
            std::path::Path::new("/a/tograph.txt"),
            "tograph.txt",
            1,
            None,
            false,
        )
        .unwrap();
        // "togr" 不在任何文件名开头，只在中间 —— 前缀索引做不到，trigram 必须能中。
        let hits = i.search("togr", 50).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.name.contains("togr")));
    }

    /// 性能红线：搜索必须走 fts5 索引，不能退化成 `files` 全表扫（那是 58 万行 ~150ms 的老账）。
    #[test]
    fn search_uses_fts_index_not_full_scan() {
        let mut i = idx();
        // 塞足够多条目，让「全表扫」与「索引查找」在 query plan 上能分出差别。
        for n in 0..500u32 {
            i.upsert(
                std::path::Path::new(&format!("/a/file_{n}.txt")),
                &format!("file_{n}.txt"),
                1,
                None,
                false,
            )
            .unwrap();
        }
        i.upsert(
            std::path::Path::new("/a/needle_in_haystack.txt"),
            "needle_in_haystack.txt",
            1,
            None,
            false,
        )
        .unwrap();

        let plan_rows: Vec<String> = i
            .conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT f.path FROM files_fts
                 JOIN files f ON f.id = files_fts.rowid
                 WHERE files_fts MATCH ?1 LIMIT ?2",
            )
            .unwrap()
            .query_map(params!["\"hay\"", 10i64], |r| {
                // EXPLAIN QUERY PLAN 返回 4 列：id/parent/notused 是整数，detail 是文本。
                let _id: i64 = r.get(0)?;
                let _parent: i64 = r.get(1)?;
                let _notused: i64 = r.get(2)?;
                let detail: String = r.get(3)?;
                Ok(detail)
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let plan = plan_rows.join(" | ");
        // 命中即证明不是全表扫：files_fts 走 VIRTUAL TABLE INDEX（trigram MATCH），
        // 基表 files 通过 rowid 主键查找（SEARCH ... INTEGER PRIMARY KEY）而非 SCAN。
        assert!(
            plan.contains("files_fts"),
            "search 应当走 files_fts 虚表，实际 plan: {plan}"
        );
        assert!(
            plan.contains("VIRTUAL TABLE INDEX"),
            "search 应当用 trigram 索引匹配，实际 plan: {plan}"
        );
        assert!(
            plan.contains("SEARCH f USING INTEGER PRIMARY KEY"),
            "files 应通过 rowid 主键查找而非全表扫，实际 plan: {plan}"
        );

        // 行为面：含 'hay' 子串的文件要能命中。
        let hits = i.search("hay", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.name.contains("hay")),
            "应命中含 'hay' 子串的文件"
        );
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

    /// Windows 落库的是反斜杠路径，`{前缀}\` 下的孩子也要一起删。旧写法只拼了
    /// `{前缀}/%`，在本机一个孩子的都匹配不上——删目录等于只删了目录自己那条。
    #[test]
    fn remove_under_covers_backslash_children() {
        let mut i = idx();
        for (p, is_dir) in [
            (r#"D:\x\proj"#, true),
            (r#"D:\x\proj\a.md"#, false),
            (r#"D:\x\proj\deep\b.md"#, false),
            (r#"D:\x\other.md"#, false),
        ] {
            i.upsert(
                Path::new(p),
                p.rsplit('\\').next().unwrap(),
                1,
                None,
                is_dir,
            )
            .unwrap();
        }
        assert_eq!(i.count(), 4);

        let n = i.remove_under(Path::new(r#"D:\x\proj"#)).unwrap();
        assert_eq!(n, 3, "目录自己 + 两条反斜杠子记录");
        assert_eq!(i.count(), 1, "只剩同级那条");
        assert_eq!(i.search("other.md", 10).unwrap().len(), 1);
    }

    /// `_` 与 `%` 在 LIKE 里是通配符：旧写法（`LIKE '前缀/%'`）删 `/x/a_b.txt` 会把
    /// 邻居 `/x/axb.txt/inner.md` 当成它的子树一起抹掉。范围扫描按字节逐位比，没这个口子。
    #[test]
    fn remove_under_does_not_treat_underscores_as_wildcards() {
        let mut i = idx();
        i.upsert(Path::new("/x/a_b.txt"), "a_b.txt", 1, None, false)
            .unwrap();
        i.upsert(Path::new("/x/axb.txt"), "axb.txt", 0, None, true)
            .unwrap();
        i.upsert(Path::new("/x/axb.txt/inner.md"), "inner.md", 1, None, false)
            .unwrap();

        let n = i.remove_under(Path::new("/x/a_b.txt")).unwrap();
        assert_eq!(n, 1, "只有被移走的那一条");
        assert_eq!(i.count(), 2, "邻居和它的子树都该还在");
        assert_eq!(i.search("inner.md", 10).unwrap().len(), 1);
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
