use std::path::PathBuf;
use std::sync::Arc;

use mo_core::MoError;
use parking_lot::Mutex;

use crate::fs_util::remove_path;
use crate::{OpInner, Operation, OperationStatus};

/// 删除操作（当前为永久删除；回收站由 `mo-platform` 后续提供）。
pub struct DeleteOperation {
    id: u64,
    path: PathBuf,
    state: Arc<Mutex<OpInner>>,
}

impl DeleteOperation {
    pub fn new(id: u64, path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            id,
            path,
            state: Arc::new(Mutex::new(OpInner::new())),
        })
    }
}

impl Operation for DeleteOperation {
    fn id(&self) -> u64 {
        self.id
    }
    fn describe(&self) -> String {
        format!("删除 {}", self.path.display())
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
        match remove_path(&self.path) {
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
