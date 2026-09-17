use std::path::PathBuf;
use std::sync::Arc;

use mo_core::MoError;
use parking_lot::Mutex;

use crate::trash::Trash;
use crate::trash_op::to_mo;
use crate::{Operation, OperationStatus, OpInner};

/// 从回收站按「原路径」还原最近一条记录（用于撤销「删除」）。
pub struct RestoreOperation {
    id: u64,
    original: PathBuf,
    trash: Arc<Trash>,
    state: Arc<Mutex<OpInner>>,
}

impl RestoreOperation {
    pub fn new(id: u64, original: PathBuf, trash: Arc<Trash>) -> Arc<Self> {
        Arc::new(Self {
            id,
            original,
            trash,
            state: Arc::new(Mutex::new(OpInner::new())),
        })
    }
}

impl Operation for RestoreOperation {
    fn id(&self) -> u64 {
        self.id
    }
    fn describe(&self) -> String {
        format!("还原（回收站） {}", self.original.display())
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
        match self.trash.restore_by_original(&self.original) {
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
