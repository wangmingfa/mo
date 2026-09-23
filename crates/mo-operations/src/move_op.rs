use std::path::PathBuf;
use std::sync::Arc;

use mo_core::MoError;
use parking_lot::Mutex;

use crate::fs_util::{copy_tree, remove_path, resolve_target, ConflictPolicy, Target};
use crate::{OpInner, Operation, OperationStatus};

/// 移动操作。
///
/// 优先走同文件系统 `rename`（快路径）；跨设备时退回复制 + 删除源。
pub struct MoveOperation {
    id: u64,
    from: PathBuf,
    to: PathBuf,
    policy: ConflictPolicy,
    state: Arc<Mutex<OpInner>>,
}

impl MoveOperation {
    /// 新建移动操作，默认冲突策略为「自动改名」（永不静默覆盖）。
    pub fn new(id: u64, from: PathBuf, to: PathBuf) -> Arc<Self> {
        Self::with_policy(id, from, to, ConflictPolicy::default())
    }

    pub fn with_policy(id: u64, from: PathBuf, to: PathBuf, policy: ConflictPolicy) -> Arc<Self> {
        Arc::new(Self {
            id,
            from,
            to,
            policy,
            state: Arc::new(Mutex::new(OpInner::new())),
        })
    }
}

impl Operation for MoveOperation {
    fn id(&self) -> u64 {
        self.id
    }
    fn describe(&self) -> String {
        format!("移动 {} → {}", self.from.display(), self.to.display())
    }
    fn status(&self) -> OperationStatus {
        self.state.lock().status
    }
    fn progress(&self) -> (u64, u64) {
        let s = self.state.lock();
        (s.done, s.total)
    }
    fn cancel(&self) {
        self.state.lock().cancel = true;
    }
    fn pause(&self) {
        self.state.lock().pause = true;
    }
    fn resume(&self) {
        self.state.lock().pause = false;
    }
    fn pausable(&self) -> bool {
        true
    }
    fn run(&self) -> Result<(), MoError> {
        {
            let mut s = self.state.lock();
            if s.cancel {
                s.status = OperationStatus::Cancelled;
                return Ok(());
            }
            s.status = OperationStatus::Running;
        }

        // 目标已存在时按策略处理：rename 到已存在的路径会直接替换，不能不带策略地调用。
        let target = match resolve_target(&self.to, self.policy) {
            Target::Write(p) => p,
            Target::Skip => {
                let mut s = self.state.lock();
                s.status = OperationStatus::Completed;
                return Ok(());
            }
            Target::Abort => {
                let mut s = self.state.lock();
                s.status = OperationStatus::Failed;
                s.error = Some(format!("目标已存在：{}", self.to.display()));
                return Err(MoError::Other(s.error.clone().unwrap_or_default()));
            }
        };

        // 快路径：同文件系统 rename。覆盖策略下先删目标——
        // rename 到非空目录在部分平台会失败，直接删掉更可控。
        if self.policy == ConflictPolicy::Overwrite && target.exists() {
            let _ = remove_path(&target);
        }
        if std::fs::rename(&self.from, &target).is_ok() {
            let mut s = self.state.lock();
            s.status = OperationStatus::Completed;
            return Ok(());
        }

        // 跨设备：复制后删除源。
        match copy_tree(&self.from, &target, &self.state) {
            Ok(()) => match remove_path(&self.from) {
                Ok(()) => {
                    let mut s = self.state.lock();
                    s.status = OperationStatus::Completed;
                    s.done = s.total;
                }
                Err(e) => {
                    let mut s = self.state.lock();
                    s.status = OperationStatus::Failed;
                    s.error = Some(e.to_string());
                    return Err(MoError::Io(e));
                }
            },
            Err(e) => {
                let mut s = self.state.lock();
                if s.cancel {
                    s.status = OperationStatus::Cancelled;
                } else {
                    s.status = OperationStatus::Failed;
                    s.error = Some(e.to_string());
                }
                return Err(MoError::Io(e));
            }
        }
        Ok(())
    }
}
