use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mo_cache::MetadataCache;
use mo_core::{Entry, FileId, FileMetadata, MetadataState};
use tokio::sync::Semaphore;

use crate::AppState;

/// 攒够这么多条再一次性提交给 SQLite。
///
/// 逐条提交时每条都是一次独立事务（一次 fsync），一万条目要几秒；
/// 批量提交后降到几十毫秒。
const WRITE_BATCH: usize = 256;

/// 后台「写回缓存」：攒批 + 最后一条完成时收尾。
///
/// 与前台逻辑完全解耦——即使缓存写失败也不影响浏览。
struct WriteBehind {
    buf: Mutex<Vec<(FileId, FileMetadata)>>,
    remaining: AtomicUsize,
    cache: Arc<MetadataCache>,
}

impl WriteBehind {
    fn new(cache: Arc<MetadataCache>, total: usize) -> Arc<Self> {
        Arc::new(Self {
            buf: Mutex::new(Vec::new()),
            // 初始为 1：防止 total 为 0 时提前 flush。
            remaining: AtomicUsize::new(total + 1),
            cache,
        })
    }

    /// 一个条目校验完成。
    fn finish(&self) {
        // 先把初始的 +1 抵消掉，再由最后一个完成的任务触发收尾。
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
            self.flush(&mut buf);
        }
    }

    fn record(&self, id: FileId, meta: FileMetadata) {
        let mut buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        buf.push((id, meta));
        if buf.len() >= WRITE_BATCH {
            self.flush(&mut buf);
        }
    }

    fn flush(&self, buf: &mut Vec<(FileId, FileMetadata)>) {
        if buf.is_empty() {
            return;
        }
        if let Err(e) = self.cache.put_many(buf) {
            tracing::warn!("元数据缓存写入失败：{e}");
        }
        buf.clear();
    }
}

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
/// 再由后台任务逐条 `stat` 校验，只有真正变化时才更新视图并写回缓存。
#[derive(Clone)]
pub struct MetadataScheduler {
    semaphore: Arc<Semaphore>,
}

impl MetadataScheduler {
    pub fn new() -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(8)),
        }
    }

    /// 用缓存预填一批条目的元数据，返回命中条数。
    ///
    /// 一次批量查询而不是 N 次单条查询，避免打开大目录时打爆 SQLite。
    pub fn prime_from_cache(cache: &MetadataCache, entries: &mut [Entry]) -> usize {
        let ids: Vec<FileId> = entries.iter().map(|e| e.id).collect();
        let Ok(map) = cache.get_many(&ids) else {
            return 0;
        };
        let mut hits = 0;
        for e in entries.iter_mut() {
            if let Some(m) = map.get(&e.id) {
                e.metadata = MetadataState::Loaded(*m);
                hits += 1;
            }
        }
        hits
    }

    /// 为一组条目校验元数据。
    ///
    /// `priority` 指定可见区间的下标范围，这些条目会被排到队列最前，
    /// 因此用户当前看到的行总是最先补全——大目录下这一点决定体感。
    pub fn load(
        &self,
        app: AppState,
        entries: Vec<Entry>,
        priority: Option<std::ops::Range<usize>>,
    ) {
        let write = app.cache().map(|c| WriteBehind::new(c, entries.len()));

        // 可见区优先：稳定排序，保证区间外的条目仍按原顺序处理。
        let mut ordered: Vec<(usize, Entry)> = entries.into_iter().enumerate().collect();
        if let Some(r) = priority {
            ordered.sort_by_key(|(i, _)| if r.contains(i) { 0usize } else { 1usize });
        }

        for (_, entry) in ordered {
            let permit = self.semaphore.clone();
            let app_task = app.clone();
            let writes = write.clone();
            let id = entry.id;
            let path = entry.path.clone();
            // 缓存 / 上次加载得到的旧值，用于判断「是否真的变了」。
            let cached = match &entry.metadata {
                MetadataState::Loaded(m) => Some(*m),
                _ => None,
            };

            // 走 AppState 自带的 runtime，避免依赖调用方线程的 tokio 上下文。
            app.spawn(async move {
                let _permit = match permit.acquire().await {
                    Ok(p) => p,
                    Err(_) => {
                        if let Some(w) = &writes {
                            w.finish();
                        }
                        return;
                    }
                };
                let fs = app_task.file_system().clone();
                if let Ok(meta) = fs.metadata(&path).await {
                    let changed = match &cached {
                        Some(old) => old != &meta,
                        None => true,
                    };
                    if changed {
                        app_task.update_metadata(id, meta).await;
                        if let Some(w) = &writes {
                            w.record(id, meta);
                        }
                    }
                }
                if let Some(w) = &writes {
                    w.finish();
                }
            });
        }

        // 抵消 WriteBehind 初始的 +1（此时所有任务都已派生）。
        if let Some(w) = &write {
            w.finish();
        }
    }
}

impl Default for MetadataScheduler {
    fn default() -> Self {
        Self::new()
    }
}
