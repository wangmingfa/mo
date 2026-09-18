//! link：创建符号链接 / 硬链接。
//!
//! 与 `RenameOperation` 一样是个「一瞬间完成」的操作，之所以仍走
//! `Operation` 接口：UI 侧统一用操作队列提交，进度面板 / 历史 / 失败提示
//! 都能复用同一条路径。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mo_core::MoError;
use parking_lot::Mutex;

use crate::{OpInner, Operation, OperationStatus};

/// 链接类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// 符号链接（可跨卷、可指向目录）。
    Symlink,
    /// 硬链接（同一卷内、不能指向目录）。
    Hardlink,
}

pub struct LinkOperation {
    id: u64,
    /// 链接目标的原路径。
    target: PathBuf,
    /// 要创建出来的链接路径。
    link: PathBuf,
    kind: LinkKind,
    state: Arc<Mutex<OpInner>>,
}

impl LinkOperation {
    pub fn new(id: u64, target: PathBuf, link: PathBuf, kind: LinkKind) -> Arc<Self> {
        Arc::new(Self {
            id,
            target,
            link,
            kind,
            state: Arc::new(Mutex::new(OpInner::new())),
        })
    }
}

impl Operation for LinkOperation {
    fn id(&self) -> u64 {
        self.id
    }
    fn describe(&self) -> String {
        let kind = if self.kind == LinkKind::Symlink {
            "符号链接"
        } else {
            "硬链接"
        };
        format!(
            "创建{} {} → {}",
            kind,
            self.link.display(),
            self.target.display()
        )
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
        let result = create_link(&self.target, &self.link, self.kind);
        let mut s = self.state.lock();
        match result {
            Ok(()) => {
                s.status = OperationStatus::Completed;
                Ok(())
            }
            Err(e) => {
                if s.cancel {
                    s.status = OperationStatus::Cancelled;
                    Ok(())
                } else {
                    s.status = OperationStatus::Failed;
                    s.error = Some(e.to_string());
                    Err(e)
                }
            }
        }
    }
}

/// 创建链接（阻塞；由操作队列在后台线程调用）。
pub fn create_link(target: &Path, link: &Path, kind: LinkKind) -> Result<(), MoError> {
    match kind {
        LinkKind::Symlink => {
            #[cfg(windows)]
            {
                let is_dir = target.is_dir();
                if is_dir {
                    std::os::windows::fs::symlink_dir(target, link).map_err(MoError::Io)
                } else {
                    std::os::windows::fs::symlink_file(target, link).map_err(MoError::Io)
                }
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(target, link).map_err(MoError::Io)
            }
        }
        LinkKind::Hardlink => std::fs::hard_link(target, link).map_err(MoError::Io),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_and_hardlink_roundtrip() {
        let dir = std::env::temp_dir().join("mo-link-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.txt");
        std::fs::write(&src, b"hello").unwrap();

        let sym = dir.join("a-sym.txt");
        create_link(&src, &sym, LinkKind::Symlink).unwrap();
        assert_eq!(std::fs::read_to_string(&sym).unwrap(), "hello");

        let hard = dir.join("a-hard.txt");
        create_link(&src, &hard, LinkKind::Hardlink).unwrap();
        assert_eq!(std::fs::read_to_string(&hard).unwrap(), "hello");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
