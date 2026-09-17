use std::path::PathBuf;
use std::sync::Arc;

use mo_core::MoError;
use parking_lot::Mutex;

use crate::fs_util::{copy_tree, resolve_target, ConflictPolicy, Target};
use crate::{OpInner, Operation, OperationStatus};

/// 复制操作（支持目录递归、取消、进度、目标冲突处理）。
pub struct CopyOperation {
    id: u64,
    from: PathBuf,
    to: PathBuf,
    policy: ConflictPolicy,
    state: Arc<Mutex<OpInner>>,
}

impl CopyOperation {
    /// 新建复制操作，默认冲突策略为「自动改名」（永不静默覆盖）。
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

impl Operation for CopyOperation {
    fn id(&self) -> u64 {
        self.id
    }
    fn describe(&self) -> String {
        format!("复制 {} → {}", self.from.display(), self.to.display())
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
    fn run(&self) -> Result<(), MoError> {
        {
            let mut s = self.state.lock();
            if s.cancel {
                s.status = OperationStatus::Cancelled;
                return Ok(());
            }
            s.status = OperationStatus::Running;
        }
        // 目标已存在时按策略处理，绝不静默覆盖。
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

        let result = copy_tree(&self.from, &target, &self.state);
        let mut s = self.state.lock();
        match result {
            Ok(()) => {
                s.status = OperationStatus::Completed;
                s.done = s.total;
            }
            Err(e) => {
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
