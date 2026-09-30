//! 传输的**冲突探测与决策**（`transfer_between` → `PendingConflict` →
//! `resolve_conflict`）——远程 leg（下面这批用例）与**两端都本地**（文末那批）
//! 走同一条：2026-09-29 起本机复制 / 移动撞上同名条目也弹这张卡，不再闷声
//! 改名成 `a 2.txt`。
//!
//! 目标端已有同名条目时**整批不提交**，交回确认卡让用户选覆盖 / 改名 / 跳过
//! （取消 = 丢弃请求什么都不传）。批里可能并存「部分完成」的文件：冲突优先
//! 弹卡，但非冲突文件在决策提交时仍按续传判据走（引擎侧会兜底）。
//!
//! 分类判据与续传共用一次 `metadata` 探测：`0 < 已传 < 源大小` 算续传候选，
//! 其余已存在（完整同名、更大、目录）算冲突——目标比源**短**的完整文件按
//! 「没传完」续传处理，这是断点续传的语义而不是冲突。本地对只判冲突不判续传
//! （本机复制不落「部分完成」的中间文件）。
//!
//! 远程用例直接传 `Endpoint::Remote(假后端)`，不需要 SessionRegistry 基建；
//! 本地用例传 `Endpoint::Local` 加一对真实临时目录。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mo_app::{AppState, ConflictDecision, Endpoint, PendingConflict, TransferOutcome};
use mo_core::{FileId, FileMetadata, MoError};
use mo_fs::{FileSystem, ReadDirEntry};

mod common;

/// 测试载荷：远小于 CHUNK_SIZE（4MiB），整份一块传完。
const PAYLOAD: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// 比 PAYLOAD 长的「目标端已有同名文件」——比源大，判成冲突而不是续传候选。
const FOREIGN: &[u8] = b"I am a longer foreign file that was here before the copy";

#[derive(Default)]
struct Mem {
    dirs: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

/// 内存假后端：与 resume.rs 的 RecFs 同款（区间替换写、按块读）。
#[derive(Clone, Default)]
struct RecFs {
    inner: Arc<Mutex<Mem>>,
}

impl RecFs {
    fn put(&self, path: &str, bytes: &[u8]) {
        let mut m = self.inner.lock().unwrap();
        for dir in parents(path) {
            m.dirs.insert(dir);
        }
        m.files.insert(PathBuf::from(path), bytes.to_vec());
    }

    fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.inner
            .lock()
            .unwrap()
            .files
            .get(&PathBuf::from(path))
            .cloned()
    }

    fn read_dir_inner(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        let m = self.inner.lock().unwrap();
        if !m.dirs.contains(path) {
            return Err(MoError::Other(format!("不是目录：{}", path.display())));
        }
        let mut out = Vec::new();
        for p in m.files.keys() {
            if p.parent() == Some(path) {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                out.push(ReadDirEntry::new(
                    FileId::synthetic(p),
                    name,
                    mo_core::EntryKind::File,
                    p.clone(),
                ));
            }
        }
        Ok(out)
    }

    /// 轮询等后台操作把 `path` 写成 `want`（提交只入队，落盘在 blocking 池）。
    async fn wait_bytes(&self, path: &str, want: &[u8]) {
        for _ in 0..250 {
            if self.bytes(path).as_deref() == Some(want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("等了 5s，{path} 还没写成期望内容");
    }
}

/// 路径的各级父目录（`/a/b/c.txt` → `/a`、`/a/b`）。
fn parents(path: &str) -> Vec<PathBuf> {
    let p = PathBuf::from(path);
    let mut out = Vec::new();
    let mut cur = p.parent();
    while let Some(c) = cur {
        if c.as_os_str().is_empty() {
            break;
        }
        out.push(c.to_path_buf());
        cur = c.parent();
    }
    out
}

#[async_trait]
impl FileSystem for RecFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.read_dir_inner(path)
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.read_dir_inner(path)
    }

    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError> {
        let m = self.inner.lock().unwrap();
        let f = m
            .files
            .get(path)
            .ok_or_else(|| MoError::Other(format!("没有这个文件：{}", path.display())))?;
        Ok(FileMetadata {
            size: f.len() as u64,
            modified: None,
            created: None,
            permissions: mo_core::Permissions::default(),
        })
    }

    async fn create_dir(&self, path: &Path) -> Result<(), MoError> {
        self.inner.lock().unwrap().dirs.insert(path.to_path_buf());
        Ok(())
    }

    async fn remove_file(&self, path: &Path) -> Result<(), MoError> {
        self.inner
            .lock()
            .unwrap()
            .files
            .remove(path)
            .ok_or_else(|| MoError::Other(format!("没有这个文件：{}", path.display())))?;
        Ok(())
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), MoError> {
        self.inner.lock().unwrap().dirs.remove(path);
        Ok(())
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), MoError> {
        let mut m = self.inner.lock().unwrap();
        let data = m
            .files
            .remove(from)
            .ok_or_else(|| MoError::Other(format!("没有这个文件：{}", from.display())))?;
        m.files.insert(to.to_path_buf(), data);
        Ok(())
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, MoError> {
        self.metadata(path).await?;
        Ok(self.inner.lock().unwrap().files.get(path).cloned().unwrap())
    }

    async fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), MoError> {
        self.write_file_chunk(path, 0, contents).await
    }

    async fn read_file_chunk(
        &self,
        path: &Path,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, MoError> {
        let m = self.inner.lock().unwrap();
        match m.files.get(path) {
            Some(f) => {
                let start = offset as usize;
                if start >= f.len() {
                    return Ok(Vec::new());
                }
                let end = (start + len as usize).min(f.len());
                Ok(f[start..end].to_vec())
            }
            None => Err(MoError::Other(format!("不是文件：{}", path.display()))),
        }
    }

    async fn write_file_chunk(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), MoError> {
        let mut m = self.inner.lock().unwrap();
        let f = m.files.entry(path.to_path_buf()).or_default();
        // 对齐四个真实后端的约定：offset == 0 是 TRUNCATE（覆盖写清掉旧尾）。
        if offset == 0 {
            f.clear();
        }
        let start = offset as usize;
        if f.len() < start {
            f.resize(start, 0);
        }
        if f.len() < start + data.len() {
            f.resize(start + data.len(), 0);
        }
        f[start..start + data.len()].copy_from_slice(data);
        Ok(())
    }

    async fn is_dir(&self, path: &Path) -> bool {
        self.inner.lock().unwrap().dirs.contains(path)
    }
}

/// 标准冲突场景：源端 `/a.bin`（整份）与 `/b.txt`，目标端 `/` 下已有
/// **更长**的同名 a.bin（判成冲突而非续传候选）；`/b.txt` 无冲突。
fn scene() -> (AppState, RecFs, RecFs) {
    let app = common::isolated("conflict", AppState::new);
    let src = RecFs::default();
    src.put("/a.bin", PAYLOAD);
    src.put("/b.txt", b"hello");
    let dst = RecFs::default();
    dst.put("/a.bin", FOREIGN);
    (app, src, dst)
}

/// 探测一遍拿回冲突请求（各决策用例共用前置）。
async fn hold(app: &AppState, src: &RecFs, dst: &RecFs) -> PendingConflict {
    match app
        .transfer_between(
            vec![PathBuf::from("/a.bin"), PathBuf::from("/b.txt")],
            Endpoint::Remote(Arc::new(src.clone())),
            Path::new("/"),
            Endpoint::Remote(Arc::new(dst.clone())),
            false,
        )
        .await
    {
        TransferOutcome::NeedsConflictConfirmation(p) => *p,
        TransferOutcome::NeedsResumeConfirmation(_) => {
            panic!("更长的同名文件是冲突，不是部分完成")
        }
        TransferOutcome::Started(_) => panic!("目标有同名文件，应当等用户决策"),
    }
}

/// 探测：目标有完整同名文件时整批不提交，冲突清单里只有那一个。
#[tokio::test]
async fn conflicting_target_holds_the_whole_batch() {
    let (app, src, dst) = scene();
    let pending = hold(&app, &src, &dst).await;
    assert_eq!(pending.conflicts, vec![PathBuf::from("/a.bin")]);
    // 什么都没传：/b.txt 还没出现。
    assert!(dst.bytes("/b.txt").is_none());
}

/// 覆盖：冲突文件照原名重写，无冲突的照常。
#[tokio::test]
async fn overwrite_decision_replaces_the_conflicting_file() {
    let (app, src, dst) = scene();
    let pending = hold(&app, &src, &dst).await;
    let ids = app
        .resolve_conflict(pending, ConflictDecision::Overwrite)
        .await;
    assert_eq!(ids.len(), 2, "两个都提交（覆盖一个、正常传一个）");
    dst.wait_bytes("/a.bin", PAYLOAD).await;
    dst.wait_bytes("/b.txt", b"hello").await;
}

/// 改名：冲突文件按 `a 2.bin` 约定改名，两边都留（与本地 Rename 同约定）。
#[tokio::test]
async fn rename_decision_keeps_both() {
    let (app, src, dst) = scene();
    let pending = hold(&app, &src, &dst).await;
    let ids = app
        .resolve_conflict(pending, ConflictDecision::Rename)
        .await;
    assert_eq!(ids.len(), 2);
    dst.wait_bytes("/a 2.bin", PAYLOAD).await;
    assert_eq!(
        dst.bytes("/a.bin").as_deref(),
        Some(FOREIGN),
        "原有文件不被覆盖"
    );
}

/// 跳过：冲突文件不提交，其余照常。
#[tokio::test]
async fn skip_decision_leaves_conflicts_the_rest() {
    let (app, src, dst) = scene();
    let pending = hold(&app, &src, &dst).await;
    let ids = app.resolve_conflict(pending, ConflictDecision::Skip).await;
    assert_eq!(ids.len(), 1, "只有无冲突的 /b.txt 提交");
    dst.wait_bytes("/b.txt", b"hello").await;
    assert_eq!(
        dst.bytes("/a.bin").as_deref(),
        Some(FOREIGN),
        "冲突文件原样不动"
    );
}

// ------------------------------------------------------------ 本地对（两端都在本机）

/// 本地场景：`src/` 下 `a.txt`（整份）与 `b.txt`，`dst/` 下已有**更长**的同名
/// `a.txt`。真临时目录 + 真 `LocalFileSystem`——本地对走的是本机队列，用假后端
/// 测不到「覆盖」到底有没有落到 `CopyOperation` 上。
fn local_scene(tag: &str) -> (AppState, PathBuf, PathBuf) {
    let app = common::isolated(tag, AppState::new);
    let root = std::env::temp_dir().join(format!("mo-conflict-local-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("src");
    let dst = root.join("dst");
    std::fs::create_dir_all(&src).expect("建源目录");
    std::fs::create_dir_all(&dst).expect("建目标目录");
    std::fs::write(src.join("a.txt"), PAYLOAD).expect("写源 a.txt");
    std::fs::write(src.join("b.txt"), b"hello").expect("写源 b.txt");
    std::fs::write(dst.join("a.txt"), FOREIGN).expect("写目标同名 a.txt");
    (app, src, dst)
}

/// 等本地文件被后台操作写成 `want`（提交只入队，落盘在 blocking 池）。
async fn wait_local(path: &Path, want: &[u8]) {
    for _ in 0..250 {
        if std::fs::read(path).ok().as_deref() == Some(want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("等了 5s，{} 还没写成期望内容", path.display());
}

/// 探测一遍拿回本地冲突请求（各决策用例共用前置）。
async fn hold_local(app: &AppState, src: &Path, dst: &Path) -> PendingConflict {
    match app
        .transfer_between(
            vec![src.join("a.txt"), src.join("b.txt")],
            Endpoint::Local,
            dst,
            Endpoint::Local,
            false,
        )
        .await
    {
        TransferOutcome::NeedsConflictConfirmation(p) => *p,
        TransferOutcome::NeedsResumeConfirmation(_) => {
            panic!("本地没有「部分完成」这回事，不该弹续传卡")
        }
        TransferOutcome::Started(_) => panic!("目标有同名文件，本机也该等用户决策"),
    }
}

/// 本机复制撞同名：整批不提交、冲突清单里只有那一个（与远程同款反馈）。
#[tokio::test]
async fn local_conflicting_target_holds_the_whole_batch() {
    let (app, src, dst) = local_scene("local-hold");
    let pending = hold_local(&app, &src, &dst).await;
    assert_eq!(pending.conflicts, vec![dst.join("a.txt")]);
    assert!(
        !dst.join("b.txt").exists(),
        "什么都没传：无冲突的 b.txt 也还没落盘"
    );
    let _ = std::fs::remove_dir_all(dst.parent().expect("有父目录"));
}

/// 本机选「覆盖」：真把目标重写了，而不是多出一个 `a 2.txt`。
#[tokio::test]
async fn local_overwrite_decision_replaces_the_file() {
    let (app, src, dst) = local_scene("local-overwrite");
    let pending = hold_local(&app, &src, &dst).await;
    let ids = app
        .resolve_conflict(pending, ConflictDecision::Overwrite)
        .await;
    assert_eq!(ids.len(), 2, "两个都提交（覆盖一个、正常传一个）");
    wait_local(&dst.join("a.txt"), PAYLOAD).await;
    wait_local(&dst.join("b.txt"), b"hello").await;
    assert!(
        !dst.join("a 2.txt").exists(),
        "选了覆盖就不该再冒出改名的副本"
    );
    let _ = std::fs::remove_dir_all(dst.parent().expect("有父目录"));
}

/// 本机选「改名」：两边都留（`a 2.txt` 与原来的 `a.txt`）。
#[tokio::test]
async fn local_rename_decision_keeps_both() {
    let (app, src, dst) = local_scene("local-rename");
    let pending = hold_local(&app, &src, &dst).await;
    app.resolve_conflict(pending, ConflictDecision::Rename)
        .await;
    wait_local(&dst.join("a 2.txt"), PAYLOAD).await;
    assert_eq!(
        std::fs::read(dst.join("a.txt")).ok().as_deref(),
        Some(FOREIGN),
        "原有文件不被覆盖"
    );
    let _ = std::fs::remove_dir_all(dst.parent().expect("有父目录"));
}

/// 本机选「跳过」：冲突的那个不动，其余照常。
#[tokio::test]
async fn local_skip_decision_leaves_conflicts_the_rest() {
    let (app, src, dst) = local_scene("local-skip");
    let pending = hold_local(&app, &src, &dst).await;
    let ids = app.resolve_conflict(pending, ConflictDecision::Skip).await;
    assert_eq!(ids.len(), 1, "只有无冲突的 b.txt 提交");
    wait_local(&dst.join("b.txt"), b"hello").await;
    assert_eq!(
        std::fs::read(dst.join("a.txt")).ok().as_deref(),
        Some(FOREIGN),
        "冲突文件原样不动"
    );
    let _ = std::fs::remove_dir_all(dst.parent().expect("有父目录"));
}
