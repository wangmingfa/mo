use std::path::PathBuf;

use tokio::sync::broadcast;

/// 全局事件。不同模块通过事件总线解耦，而不是互相直接调用。
///
/// ```text
/// Filesystem ─► EventBus ─┬─► DirectoryModel
///                        ├─► SearchIndex
///                        ├─► Cache
///                        └─► UI
/// ```
#[derive(Debug, Clone)]
pub enum AppEvent {
    DirectoryChanged {
        path: PathBuf,
    },
    EntryCreated {
        path: PathBuf,
    },
    EntryDeleted {
        path: PathBuf,
    },
    EntryRenamed {
        from: PathBuf,
        to: PathBuf,
    },

    MetadataLoaded {
        path: PathBuf,
    },
    ThumbnailLoaded {
        path: PathBuf,
    },

    /// 全局搜索索引进度更新（后台爬取时周期性广播）。
    IndexUpdated {
        indexed: usize,
        root: PathBuf,
    },

    OperationStarted {
        id: u64,
    },
    OperationProgress {
        id: u64,
        done: u64,
        total: u64,
    },
    OperationFinished {
        id: u64,
    },

    /// 回收站内容变化（清空 / 永久删除 / 还原）。
    TrashChanged,

    NavigationChanged {
        path: PathBuf,
    },
}

/// 进程内事件总线（基于 `tokio::sync::broadcast`）。
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AppEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1024);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: AppEvent) {
        // 无订阅者时忽略错误。
        let _ = self.tx.send(event);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
