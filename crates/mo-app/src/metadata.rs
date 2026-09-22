use std::path::PathBuf;
use std::sync::Arc;

use mo_cache::MetadataCache;
use mo_core::{Directory, Entry, FileId, FileMetadata, MetadataState, Permissions};
use tokio::sync::Semaphore;

use crate::AppState;

/// 单批 stat 的条目数。
///
/// 一批 = 一次 blocking 任务、一次状态写锁、一条 SQLite 事务。
/// 逐条处理时，一万条目就是一万次 spawn + 一万次写锁 + 上万次独立事务（fsync）；
/// 批量化后全部降两个数量级——这是大目录滚动闪烁修复的核心。
const STAT_BATCH: usize = 64;

/// 元数据调度器：限流并发 + 可见区优先 + 缓存感知。
///
/// 对应架构中的 `MetadataScheduler`：
///
/// ```text
/// Directory Reader → Entries → Metadata Queue
///                              ├── visible      （高优先级）
///                              ├── selected
///                              ├── near viewport
///                              └── background
/// ```
///
/// 缓存策略是 **stale-while-revalidate**：
/// 打开目录时先用缓存值把条目填满（用户立刻看到大小 / 日期），
/// 再由后台任务逐批 `stat` 校验，只有真正变化时才更新视图并写回缓存。
#[derive(Clone)]
pub struct MetadataScheduler {
    semaphore: Arc<Semaphore>,
}

impl MetadataScheduler {
    pub fn new() -> Self {
        // 8 批并发 × 64 条 = 至多 512 个 stat 在 blocking 池排队，本地 SSD 毫无压力。
        Self {
            semaphore: Arc::new(Semaphore::new(8)),
        }
    }

    /// 用缓存预填**视图首屏**的元数据，返回命中条数。
    ///
    /// 只填前 `upto` 个**视图序**的条目，而不是整份列表：`upto` 之外那两万条用户
    /// 此刻一行都看不到，而「一次查 2.7 万个 id」实测要 **133ms**（打开大目录时最大
    /// 的一笔延迟）。它们的值由随后的 [`MetadataScheduler::load`] 逐条 `stat` 时补——
    /// 那里的顺序本来就是首屏优先，滚到哪行、哪行就有值。
    ///
    /// 一次批量查询而不是 N 次单条查询，避免打开大目录时打爆 SQLite。
    pub fn prime_visible_from_cache(
        cache: &MetadataCache,
        dir: &mut Directory,
        upto: usize,
    ) -> usize {
        // 先把前 `upto` 个视图下标抄出来：`dir.view` 与 `dir.entries` 要同时借。
        let head: Vec<usize> = dir
            .view
            .visible_indices()
            .iter()
            .take(upto)
            .copied()
            .collect();
        if head.is_empty() {
            return 0;
        }
        let ids: Vec<FileId> = head.iter().map(|&i| dir.entries[i].id).collect();
        let Ok(map) = cache.get_many(&ids) else {
            return 0;
        };
        let mut hits = 0;
        for i in head {
            if let Some(m) = map.get(&dir.entries[i].id) {
                dir.entries[i].metadata = MetadataState::Loaded(*m);
                hits += 1;
            }
        }
        hits
    }

    /// 为一组条目校验元数据（批量版）。
    ///
    /// `priority` 指定可见区间的下标范围，这些条目所在批次会被排到队列最前，
    /// 因此用户当前看到的行总是最先补全——大目录下这一点决定体感。
    ///
    /// ⚠️ 必须**批量**处理，不要退化回逐条 spawn：
    /// 逐条会让 2.7 万个任务挤满仅有的几个 runtime worker（且内含阻塞 stat），
    /// UI 的窗口取回任务被排到积压后面，滚动时整屏占位符闪烁。
    pub fn load(
        &self,
        app: AppState,
        entries: Vec<Entry>,
        priority: Option<std::ops::Range<usize>>,
    ) {
        let cache = app.cache();

        // 可见区优先：稳定排序，保证区间外的条目仍按原顺序处理。
        let mut ordered: Vec<(usize, Entry)> = entries.into_iter().enumerate().collect();
        if let Some(r) = priority {
            ordered.sort_by_key(|(i, _)| if r.contains(i) { 0usize } else { 1usize });
        }

        for chunk in ordered.chunks(STAT_BATCH) {
            let permit = self.semaphore.clone();
            let app_task = app.clone();
            let cache_task = cache.clone();
            // (id, path, 旧值)：旧值用于判断「是否真的变了」，没变就不写锁、不进缓存。
            let batch: Vec<(FileId, PathBuf, Option<FileMetadata>)> = chunk
                .iter()
                .map(|(_, e)| {
                    let cached = match &e.metadata {
                        MetadataState::Loaded(m) => Some(*m),
                        _ => None,
                    };
                    (e.id, e.path.clone(), cached)
                })
                .collect();

            // 走 AppState 自带的 runtime，避免依赖调用方线程的 tokio 上下文。
            app.spawn(async move {
                let _permit = match permit.acquire().await {
                    Ok(p) => p,
                    Err(_) => return,
                };
                // stat 与 SQLite 写回都是阻塞操作，整批放 blocking 池。
                let changed = app_task.spawn_blocking(move || {
                    let mut changed: Vec<(FileId, FileMetadata)> = Vec::new();
                    for (id, path, cached) in &batch {
                        let Ok(m) = std::fs::metadata(path) else {
                            continue;
                        };
                        let meta = FileMetadata {
                            size: m.len(),
                            modified: m.modified().ok(),
                            created: m.created().ok(),
                            permissions: Permissions {
                                readonly: m.permissions().readonly(),
                                hidden: false,
                                mode: unix_mode(&m),
                            },
                        };
                        if cached.is_none_or(|old| old != meta) {
                            changed.push((*id, meta));
                        }
                    }
                    if let Some(c) = &cache_task {
                        if !changed.is_empty() {
                            if let Err(e) = c.put_many(&changed) {
                                tracing::warn!("元数据缓存写入失败：{e}");
                            }
                        }
                    }
                    changed
                });
                // 一批只拿一次写锁。
                if let Ok(changed) = changed.await {
                    if !changed.is_empty() {
                        app_task.update_metadata_batch(changed).await;
                    }
                }
            });
        }
    }
}

impl Default for MetadataScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// 取 unix 权限位（低 9 位）；非 unix 平台返回 0。
fn unix_mode(m: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        0
    }
}
