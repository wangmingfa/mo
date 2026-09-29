//! 断点续传的**探测与决策**（`transfer_between` → `PendingResume` →
//! `resolve_resume`）。
//!
//! 续传的引擎侧（`can_resume` 判据、断点续写、进度预置）由 mo-operations 的
//! `tests/transfer.rs` 钉住；这里钉的是「应用层怎么把它递到用户面前」：
//!
//! * 一批里只要有「部分完成」的目标，**整批不提交**，交回确认卡；
//! * 继续 = 部分文件从断点续写，其余照常；
//! * 重传 = 全部照常（目标已存在会被改名）；
//! * 跳过 = 部分文件不提交，其余照常。
//!
//! 端点直接传 `Endpoint::Remote(假后端)`——`transfer_between` 只认端点不认
//! 会话，这里不需要 SessionRegistry 那套基建。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mo_app::{AppState, Endpoint, PendingResume, ResumeDecision, TransferOutcome};
use mo_core::{FileId, FileMetadata, MoError};
use mo_fs::{FileSystem, ReadDirEntry};

mod common;

/// 测试载荷：远小于 CHUNK_SIZE（4MiB），整份一块传完。
const PAYLOAD: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
/// 「上次被取消留下的一半」。
const HALF: usize = PAYLOAD.len() / 2;

#[derive(Default)]
struct Mem {
    dirs: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

/// 内存假后端：行为对齐 mo-operations 测试里的 MemFs（区间替换写、按块读）。
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

    /// 列目录的实现体（异步 / 阻塞两个入口共用，里面没有真 IO）。
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
        panic!(
            "等了 5s，{path} 还没写成期望内容（实际 {:?}）",
            self.bytes(path).map(|b| b.len())
        );
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

/// 一套标准的「重提」场景：源端 `/a.bin`（整份）与 `/b.txt`，
/// 目标端 `/` 下有 a.bin 的半份（上次被取消留下的）。
fn scene() -> (AppState, RecFs, RecFs) {
    let app = common::isolated("resume", AppState::new);
    let src = RecFs::default();
    src.put("/a.bin", PAYLOAD);
    src.put("/b.txt", b"hello");
    let dst = RecFs::default();
    dst.put("/a.bin", &PAYLOAD[..HALF]);
    (app, src, dst)
}

/// 探测：有部分完成的目标时整批不提交，交回带部分清单的请求。
#[tokio::test]
async fn partial_target_holds_the_whole_batch() {
    let (app, src, dst) = scene();
    let outcome = app
        .transfer_between(
            vec![PathBuf::from("/a.bin"), PathBuf::from("/b.txt")],
            Endpoint::Remote(Arc::new(src.clone())),
            Path::new("/"),
            Endpoint::Remote(Arc::new(dst.clone())),
            false,
        )
        .await;
    let TransferOutcome::NeedsResumeConfirmation(pending) = outcome else {
        panic!("目标有半份文件，应当交回续传确认而不是直接提交");
    };
    assert_eq!(pending.partial, vec![PathBuf::from("/a.bin")]);
    assert_eq!(pending.paths.len(), 2, "请求要带回整批路径");
    // 什么都没提交：b.txt（无冲突的那个）也没被传。
    assert!(
        dst.bytes("/b.txt").is_none(),
        "探测阶段不该提交任何传输（b.txt 却已经出现在目标端）"
    );
    assert_eq!(
        dst.bytes("/a.bin").as_deref(),
        Some(&PAYLOAD[..HALF]),
        "部分文件原样留着"
    );
    let _ = src;
}

/// 继续：部分文件从断点续写成整份，其余照常。
#[tokio::test]
async fn resume_decision_completes_the_partial_file() {
    let (app, src, dst) = scene();
    let pending = hold(&app, &src, &dst).await;
    let ids = app.resolve_resume(pending, ResumeDecision::Resume).await;
    assert_eq!(ids.len(), 2, "两个都提交（a.bin 续传、b.txt 常规）");
    dst.wait_bytes("/a.bin", PAYLOAD).await;
    assert_eq!(dst.bytes("/b.txt").as_deref(), Some(&b"hello"[..]));
}

/// 跳过：部分文件不动，其余照常。
#[tokio::test]
async fn skip_decision_leaves_partial_transfers_the_rest() {
    let (app, src, dst) = scene();
    let pending = hold(&app, &src, &dst).await;
    let ids = app.resolve_resume(pending, ResumeDecision::Skip).await;
    assert_eq!(ids.len(), 1, "只有 b.txt 提交");
    dst.wait_bytes("/b.txt", b"hello").await;
    assert_eq!(
        dst.bytes("/a.bin").as_deref(),
        Some(&PAYLOAD[..HALF]),
        "跳过的那个文件原样留着"
    );
}

/// 重传：全部照常——部分文件留着，完整内容落到改名的新文件（永不静默覆盖）。
#[tokio::test]
async fn rename_decision_rereads_to_a_new_name() {
    let (app, src, dst) = scene();
    let pending = hold(&app, &src, &dst).await;
    let ids = app.resolve_resume(pending, ResumeDecision::Rename).await;
    assert_eq!(ids.len(), 2);
    dst.wait_bytes("/a 2.bin", PAYLOAD).await;
    assert_eq!(
        dst.bytes("/a.bin").as_deref(),
        Some(&PAYLOAD[..HALF]),
        "原部分文件不被覆盖"
    );
}

/// 探测一遍拿回请求（各决策用例共用前置）。
async fn hold(app: &AppState, src: &RecFs, dst: &RecFs) -> PendingResume {
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
        TransferOutcome::NeedsResumeConfirmation(p) => *p,
        TransferOutcome::Started(_) => panic!("半份目标在，应当等用户决策"),
    }
}
