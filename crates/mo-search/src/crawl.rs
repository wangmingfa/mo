use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mo_fs::FileSystem;
use parking_lot::Mutex;

use crate::index::FileIndex;

/// 一批写多少条：爬取攒够这么多就取一次索引锁、开一个事务写完、立刻放锁。
const BATCH: usize = 500;

/// 一条待写入索引的爬取结果（爬取阶段只知 name/kind/path，size/modified 留 0）。
pub struct CrawledEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// 递归爬取 `root` 下的文件并写入索引。
///
/// 设计取舍：爬取阶段只取 `name / kind / path`（来自 [`mo_fs::FileSystem::read_dir_blocking`]），
/// **不逐个 stat**，因此即使几十万文件也很快；`size` / `modified` 留 0，
/// 后续可单独做「元数据补全」遍历（本阶段先不做，全局搜索以文件名为准）。
///
/// `index` 是**加锁批量写**而不是全程持有：`crawl` 每攒满 [`BATCH`] 条才短暂取锁
/// 写一批。曾经整棵遍历握着一把锁（主目录三档就是几万条、几十秒），UI 主线程
/// 任何 `index_count` / `global_search` 都在锁上冻住——这就是「应用隔几秒卡一下、
/// 鼠标 hover 几秒没反应」的根源。IO（`read_dir`）也全在锁外。
///
/// * `max_depth`：`0` 表示不限制；非 0 限制递归深度，避免索引整块磁盘时失控。
/// * `skip_hidden`：`true` 时跳过隐藏条目（与「显示隐藏文件」开关同判据）。索引
///   里藏着的条目搜索也搜不到，所以这里必须与列表里的所见一致——否则用户会看到
///   「列表里没有、搜索却搜得出来」这种分裂。
/// * `stop`：外部可置位中断（如用户切到别的目录）。
/// * `limit`：**最多处理多少条**，`0` 表示不限。后台自举必须带上限——「进了一个
///   大目录」不该变成一次规模未知的几分钟爬取（主目录三层就是十万条量级）。
///   达到上限即停：索引是**可增量补齐**的，少爬一点下次再补，好过把机器占死。
/// * `on_progress`：每落盘一批回调一次已处理数量。
// 参数是多了点，但这是条「一次爬完一棵树」的底层入口，全是正交开关；拆成
// builder 只会让调用方更难读。
#[allow(clippy::too_many_arguments)]
pub fn crawl(
    index: &Mutex<FileIndex>,
    fs: &dyn FileSystem,
    root: &Path,
    max_depth: usize,
    skip_hidden: bool,
    limit: usize,
    stop: &AtomicBool,
    mut on_progress: impl FnMut(usize),
) -> anyhow::Result<usize> {
    let mut counted = 0usize;
    let mut batch: Vec<CrawledEntry> = Vec::with_capacity(BATCH);
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
        &mut batch,
    )?;
    if !batch.is_empty() {
        flush(index, &mut batch, &mut on_progress, counted);
    }
    if counted > 0 {
        on_progress(counted);
    }
    Ok(counted)
}

/// 把攒下的一批写进索引：取锁 → 一个事务写完 → **放锁**，再报进度。
fn flush(
    index: &Mutex<FileIndex>,
    batch: &mut Vec<CrawledEntry>,
    on_progress: &mut dyn FnMut(usize),
    counted: usize,
) {
    if let Err(e) = index.lock().upsert_batch(batch) {
        tracing::debug!("索引批量写入失败（{} 条）：{e}", batch.len());
    }
    batch.clear();
    on_progress(counted);
}

#[allow(clippy::too_many_arguments)]
fn crawl_dir(
    index: &Mutex<FileIndex>,
    fs: &dyn FileSystem,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    skip_hidden: bool,
    limit: usize,
    stop: &AtomicBool,
    on_progress: &mut dyn FnMut(usize),
    counted: &mut usize,
    batch: &mut Vec<CrawledEntry>,
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
        let is_dir = e.kind.is_dir();
        batch.push(CrawledEntry {
            path: e.path.clone(),
            name: e.name.clone(),
            is_dir,
        });
        *counted += 1;
        if batch.len() >= BATCH {
            flush(index, batch, on_progress, *counted);
        }
        if is_dir {
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
                batch,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use mo_fs::LocalFileSystem;

    use super::*;
    use crate::index::FileIndex;

    /// 爬取**不得**横跨整棵遍历持有索引锁：曾经全程握着 `index.lock()` 爬几万条，
    /// UI 主线程的 `index_count()`（同一把锁）在自举期间隔几秒冻几秒。
    /// 进度回调在每批落盘之后、锁已释放时同线程调用——能立刻取到锁即证明这一点。
    #[test]
    fn crawl_releases_the_lock_between_batches() {
        let dir = std::env::temp_dir().join(format!("mo-crawl-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 两倍批量以上：保证至少发生一次「锁已放下、回调正在跑」。
        for i in 0..1200 {
            std::fs::write(dir.join(format!("f{i:04}")), b"x").unwrap();
        }

        let index = Mutex::new(FileIndex::open_in_memory().unwrap());
        let stop = AtomicBool::new(false);
        let mut callbacks = 0usize;
        let n = crawl(
            &index,
            &LocalFileSystem,
            &dir,
            0,
            false,
            0,
            &stop,
            |counted| {
                callbacks += 1;
                assert!(
                    index.try_lock().is_some(),
                    "报进度时索引锁应已释放（已处理 {counted} 条）"
                );
            },
        )
        .unwrap();
        assert_eq!(n, 1200);
        assert!(callbacks >= 2, "每批回调一次，1200 条至少 2 次");
        assert_eq!(index.lock().count(), 1200);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 批量写撞上有记录的旧行时**不**把 size/modified 抹成 0（爬取阶段不知道它们）。
    #[test]
    fn upsert_batch_keeps_existing_metadata_on_conflict() {
        let mut i = FileIndex::open_in_memory().unwrap();
        i.upsert(Path::new("/a/x.txt"), "x.txt", 42, Some(7), false)
            .unwrap();
        i.upsert_batch(&[CrawledEntry {
            path: PathBuf::from("/a/x.txt"),
            name: "x.txt".into(),
            is_dir: false,
        }])
        .unwrap();
        let hits = i.search("x.txt", 10).unwrap();
        assert_eq!(hits[0].size, 42);
        assert_eq!(hits[0].modified, Some(7));

        // 新行照常插入。
        i.upsert_batch(&[CrawledEntry {
            path: PathBuf::from("/a/y.txt"),
            name: "y.txt".into(),
            is_dir: false,
        }])
        .unwrap();
        assert_eq!(i.count(), 2);
    }
}
