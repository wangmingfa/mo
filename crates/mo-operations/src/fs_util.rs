use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::{OpInner, OperationStatus};

/// 取消导致的 IO 错误。
pub(crate) fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "operation cancelled")
}

/// 目标已存在时的处理策略。
///
/// 默认是 [`ConflictPolicy::Rename`]：**永不静默覆盖**。
/// `std::fs::rename` / `fs::write` 在目标存在时都会直接替换，
/// 一次误操作就是不可逆的数据丢失，因此默认走「另存为新名字」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// 自动改名：`a.txt` → `a 2.txt` → `a 3.txt`。
    #[default]
    Rename,
    /// 保留目标，跳过这个源文件。
    Skip,
    /// 覆盖目标（有数据丢失风险，需用户显式选择）。
    Overwrite,
    /// 整个操作失败。
    Abort,
}

/// 目标冲突时的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 写入这个路径。
    Write(PathBuf),
    /// 跳过（目标已存在且策略为 Skip）；操作视为成功但什么都不做。
    Skip,
    /// 中止整个操作。
    Abort,
}

/// 按策略解析目标路径。
pub fn resolve_target(to: &Path, policy: ConflictPolicy) -> Target {
    if !to.exists() {
        return Target::Write(to.to_path_buf());
    }
    match policy {
        ConflictPolicy::Overwrite => Target::Write(to.to_path_buf()),
        ConflictPolicy::Skip => Target::Skip,
        ConflictPolicy::Abort => Target::Abort,
        ConflictPolicy::Rename => Target::Write(unique_path(to)),
    }
}

/// 生成一个不冲突的路径：`a.txt` → `a 2.txt` → `a 3.txt`。
pub fn unique_path(to: &Path) -> PathBuf {
    let Some(parent) = to.parent() else {
        return to.to_path_buf();
    };
    let stem = to
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = to
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    for i in 2..10_000 {
        let candidate = parent.join(format!("{stem} {i}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // 理论上到不了这里；兜底用时间戳避免覆盖。
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!("{stem} {nanos}{ext}"))
}

/// 阻塞当前线程直到恢复或取消（`copy_tree` 的检查点用）。
///
/// 暂停是**协作式**的：`pause()` 只置标记，真正停下来是在传输循环的下个检查点。
/// 进入等待时把状态标成 [`OperationStatus::Paused`]（进度面板才显示「已暂停」），
/// 恢复后标回 `Running`。返回 `true` 表示等待期间收到了取消——调用方直接走
/// 取消分支（终态由 `run()` 收口时判定）。
pub(crate) fn wait_if_paused(state: &Arc<Mutex<OpInner>>) -> bool {
    let mut paused_shown = false;
    loop {
        {
            let mut s = state.lock();
            if s.cancel {
                return true;
            }
            if !s.pause {
                if paused_shown {
                    s.status = OperationStatus::Running;
                }
                return false;
            }
            if !paused_shown {
                s.status = OperationStatus::Paused;
                paused_shown = true;
            }
        }
        // 轮询间隔：暂停不是热路径，50ms 的恢复延迟肉眼无感，也不空转烧 CPU。
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// 递归复制 `from` → `to`，按字节累计 `total` / `done`，并尊重取消 / 暂停标记。
pub(crate) fn copy_tree(from: &Path, to: &Path, state: &Arc<Mutex<OpInner>>) -> io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let p = entry.path();
            let dest = to.join(entry.file_name());
            if wait_if_paused(state) {
                return Err(cancelled());
            }
            copy_tree(&p, &dest, state)?;
            if state.lock().cancel {
                return Err(cancelled());
            }
        }
    } else {
        if let Some(parent) = to.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        if wait_if_paused(state) {
            return Err(cancelled());
        }
        let data = std::fs::read(from)?;
        std::fs::write(to, &data)?;
        {
            let mut s = state.lock();
            s.total += data.len() as u64;
            s.done += data.len() as u64;
        }
    }
    Ok(())
}

/// 删除文件或（递归）目录。
pub(crate) fn remove_path(p: &Path) -> io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// 移动 `from` → `to`：优先 `rename`（同文件系统快路径），
/// 跨设备时退回「复制 + 删除源」。会创建 `to` 的父目录。
///
/// 进度不对外暴露（回收站 / 撤销用的移动通常很快），因此传入一个临时状态。
pub(crate) fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // 跨设备：复制后删除源。
    let state = Arc::new(Mutex::new(OpInner::new()));
    copy_tree(from, to, &state)?;
    remove_path(from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 暂停的语义：阻塞中的检查点把状态标成 Paused；恢复后标回 Running；
    /// 取消能让等待方立即返回 true（终态 Cancelled 由 run() 收口时判定）。
    #[test]
    fn wait_if_paused_blocks_until_resume_or_cancel() {
        let state = Arc::new(Mutex::new(OpInner::new()));
        state.lock().status = OperationStatus::Running;
        state.lock().pause = true;

        let s2 = state.clone();
        let t = std::thread::spawn(move || wait_if_paused(&s2));
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            state.lock().status,
            OperationStatus::Paused,
            "暂停中应标成 Paused"
        );
        assert!(!t.is_finished(), "没恢复就不该解除阻塞");

        // 恢复：等待方返回 false，状态标回 Running。
        state.lock().pause = false;
        assert!(!t.join().unwrap(), "恢复后应正常放行");
        assert_eq!(state.lock().status, OperationStatus::Running);

        // 取消：阻塞中的等待方立即返回 true。
        state.lock().pause = true;
        let s3 = state.clone();
        let t2 = std::thread::spawn(move || wait_if_paused(&s3));
        std::thread::sleep(Duration::from_millis(120));
        state.lock().cancel = true;
        assert!(t2.join().unwrap(), "取消应让等待方立即返回");
    }
}
