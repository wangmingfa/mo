use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use mo_core::MoError;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// 文件系统监听事件（已从 `notify` 的事件归一化）。
#[derive(Debug, Clone)]
pub enum WatcherEvent {
    Created(PathBuf),
    Removed(PathBuf),
    Modified(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
}

/// 基于 `notify` 的文件系统监听器。
///
/// 架构：Filesystem → OS Watcher → notify → FileSystemEvent → DirectoryModel → UI。
/// 外部程序删除一个文件时，只发出 `Removed` 事件，DirectoryModel 据此移除对应条目，
/// 而不是重新读取整个目录。
pub struct FileSystemWatcher {
    #[allow(dead_code)]
    inner: RecommendedWatcher,
    rx: mpsc::Receiver<WatcherEvent>,
}

impl FileSystemWatcher {
    /// 监听 `path`（非递归）。返回的 watcher 在 drop 时停止监听。
    pub fn watch(path: &Path) -> Result<Self, MoError> {
        let (tx, rx) = mpsc::channel::<WatcherEvent>();
        let mut inner = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(ev) = res {
                for p in ev.paths.iter() {
                    let we = match ev.kind {
                        EventKind::Create(_) => WatcherEvent::Created(p.clone()),
                        EventKind::Remove(_) => WatcherEvent::Removed(p.clone()),
                        // notify 把重命名表达为带两个路径的 Modify 事件；
                        // 其余 Modify（内容/属性变化）按 Modified 处理。
                        EventKind::Modify(_) if ev.paths.len() >= 2 => WatcherEvent::Renamed {
                            from: ev.paths[0].clone(),
                            to: ev.paths[1].clone(),
                        },
                        EventKind::Modify(_) => WatcherEvent::Modified(p.clone()),
                        _ => continue,
                    };
                    let _ = tx.send(we);
                }
            }
        })
        .map_err(|e| MoError::Other(e.to_string()))?;

        inner
            .watch(path, RecursiveMode::NonRecursive)
            .map_err(|e| MoError::Other(e.to_string()))?;

        Ok(Self { inner, rx })
    }

    /// 阻塞获取下一个事件（最多等待 `timeout`）。
    pub fn recv_timeout(&self, timeout: Duration) -> Option<WatcherEvent> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// 非阻塞获取一个事件（若有）。
    pub fn try_recv(&self) -> Option<WatcherEvent> {
        self.rx.try_recv().ok()
    }
}
