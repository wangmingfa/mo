use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::OpInner;

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

/// 递归复制 `from` → `to`，按字节累计 `total` / `done`，并尊重取消标记。
pub(crate) fn copy_tree(from: &Path, to: &Path, state: &Arc<Mutex<OpInner>>) -> io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let p = entry.path();
            let dest = to.join(entry.file_name());
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
        if state.lock().cancel {
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
