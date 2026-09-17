use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use mo_fs::FileSystem;

use crate::index::FileIndex;

/// 递归爬取 `root` 下的文件并写入索引。
///
/// 设计取舍：爬取阶段只取 `name / kind / path`（来自 [`mo_fs::FileSystem::read_dir_blocking`]），
/// **不逐个 stat**，因此即使几十万文件也很快；`size` / `modified` 留 0，
/// 后续可单独做「元数据补全」遍历（本阶段先不做，全局搜索以文件名为准）。
///
/// * `max_depth`：`0` 表示不限制；非 0 限制递归深度，避免索引整块磁盘时失控。
/// * `stop`：外部可置位中断（如用户切到别的目录）。
/// * `on_progress`：每处理一批（默认 500 条）回调一次已索引数量。
pub fn crawl(
    index: &mut FileIndex,
    fs: &dyn FileSystem,
    root: &Path,
    max_depth: usize,
    stop: &AtomicBool,
    mut on_progress: impl FnMut(usize),
) -> anyhow::Result<usize> {
    let mut counted = 0usize;
    crawl_dir(index, fs, root, 0, max_depth, stop, &mut on_progress, &mut counted)?;
    if counted > 0 {
        on_progress(counted);
    }
    Ok(counted)
}

#[allow(clippy::too_many_arguments)]
fn crawl_dir(
    index: &mut FileIndex,
    fs: &dyn FileSystem,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(usize),
    counted: &mut usize,
) -> anyhow::Result<()> {
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if max_depth > 0 && depth >= max_depth {
        return Ok(());
    }

    let entries = match fs.read_dir_blocking(dir) {
        Ok(e) => e,
        // 无权限 / 不是目录等：跳过该分支，不中断整体索引。
        Err(e) => {
            tracing::debug!("索引跳过 {dir:?}：{e}");
            return Ok(());
        }
    };

    for e in entries {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let _ = index.upsert(&e.path, &e.name, 0, None, e.kind.is_dir());
        *counted += 1;
        if *counted % 500 == 0 {
            on_progress(*counted);
        }
        if e.kind.is_dir() {
            crawl_dir(index, fs, &e.path, depth + 1, max_depth, stop, on_progress, counted)?;
        }
    }
    Ok(())
}
