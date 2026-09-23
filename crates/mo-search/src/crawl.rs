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
/// * `skip_hidden`：`true` 时跳过隐藏条目（与「显示隐藏文件」开关同判据）。索引
///   里藏着的条目搜索也搜不到，所以这里必须与列表里的所见一致——否则用户会看到
///   「列表里没有、搜索却搜得出来」这种分裂。
/// * `stop`：外部可置位中断（如用户切到别的目录）。
/// * `limit`：**最多处理多少条**，`0` 表示不限。后台自举必须带上限——「进了一个
///   大目录」不该变成一次规模未知的几分钟爬取（主目录三层就是十万条量级）。
///   达到上限即停：索引是**可增量补齐**的，少爬一点下次再补，好过把机器占死。
/// * `on_progress`：每处理一批（默认 500 条）回调一次已索引数量。
// 参数是多了点，但这是条「一次爬完一棵树」的底层入口，全是正交开关；拆成
// builder 只会让调用方更难读。
#[allow(clippy::too_many_arguments)]
pub fn crawl(
    index: &mut FileIndex,
    fs: &dyn FileSystem,
    root: &Path,
    max_depth: usize,
    skip_hidden: bool,
    limit: usize,
    stop: &AtomicBool,
    mut on_progress: impl FnMut(usize),
) -> anyhow::Result<usize> {
    let mut counted = 0usize;
    crawl_dir(
        index,
        fs,
        root,
        0,
        max_depth,
        skip_hidden,
        limit,
        stop,
        &mut on_progress,
        &mut counted,
    )?;
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
    skip_hidden: bool,
    limit: usize,
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
    if limit > 0 && *counted >= limit {
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
        // 隐藏条目整棵子树都不进索引：`.git` / `node_modules` 这种目录动辄几万
        // 个文件，滤掉它们既是「所见即所搜」，也让索引体积小一个数量级。
        if skip_hidden && e.hidden {
            continue;
        }
        let _ = index.upsert(&e.path, &e.name, 0, None, e.kind.is_dir());
        *counted += 1;
        if (*counted).is_multiple_of(500) {
            on_progress(*counted);
        }
        if e.kind.is_dir() {
            crawl_dir(
                index,
                fs,
                &e.path,
                depth + 1,
                max_depth,
                skip_hidden,
                limit,
                stop,
                on_progress,
                counted,
            )?;
        }
    }
    Ok(())
}
