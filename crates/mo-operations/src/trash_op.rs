use std::path::PathBuf;
use std::sync::Arc;

use mo_core::MoError;
use parking_lot::Mutex;

use crate::trash::{Trash, TrashError};
use crate::{OpInner, Operation, OperationStatus};

/// 把文件 / 目录移入回收站（而不是永久删除）。
///
/// 这样「删除」就天然可撤销——撤销时只需按原路径从回收站还原。
pub struct TrashOperation {
    id: u64,
    path: PathBuf,
    trash: Arc<Trash>,
    state: Arc<Mutex<OpInner>>,
}

impl TrashOperation {
    pub fn new(id: u64, path: PathBuf, trash: Arc<Trash>) -> Arc<Self> {
        Arc::new(Self {
            id,
            path,
            trash,
            state: Arc::new(Mutex::new(OpInner::new())),
        })
    }
}

impl Operation for TrashOperation {
    fn id(&self) -> u64 {
        self.id
    }
    fn describe(&self) -> String {
        format!("删除（回收站） {}", self.path.display())
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
        match self.trash.trash(&self.path) {
            Ok(_) => {
                let mut s = self.state.lock();
                s.status = OperationStatus::Completed;
            }
            Err(e) => {
                let mut s = self.state.lock();
                s.status = OperationStatus::Failed;
                s.error = Some(e.to_string());
                return Err(to_mo(e));
            }
        }
        Ok(())
    }
}

/// 把回收站错误映射到 `MoError`。
pub(crate) fn to_mo(e: TrashError) -> MoError {
    match e {
        TrashError::Io(io) => MoError::Io(io),
        TrashError::Json(j) => MoError::Other(j.to_string()),
    }
}
