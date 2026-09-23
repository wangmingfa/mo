use std::collections::HashMap;

use crate::{OperationStatus, SharedOperation};

/// 一个操作的只读快照，用于 UI 展示进度面板。
#[derive(Debug, Clone)]
pub struct OperationHandle {
    pub id: u64,
    pub describe: String,
    pub status: OperationStatus,
    pub progress: (u64, u64),
    /// 该操作是否支持暂停（复制 / 移动这类字节级传输）。UI 据此决定
    /// 行尾给「暂停 / 继续」还是「取消」——不支持的操作给了暂停按钮也停不下来。
    pub pausable: bool,
}

/// 操作管理器：维护操作队列与每个操作的句柄。
///
/// 架构上对应：
///
/// ```text
/// OperationManager
/// ├── Copy #1
/// ├── Move #2
/// ├── Delete #3
/// └── Copy #4
/// ```
pub struct OperationManager {
    next_id: u64,
    handles: HashMap<u64, SharedOperation>,
}

impl OperationManager {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            handles: HashMap::new(),
        }
    }

    /// 分配下一个唯一操作 ID。
    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 注册一个操作（不立即执行）。
    pub fn register(&mut self, op: SharedOperation) {
        self.handles.insert(op.id(), op);
    }

    /// 取消指定操作。
    pub fn cancel(&self, id: u64) {
        if let Some(op) = self.handles.get(&id) {
            op.cancel();
        }
    }

    /// 暂停一个支持暂停的操作（复制 / 移动）。
    pub fn pause(&self, id: u64) {
        if let Some(op) = self.handles.get(&id) {
            op.pause();
        }
    }

    /// 恢复一个已暂停的操作。
    pub fn resume(&self, id: u64) {
        if let Some(op) = self.handles.get(&id) {
            op.resume();
        }
    }

    /// 查询操作状态。
    pub fn status(&self, id: u64) -> Option<OperationStatus> {
        self.handles.get(&id).map(|o| o.status())
    }

    /// 当前所有操作的快照（用于进度面板）。
    pub fn snapshot(&self) -> Vec<OperationHandle> {
        self.handles
            .values()
            .map(|o| OperationHandle {
                id: o.id(),
                describe: o.describe(),
                status: o.status(),
                progress: o.progress(),
                pausable: o.pausable(),
            })
            .collect()
    }

    /// 移除一个操作（UI 关闭进度条目后调用，避免句柄无限堆积）。
    pub fn remove(&mut self, id: u64) {
        self.handles.remove(&id);
    }
}

impl Default for OperationManager {
    fn default() -> Self {
        Self::new()
    }
}
