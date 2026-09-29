//! 远程目录的轮询刷新（`AppState::spawn_remote_poll_pump` / `poll_remote_listing_once`）。
//!
//! 远程协议没有事件推送：另一台设备写入、本进程外的操作，用户停在目录上时是
//! 看不见的。轮询泵按固定间隔重读**当前远程目录**，与模型比对——**有差异才整
//! 目录重读**（闲着的目录每拍只花一次列目录的网络往返，零 UI 动作）。
//!
//! 泵的 5s 节拍在 headless 测试里等不起，这里直接驱动 `poll_remote_listing_once`
//! 这一轮。它要钉住的判据：
//!
//! 1. **别的设备新建 / 删除的条目，下一拍就出现在 / 消失于列表**——这是这条泵
//!    存在的全部理由。
//! 2. **列表没变就不重读**：一次轮询只花一次列目录往返（计数 +1），不触发
//!    整目录刷新（那会再多一次往返）。没有这条，泵就成了持续的无谓流量。
//! 3. **读失败（连接抖了）这一轮就算了**：不 panic、列表不动、也不触发刷新，
//!    自愈与报错都交给下一次真正的导航。
//!
//! 为什么守卫落在 `mo-app` 而不是 UI 层：headless 的 GPUI 测试调度器会把「后台
//! tokio 线程唤醒测试任务」判成不确定性直接 panic（见 `verify-gpui-layout-headless`）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mo_app::{AppState, SessionRegistry};
use mo_core::{EntryKind, FileId, FileMetadata, MoError};
use mo_fs::{FileSystem, ReadDirEntry};
use mo_remote::RemoteUrl;

mod common;

/// 测试用的远程地址（与 remote_local.rs 同款：真去连就干脆地拒绝）。
const TEST_URL: &str = "ftp://127.0.0.1:1";

/// 共享的可变列表：测试在两次轮询之间改它，扮演「另一台设备写入」。
type Listing = Arc<Mutex<Vec<ReadDirEntry>>>;

/// 列目录被调用的次数——「没变就不重读」的观测点。
type ReadCount = Arc<AtomicUsize>;

/// 「连接断了」的开关把手——测试在网络抖动那一侧。
type FailSwitch = Arc<AtomicBool>;

/// 列表由测试外部掌控的假远程后端：`/` 下的条目就是 `listing` 里那份。
struct PollFakeFs {
    listing: Listing,
    reads: ReadCount,
    fail: FailSwitch,
}

impl PollFakeFs {
    /// 后端 + 三个外部把手（列表 / 读计数 / 断连开关）。
    fn new(entries: Vec<ReadDirEntry>) -> (Self, Listing, ReadCount, FailSwitch) {
        let listing: Listing = Arc::new(Mutex::new(entries));
        let reads: ReadCount = Arc::new(AtomicUsize::new(0));
        let fail: FailSwitch = Arc::new(AtomicBool::new(false));
        (
            Self {
                listing: listing.clone(),
                reads: reads.clone(),
                fail: fail.clone(),
            },
            listing,
            reads,
            fail,
        )
    }

    fn file(name: &str) -> ReadDirEntry {
        let path = PathBuf::from(format!("/{name}"));
        ReadDirEntry::new(
            FileId::synthetic(&path),
            name.to_string(),
            EntryKind::File,
            path.clone(),
        )
    }
}

#[async_trait]
impl FileSystem for PollFakeFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.read_dir_blocking(path)
    }

    fn is_alive(&self) -> bool {
        !self.fail.load(Ordering::SeqCst)
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(mo_remote::RemoteError::disconnected(
                "列目录",
                "Broken pipe (os error 32)",
            )
            .into());
        }
        if path.to_str() == Some("/") {
            return Ok(self.listing.lock().unwrap().clone());
        }
        Err(MoError::Other(format!(
            "远端没有这样的目录：{}",
            path.display()
        )))
    }

    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError> {
        let known = self.listing.lock().unwrap().iter().any(|e| e.path == path);
        if known {
            Ok(FileMetadata {
                size: 0,
                modified: None,
                created: None,
                permissions: mo_core::Permissions::default(),
            })
        } else {
            Err(MoError::Other("假远程后端不支持这个操作".to_string()))
        }
    }

    // 轮询是纯读路径，下面的改动类方法用不到，桩返回不支持即可。
    async fn create_dir(&self, _path: &Path) -> Result<(), MoError> {
        Err(MoError::Other("假远程后端不支持这个操作".to_string()))
    }

    async fn write_file(&self, _path: &Path, _contents: &[u8]) -> Result<(), MoError> {
        Err(MoError::Other("假远程后端不支持这个操作".to_string()))
    }

    async fn write_file_chunk(
        &self,
        _path: &Path,
        _offset: u64,
        _data: &[u8],
    ) -> Result<(), MoError> {
        Err(MoError::Other("假远程后端不支持这个操作".to_string()))
    }

    async fn remove_file(&self, _path: &Path) -> Result<(), MoError> {
        Err(MoError::Other("假远程后端不支持这个操作".to_string()))
    }

    async fn remove_dir(&self, _path: &Path) -> Result<(), MoError> {
        Err(MoError::Other("假远程后端不支持这个操作".to_string()))
    }

    async fn rename(&self, _from: &Path, _to: &Path) -> Result<(), MoError> {
        Err(MoError::Other("假远程后端不支持这个操作".to_string()))
    }
}

/// 假连接 + 停在远程根目录的 `AppState`，连同三个外部把手。
struct Scene {
    app: AppState,
    listing: Listing,
    reads: ReadCount,
    fail: FailSwitch,
}

async fn scene(tag: &str) -> Scene {
    // ⚠️ 独占的会话表（不是 `AppState::new` 的进程级那张）：同二进制的用例并行跑，
    // 同端点 + 同用户名在进程级表里会被去重成一行——后装的假后端把先装的顶掉，
    // 各用例的「外部改动」就互相窜了（单跑永远绿，一轮全红那种）。
    let seq = {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        SEQ.fetch_add(1, Ordering::SeqCst)
    };
    let trash = std::env::temp_dir().join(format!("mo-trash-poll-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&trash);
    let app = common::isolated(tag, || {
        AppState::with_sessions(trash, Arc::new(SessionRegistry::new()))
    });
    let (fs, listing, reads, fail) = PollFakeFs::new(vec![PollFakeFs::file("seed.txt")]);
    app.install_backend_for_test(Arc::new(fs), TEST_URL);
    app.open_directory(Path::new("/"))
        .await
        .expect("打开远程根目录");
    Scene {
        app,
        listing,
        reads,
        fail,
    }
}

/// 另一台设备新建的文件，下一拍出现在列表里。
#[tokio::test]
async fn poll_picks_up_an_entry_created_elsewhere() {
    let s = scene("poll-created").await;
    assert!(
        !s.app
            .current_entries()
            .await
            .iter()
            .any(|e| e.path == Path::new("/late.txt")),
        "前提：另一台设备还没写"
    );

    // 「另一台设备」写了一个文件。
    s.listing.lock().unwrap().push(PollFakeFs::file("late.txt"));
    s.app.poll_remote_listing_once().await;

    assert!(
        s.app
            .current_entries()
            .await
            .iter()
            .any(|e| e.path == Path::new("/late.txt")),
        "下一拍，外部新建的条目应当出现在列表里"
    );
}

/// 另一台设备删掉的文件，下一拍从列表里消失。
#[tokio::test]
async fn poll_drops_an_entry_removed_elsewhere() {
    let s = scene("poll-removed").await;
    assert!(
        s.app
            .current_entries()
            .await
            .iter()
            .any(|e| e.path == Path::new("/seed.txt")),
        "前提：seed 还在"
    );

    s.listing
        .lock()
        .unwrap()
        .retain(|e| e.path != Path::new("/seed.txt"));
    s.app.poll_remote_listing_once().await;

    assert!(
        !s.app
            .current_entries()
            .await
            .iter()
            .any(|e| e.path == Path::new("/seed.txt")),
        "下一拍，外部删掉的条目应当从列表里消失"
    );
}

/// 列表没变就不重读：一次轮询只花一次列目录往返，不触发整目录刷新。
#[tokio::test]
async fn poll_skips_the_refresh_when_the_listing_is_unchanged() {
    let s = scene("poll-unchanged").await;
    let after_open = s.reads.load(Ordering::SeqCst);
    assert!(after_open >= 1, "打开目录至少读了一次");

    // 变了：比对读一次 + 刷新重读一次。
    s.listing.lock().unwrap().push(PollFakeFs::file("late.txt"));
    s.app.poll_remote_listing_once().await;
    assert_eq!(
        s.reads.load(Ordering::SeqCst),
        after_open + 2,
        "有差异：比对一次 + 整目录重读一次"
    );

    // 没变：只比对，不再重读。
    s.app.poll_remote_listing_once().await;
    s.app.poll_remote_listing_once().await;
    assert_eq!(
        s.reads.load(Ordering::SeqCst),
        after_open + 4,
        "没差异的每一拍只多一次列目录往返"
    );
}

/// 读失败（连接抖了）这一轮就算了：不 panic、列表不动、也不触发刷新。
#[tokio::test]
async fn poll_survives_a_failing_backend() {
    let s = scene("poll-fail").await;
    let before = s.reads.load(Ordering::SeqCst);

    s.fail.store(true, Ordering::SeqCst);
    s.app.poll_remote_listing_once().await;

    assert_eq!(
        s.reads.load(Ordering::SeqCst),
        before + 1,
        "这一拍只花了「比对」那一次往返（失败后不重读）"
    );
    assert!(
        s.app
            .current_entries()
            .await
            .iter()
            .any(|e| e.path == Path::new("/seed.txt")),
        "失败那一拍列表不动"
    );

    // 连接「恢复」后，积累的外部改动在下一拍照常进来。
    s.fail.store(false, Ordering::SeqCst);
    s.listing.lock().unwrap().push(PollFakeFs::file("late.txt"));
    s.app.poll_remote_listing_once().await;
    assert!(
        s.app
            .current_entries()
            .await
            .iter()
            .any(|e| e.path == Path::new("/late.txt")),
        "自愈之后的下一拍应当照常抓到外部改动"
    );
}

/// `RemoteUrl` 在本文件里只被 `install_backend_for_test` 的解析路径用到；
/// 显式引用防「未用导入」告警（解析发生在 mo-app 内部）。
#[allow(dead_code)]
fn _url_is_used(_: RemoteUrl) {}
