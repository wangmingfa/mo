use std::sync::Arc;

use mo_core::{Entry, ThumbnailState};
use mo_thumbnails::{ThumbnailCache, DEFAULT_SIZE};
use tokio::sync::Semaphore;

use crate::AppState;

/// 缩略图调度器：只为**当前可见**的条目生成缩略图。
///
/// 缩略图是典型的昂贵任务（解码一张大图可能几十毫秒），因此：
///   * 只有 UI 报告可见区后才请求，绝不「打开目录就全量生成」；
///   * 并发受信号量限制（默认 4，解码是 CPU 密集，超过核数只会让列表更卡）；
///   * 命中磁盘缓存时只是一次 `stat`，直接标记完成，不进后台池；
///   * 生成过程放在 blocking 池，不占用异步 worker。
#[derive(Clone)]
pub struct ThumbnailScheduler {
    cache: Arc<ThumbnailCache>,
    semaphore: Arc<Semaphore>,
    size: u32,
    /// 已经排上队（还没落地）的条目 id。见 [`ThumbnailScheduler::request`] 里的说明。
    inflight: Arc<std::sync::Mutex<std::collections::HashSet<mo_core::FileId>>>,
}

impl ThumbnailScheduler {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(ThumbnailCache::new()),
            semaphore: Arc::new(Semaphore::new(4)),
            size: DEFAULT_SIZE,
            inflight: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// 指定缓存目录（测试用）。
    pub fn with_cache(cache: ThumbnailCache) -> Self {
        Self {
            cache: Arc::new(cache),
            semaphore: Arc::new(Semaphore::new(4)),
            size: DEFAULT_SIZE,
            inflight: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// 底层磁盘缓存。
    pub fn cache(&self) -> Arc<ThumbnailCache> {
        self.cache.clone()
    }

    /// 为一批条目请求缩略图（fire-and-forget）。
    ///
    /// 可以放心地在**每一帧**重复调用（UI 就是这么用的：只给看得见的行排队）：
    ///
    /// * 已加载 / 生成失败的条目直接跳过；
    /// * 已经在队的条目靠 [`ThumbnailScheduler::inflight`] 去重。
    ///
    /// ⚠️ 后者不是可有可无的：`ThumbnailState::Loading` 从来没有被置位过（条目从
    /// `Idle` 直接到 `Loaded`/`Failed`），而 UI 手里那份窗口快照要等下一轮同步才
    /// 更新——只判 `Idle` 的话，同一张图会被每帧排一个新任务。
    pub fn request(&self, app: AppState, entries: Vec<Entry>) {
        for entry in entries {
            if !entry.supports_thumbnail() {
                continue;
            }
            if !matches!(entry.thumbnail, ThumbnailState::Idle) {
                continue;
            }

            let id = entry.id;
            let path = entry.path.clone();
            let cache = self.cache.clone();
            let semaphore = self.semaphore.clone();
            let size = self.size;
            let app_task = app.clone();
            let inflight = self.inflight.clone();
            {
                let mut inflight = self.inflight.lock().unwrap();
                if !inflight.insert(id) {
                    continue;
                }
                // 逛久了这个账本会涨：封顶整清，代价只是少数几张图被重复排一次队。
                if inflight.len() > INFLIGHT_MAX {
                    inflight.clear();
                }
            }

            app.spawn(async move {
                let _permit = match semaphore.acquire().await {
                    Ok(p) => p,
                    Err(_) => return,
                };
                // 一趟 blocking 干完两件事：磁盘缓存（命中 / 生成，跨会话复用）→
                // 解码成**内存位图**。UI 的 `img(path)` 要异步读盘解码，位图没到
                // 之前那一格什么都不画（切目录时图标闪烁的另一半来源）；位图交给
                // UI 后 `ImageSource::Render` 同步上屏。缓存 PNG 很小（128px），
                // 解开是亚毫秒级的后台活。
                let handle = app_task.spawn_blocking(move || {
                    cache.get_or_create(&id, &path, size).and_then(|p| {
                        mo_thumbnails::decode_bitmap(&p).ok_or_else(|| {
                            mo_thumbnails::ThumbnailError::Decode(p.display().to_string())
                        })
                    })
                });
                let state = match handle.await {
                    Ok(Ok(bm)) => ThumbnailState::Loaded(std::sync::Arc::new(bm)),
                    Ok(Err(e)) => {
                        tracing::debug!("缩略图生成失败 {id}：{e}");
                        ThumbnailState::Failed
                    }
                    Err(_) => ThumbnailState::Failed,
                };
                // 销账：条目状态随即会变成 Loaded / Failed，之后不必再排。
                inflight.lock().unwrap().remove(&id);
                app_task.set_thumbnail(id, state).await;
            });
        }
    }
}

/// `inflight` 账本的封顶（超出整清）。
const INFLIGHT_MAX: usize = 4000;

impl Default for ThumbnailScheduler {
    fn default() -> Self {
        Self::new()
    }
}
