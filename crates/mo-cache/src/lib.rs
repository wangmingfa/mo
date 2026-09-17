//! mo-cache：缓存层（元数据 / 缩略图 / 目录 / 搜索）。
//!
//! 当前阶段用 SQLite 持久化元数据缓存；后续可扩展缩略图缓存、
//! 目录缓存与搜索索引元数据。选择 SQLite，是因为 Mo 后续很可能要承载
//! thumbnail cache / metadata cache / search index / recent / favorites /
//! operation history / app state 等结构化数据。

use mo_core::{FileId, FileMetadata, MoError, Permissions};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn re(e: impl std::fmt::Display) -> MoError {
    MoError::Other(e.to_string())
}

/// 元数据缓存（SQLite 持久化，线程安全）。
///
/// 内部用 `Mutex` 包住连接，因此 `&self` 即可并发调用，可以放心放进 `Arc`；
/// 写入走 `prepare_cached`，批量写入合并成**一个事务**——
/// 逐条提交时每条都是一次 fsync，一万条目要几秒，合并后降到几十毫秒。
pub struct MetadataCache {
    conn: Mutex<Connection>,
}

impl MetadataCache {
    /// 在 `path` 打开（或创建）缓存数据库。
    pub fn open(path: &Path) -> Result<Self, MoError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(re)?;
        }
        let conn = Connection::open(path).map_err(re)?;
        let cache = Self {
            conn: Mutex::new(conn),
        };
        cache.init()?;
        Ok(cache)
    }

    /// 打开默认位置的缓存库（`<用户缓存目录>/mo/metadata.sqlite`）。
    pub fn open_default() -> Result<Self, MoError> {
        Self::open(&default_cache_file())
    }

    fn init(&self) -> Result<(), MoError> {
        let conn = self.conn.lock().map_err(re)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS metadata (
                id       TEXT PRIMARY KEY,
                size     INTEGER NOT NULL,
                modified INTEGER NOT NULL,
                created  INTEGER NOT NULL,
                readonly INTEGER NOT NULL
             )",
        )
        .map_err(re)?;
        Ok(())
    }

    /// 写入 / 覆盖一条元数据。
    pub fn put(&self, id: &FileId, m: &FileMetadata) -> Result<(), MoError> {
        self.put_many(&[(*id, *m)])
    }

    /// 批量写入：合并为单个事务，用于打开大目录后一次性落盘。
    pub fn put_many(&self, items: &[(FileId, FileMetadata)]) -> Result<(), MoError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().map_err(re)?;
        let tx = conn.transaction().map_err(re)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO metadata (id, size, modified, created, readonly)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(re)?;
            for (id, m) in items {
                stmt.execute(params![
                    id.to_string(),
                    m.size as i64,
                    m.modified.map(systemtime_to_secs).unwrap_or(0),
                    m.created.map(systemtime_to_secs).unwrap_or(0),
                    m.permissions.readonly as i64,
                ])
                .map_err(re)?;
            }
        }
        tx.commit().map_err(re)?;
        Ok(())
    }

    /// 读取一条元数据（无则返回 `None`）。
    pub fn get(&self, id: &FileId) -> Result<Option<FileMetadata>, MoError> {
        let conn = self.conn.lock().map_err(re)?;
        let mut stmt = conn
            .prepare_cached("SELECT size, modified, created, readonly FROM metadata WHERE id = ?1")
            .map_err(re)?;
        let mut rows = stmt.query(params![id.to_string()]).map_err(re)?;
        if let Some(row) = rows.next().map_err(re)? {
            Ok(Some(row_to_metadata(row)?))
        } else {
            Ok(None)
        }
    }

    /// 批量读取：一次遍历取回所有命中的条目。
    ///
    /// 打开大目录时用它把整屏元数据的 N 次查询压成一次遍历。
    pub fn get_many(&self, ids: &[FileId]) -> Result<HashMap<FileId, FileMetadata>, MoError> {
        let mut out = HashMap::with_capacity(ids.len());
        if ids.is_empty() {
            return Ok(out);
        }
        let conn = self.conn.lock().map_err(re)?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, size, modified, created, readonly FROM metadata WHERE id = ?1",
            )
            .map_err(re)?;
        for id in ids {
            let mut rows = stmt.query(params![id.to_string()]).map_err(re)?;
            if let Some(row) = rows.next().map_err(re)? {
                // id 列在批量查询里位于 0，其余字段整体后移一位。
                out.insert(*id, row_to_metadata_offset(row)?);
            }
        }
        Ok(out)
    }

    /// 清空全部缓存条目。
    pub fn clear(&self) -> Result<(), MoError> {
        let conn = self.conn.lock().map_err(re)?;
        conn.execute("DELETE FROM metadata", []).map_err(re)?;
        Ok(())
    }
}

fn row_to_metadata(row: &rusqlite::Row) -> Result<FileMetadata, MoError> {
    let size: i64 = row.get(0).map_err(re)?;
    let modified: i64 = row.get(1).map_err(re)?;
    let created: i64 = row.get(2).map_err(re)?;
    let readonly: i64 = row.get(3).map_err(re)?;
    Ok(FileMetadata {
        size: size as u64,
        modified: from_secs(modified),
        created: from_secs(created),
        permissions: Permissions {
            readonly: readonly != 0,
            hidden: false,
        },
    })
}

/// 带 id 列的查询结果（列整体后移一位）。
fn row_to_metadata_offset(row: &rusqlite::Row) -> Result<FileMetadata, MoError> {
    let size: i64 = row.get(1).map_err(re)?;
    let modified: i64 = row.get(2).map_err(re)?;
    let created: i64 = row.get(3).map_err(re)?;
    let readonly: i64 = row.get(4).map_err(re)?;
    Ok(FileMetadata {
        size: size as u64,
        modified: from_secs(modified),
        created: from_secs(created),
        permissions: Permissions {
            readonly: readonly != 0,
            hidden: false,
        },
    })
}

/// Mo 的缓存根目录（`<用户缓存目录>/mo`）。
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("mo")
}

fn default_cache_file() -> PathBuf {
    cache_dir().join("metadata.sqlite")
}

fn systemtime_to_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn from_secs(secs: i64) -> Option<SystemTime> {
    if secs <= 0 {
        None
    } else {
        Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
    }
}
