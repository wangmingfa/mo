use std::path::PathBuf;
use std::sync::Arc;

use mo_core::MoError;
use parking_lot::Mutex;

use crate::{OpInner, Operation, OperationStatus};

/// 重命名操作（单文件 / 目录）。
pub struct RenameOperation {
    id: u64,
    from: PathBuf,
    to: PathBuf,
    state: Arc<Mutex<OpInner>>,
}

impl RenameOperation {
    pub fn new(id: u64, from: PathBuf, to: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            id,
            from,
            to,
            state: Arc::new(Mutex::new(OpInner::new())),
        })
    }
}

impl Operation for RenameOperation {
    fn id(&self) -> u64 {
        self.id
    }
    fn describe(&self) -> String {
        format!("重命名 {} → {}", self.from.display(), self.to.display())
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
        match std::fs::rename(&self.from, &self.to) {
            Ok(()) => {
                let mut s = self.state.lock();
                s.status = OperationStatus::Completed;
            }
            Err(e) => {
                let mut s = self.state.lock();
                if s.cancel {
                    s.status = OperationStatus::Cancelled;
                } else {
                    s.status = OperationStatus::Failed;
                    s.error = Some(e.to_string());
                    return Err(MoError::Io(e));
                }
            }
        }
        Ok(())
    }
}
