//! mo-operations：文件操作层。
//!
//! UI 绝不直接调用 `std::fs::copy` 之类，而是发出命令 → `OperationManager`
//! 把命令变成后台任务，并暴露统一的进度 / 取消 / 暂停 / 恢复能力。
//!
//! ```text
//! UI → Command → OperationManager → OperationQueue → CopyOperation
//! ```

mod archive;
mod copy;
mod dedup;
mod delete;
mod fs_util;
mod hash;
mod link;
mod manager;
mod move_op;
mod perms;
mod rename;
mod restore_op;
mod sync;
mod trash;
mod trash_op;

pub use archive::{create_archive, extract_archive, ArchiveFormat};
pub use copy::CopyOperation;
pub use dedup::{find_duplicates, DedupReport, DupGroup};
pub use delete::DeleteOperation;
pub use fs_util::{resolve_target, unique_path, ConflictPolicy, Target};
pub use hash::{compute_hashes, HashAlgo};
pub use link::{create_link, LinkKind, LinkOperation};
pub use manager::{OperationHandle, OperationManager};
pub use move_op::MoveOperation;
pub use perms::{mode_string, set_permissions};
pub use rename::RenameOperation;
pub use restore_op::RestoreOperation;
pub use sync::{
    apply as apply_sync_plan, plan as plan_sync, Action as SyncAction,
    ConflictPolicy as SyncConflictPolicy, Plan as SyncPlan, SyncMode, SyncOptions, SyncReport,
};
pub use trash::{Trash, TrashEntry, TrashError};
pub use trash_op::TrashOperation;

use mo_core::MoError;
use std::sync::Arc;

/// 操作状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// 操作共享的可变状态：通过 `Arc<Mutex<OpInner>>` 在内部控制进度 / 取消 / 暂停。
pub(crate) struct OpInner {
    pub status: OperationStatus,
    pub done: u64,
    pub total: u64,
    pub cancel: bool,
    pub pause: bool,
    pub error: Option<String>,
}

impl OpInner {
    pub fn new() -> Self {
        Self {
            status: OperationStatus::Pending,
            done: 0,
            total: 0,
            cancel: false,
            pause: false,
            error: None,
        }
    }
}

/// 一个文件操作。所有后台任务都实现这个 trait，并拥有：
/// state / progress / cancel / pause / resume / error。
pub trait Operation: Send + Sync + 'static {
    fn id(&self) -> u64;
    fn describe(&self) -> String;
    fn status(&self) -> OperationStatus;
    fn progress(&self) -> (u64, u64);
    fn cancel(&self);
    fn pause(&self);
    fn resume(&self);
    /// 是否支持暂停。只有真正在循环里检查暂停标记的操作（复制 / 移动这类
    /// 字节级传输）才该暴露「暂停」按钮——单文件快操作按了也没处停，UI 会骗人。
    fn pausable(&self) -> bool {
        false
    }
    /// 在后台任务中执行；实现应周期性检查取消标记。
    fn run(&self) -> Result<(), MoError>;
}

/// 可被 `OperationManager` 持有的共享操作引用。
pub type SharedOperation = Arc<dyn Operation>;
