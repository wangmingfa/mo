//! 传输队列 journal：崩掉 / 退出时把**没传完**的批次留在盘上，下次启动弹恢复卡。
//!
//! ## 为什么是进程级单例
//!
//! `AppState` 一页一个（`new_tab` → `AppState::new()`），而 journal 文件是
//! 进程全局的：两个标签页同时传文件，各自的内存副本交错重写同一份文件会互相
//! 覆盖（后完成的一方拿旧快照把新提交的批次抹掉）。所以记账统一收口在
//! [`journal_cell`] 的进程级表里，**按配置目录分桶**——测试把 `MO_CONFIG_DIR`
//! 钉到各自临时目录后互不可见。
//!
//! ## 记什么、何时进出
//!
//! * 进：`transfer_between` / `resolve_conflict` / `resolve_resume` 每提交一批
//!   就记一条（批次描述 + 各操作 handle id）。
//! * 出：每个操作完成时 [`complete`] 把它的 id 从批次里摘掉（id 摘空 = 整批
//!   出列）；启动恢复卡上「丢弃」显式移除。
//! * 重启后旧批次里的 id 属于**上一个进程**的操作计数，与新进程的 id 会撞号
//!   ——条目带 `epoch`（进程首用时刻），[`complete`] 只摘**本轮**的条目，
//!   陈年批次只能被恢复卡的处置（恢复 / 丢弃 / 留册）动。
//!
//! 恢复 = 按原批次描述重跑一遍 `transfer_between`（端点重建、挂载点重探、
//! 冲突 / 续传卡都走既有链路），不另发明第二条提交路径——见
//! [`crate::AppState::recover_transfers`]。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 一批没传完的传输：`transfer_between` 的批次描述 + 各操作的 handle id。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedTransfer {
    /// 批内各操作的 handle id：每完成一个摘一个，摘空整批出列。
    pub ids: Vec<u64>,
    /// 源路径列表（恢复时原样交回 `transfer_between`）。
    pub paths: Vec<PathBuf>,
    /// 源端点键（`RemoteUrl::endpoint()`；`None` = 本机路径——网络挂载点也在
    /// 这档：恢复时按路径重探 `net_endpoint`，不依赖会话实例）。
    pub src_ep: Option<String>,
    /// 目标目录。
    pub dest: PathBuf,
    /// 目标端点键（`None` 同上）。
    pub dest_ep: Option<String>,
    /// 是否移动。
    pub move_: bool,
    /// 记账进程的纪元（进程首用时刻的毫秒）：跨进程的 id 撞号屏障。
    pub epoch: u64,
    /// 恢复卡是否已经弹给用户看过——**只活在内存**（serde 挡在外面）：卡开着
    /// 的时候崩掉，批次还在盘上，下次启动再弹一次。多窗口只弹一次的闸门
    /// （`pending_transfers` 进出时置位）。`pub` 仅为 UI 测试预置载荷时能构造。
    #[serde(skip)]
    pub offered: bool,
}

/// 进程纪元：本轮进程 journal 首用时刻（毫秒）。文件里的旧条目带的是上一个
/// 进程的纪元，`complete` 只认当前纪元。
fn epoch() -> u64 {
    static EPOCH: OnceLock<u64> = OnceLock::new();
    *EPOCH.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// 测试专用的目录钉子（进程级）：集成测试与 UI 测试用它把 journal 指到自己的
/// 临时目录，绕开 `MO_CONFIG_DIR` 那把「只在构造期持锁」的进程级竞态。
/// 生产路径从不设置它，`dir()` 每次多查一个 `Option` 而已。
static TEST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 测试专用：钉住 / 解除 journal 的目录（`None` = 解除，回到环境变量判据）。
/// 见 [`TEST_DIR`]。集成测试进出都要清桶（桶按目录缓存），见
/// [`journal_cell`]。
#[doc(hidden)]
pub fn set_dir_for_tests(dir: Option<PathBuf>) {
    *TEST_DIR.lock().unwrap() = dir;
}

/// 配置目录（与 `AppState::config_path` 同一判据：测试钉子优先，`MO_CONFIG_DIR`
/// 次之）。
fn dir() -> PathBuf {
    if let Some(d) = TEST_DIR.lock().unwrap().clone() {
        return d;
    }
    if let Ok(d) = std::env::var("MO_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mo")
}

/// journal 文件路径（`<配置目录>/transfer-queue.json`——机器态，与 session.json
/// 同一「不进 config.json」的理由：它随传输增减，跟手改设置放一起会互相覆盖）。
fn path() -> PathBuf {
    dir().join("transfer-queue.json")
}

/// 进程级账本：按配置目录分桶（测试隔离靠它），桶内是还挂着的批次。
fn journal_cell() -> &'static Mutex<HashMap<PathBuf, Vec<QueuedTransfer>>> {
    static JOURNALS: OnceLock<Mutex<HashMap<PathBuf, Vec<QueuedTransfer>>>> = OnceLock::new();
    JOURNALS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 读盘上的批次列表（读不到 / 解析失败都当空——journal 只是锦上添花的账，
/// 坏了不该挡住应用启动）。
fn load() -> Vec<QueuedTransfer> {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 把内存里的批次列表写回盘上（父目录不存在就建——首跑时配置目录可能还没落盘）。
fn save(entries: &[QueuedTransfer]) {
    let p = path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(entries) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&p, text) {
                tracing::warn!("传输队列 journal 写盘失败：{e}");
            }
        }
        Err(e) => tracing::warn!("传输队列 journal 序列化失败：{e}"),
    }
}

/// 在当前配置目录的桶里改账：桶不存在先从盘上加载。**不自动落盘**——
/// 每个调用方按「是否真的变了」决定（`complete` 在大多数操作上无账可改，
/// 每次都重写文件是纯浪费）。
fn with_entries<T>(f: impl FnOnce(&mut Vec<QueuedTransfer>) -> T) -> T {
    let mut buckets = journal_cell().lock().unwrap();
    let entries = buckets.entry(dir()).or_insert_with(load);
    f(entries)
}

/// 批次描述相同（不看 ids / epoch）：恢复卡处置（移除 / 丢弃）的匹配键。
/// 快照到处置之间 ids 可能缩水（操作陆续完成），按 ids 比会漏。
fn same_batch(a: &QueuedTransfer, b: &QueuedTransfer) -> bool {
    a.paths == b.paths
        && a.dest == b.dest
        && a.move_ == b.move_
        && a.src_ep == b.src_ep
        && a.dest_ep == b.dest_ep
}

/// 记一批刚提交的传输（`ids` 为空 = 没有可等待的东西，不记）。
pub fn record_batch(
    paths: Vec<PathBuf>,
    src_ep: Option<String>,
    dest: PathBuf,
    dest_ep: Option<String>,
    move_: bool,
    ids: Vec<u64>,
) {
    if ids.is_empty() || paths.is_empty() {
        return;
    }
    with_entries(|entries| {
        entries.push(QueuedTransfer {
            ids,
            paths,
            src_ep,
            dest,
            dest_ep,
            move_,
            epoch: epoch(),
            offered: true, // 自己刚提交的，不用再弹恢复卡。
        });
        save(entries);
    });
}

/// 一个操作完成了：把它从**本轮**批次的 id 列表里摘掉，摘空整批出列。
///
/// 只认当前纪元——重启后新进程的操作 id 从 1 重数，不去碰上个进程留下的批次。
/// 无账可改（绝大多数操作不在册）就不写盘。
pub fn complete(id: u64) {
    let now = epoch();
    with_entries(|entries| {
        let mut changed = false;
        for b in entries.iter_mut() {
            if b.epoch == now && b.ids.contains(&id) {
                b.ids.retain(|&x| x != id);
                changed = true;
            }
        }
        if changed {
            entries.retain(|b| !(b.epoch == now && b.ids.is_empty()));
            save(entries);
        }
    });
}

/// 启动时还没处置过的批次（快照 + 进程内标记已弹出，**不动盘上的账**）：
/// 「没处置 = 再问」，比「弹过就再也不问」安全。多窗口不重复弹——
/// `offered` 标记留在桶里，第二个调用者拿到空表。
pub fn pending_transfers() -> Vec<QueuedTransfer> {
    let mut buckets = journal_cell().lock().unwrap();
    let entries = buckets.entry(dir()).or_insert_with(load);
    let pending: Vec<QueuedTransfer> = entries.iter().filter(|b| !b.offered).cloned().collect();
    for b in entries.iter_mut() {
        b.offered = true;
    }
    pending
}

/// 从账里移除指定批次（恢复重提前 / 用户丢弃时）。
pub fn remove_batches(batches: &[QueuedTransfer]) {
    with_entries(|entries| {
        let before = entries.len();
        entries.retain(|e| !batches.iter().any(|b| same_batch(e, b)));
        if entries.len() != before {
            save(entries);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// journal 的测试互斥：`set_dir_for_tests` 是进程级单槽，四个用例串行占用。
    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 把 journal 钉进临时目录跑一段（桶按目录缓存，进出各清一次）。
    fn in_dir(tag: &str, f: impl FnOnce()) {
        let _guard = test_lock();
        let d = std::env::temp_dir().join(format!("mo-journal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        journal_cell().lock().unwrap().remove(&d);
        set_dir_for_tests(Some(d.clone()));
        f();
        set_dir_for_tests(None);
        journal_cell().lock().unwrap().remove(&d);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn batch(ids: Vec<u64>, dest: &str) -> QueuedTransfer {
        QueuedTransfer {
            ids,
            paths: vec![PathBuf::from("/src/a.bin")],
            src_ep: None,
            dest: PathBuf::from(dest),
            dest_ep: None,
            move_: false,
            epoch: epoch(),
            offered: true,
        }
    }

    #[test]
    fn record_complete_and_empty_batch_exits_the_journal() {
        in_dir("rec-complete", || {
            record_batch(
                vec![PathBuf::from("/src/a.bin"), PathBuf::from("/src/b.bin")],
                None,
                PathBuf::from("/dst"),
                None,
                false,
                vec![7, 8],
            );
            complete(7);
            let left = journal_cell().lock().unwrap().get(&dir()).unwrap().clone();
            assert_eq!(left.len(), 1, "还有一个 id 挂着，批次必须在册");
            assert_eq!(left[0].ids, vec![8]);
            complete(8);
            let left = journal_cell().lock().unwrap().get(&dir()).unwrap().clone();
            assert!(left.is_empty(), "id 摘空 = 整批出列");
        });
    }

    #[test]
    fn complete_ignores_entries_from_other_epochs() {
        in_dir("rec-epoch", || {
            // 手工塞一条「上个进程」的批次：epoch 改掉，ids 里放个新进程会用的号。
            let mut stale = batch(vec![1, 2], "/dst-old");
            stale.epoch = epoch().wrapping_sub(1);
            with_entries(|entries| {
                entries.push(stale);
                save(entries);
            });
            // 本轮新进程里 id=1 的操作完成：不许碰旧批次的 id。
            complete(1);
            let left = journal_cell().lock().unwrap().get(&dir()).unwrap().clone();
            assert_eq!(left.len(), 1, "陈年批次不受新进程 id 影响");
            assert_eq!(left[0].ids, vec![1, 2], "旧批次的 id 一个都不许摘");
        });
    }

    #[test]
    fn pending_offers_once_then_stays_quiet_until_disposed() {
        in_dir("rec-pending", || {
            // 手工塞一条「上个进程」的批次（offered=false，模拟重启后加载）。
            let mut stale = batch(vec![1], "/dst-old");
            stale.epoch = epoch().wrapping_sub(1);
            stale.offered = false;
            with_entries(|entries| {
                entries.push(stale);
                save(entries);
            });

            let first = pending_transfers();
            assert_eq!(first.len(), 1, "首次要弹出来");
            let second = pending_transfers();
            assert!(second.is_empty(), "进程内只弹一次（多窗口不重复弹）");
            // 盘上的账没动：卡开着的时候崩掉，下次启动还会再问。
            let on_disk = load();
            assert_eq!(on_disk.len(), 1, "快照不落盘（没处置 = 再问）");

            // 处置（丢弃）之后才真正出列。
            remove_batches(&first);
            let on_disk = load();
            assert!(on_disk.is_empty());
        });
    }

    #[test]
    fn journal_survives_a_restart_via_the_file() {
        in_dir("rec-restart", || {
            record_batch(
                vec![PathBuf::from("/src/big.iso")],
                Some("sftp://10.0.0.9:22".to_string()),
                PathBuf::from("/remote/dst"),
                Some("sftp://10.0.0.9:22".to_string()),
                false,
                vec![42],
            );
            // 「重启」：把内存桶倒掉，从盘上重新加载（新进程的真实路径）。
            journal_cell().lock().unwrap().remove(&dir());
            let mut loaded = load();
            assert_eq!(loaded.len(), 1);
            loaded[0].offered = false; // 新进程里它还没弹过
            with_entries(|entries| *entries = loaded);
            let pending = pending_transfers();
            assert_eq!(pending.len(), 1);
            assert_eq!(
                pending[0].src_ep.as_deref(),
                Some("sftp://10.0.0.9:22"),
                "端点键要跨重启活下来"
            );
        });
    }
}
