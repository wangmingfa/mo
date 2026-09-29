//! 远程撤销模型（`Reversible::RemoteCopy` / `RemoteMove` / `RemoteRename`）。
//!
//! 撤销记录在操作发生时捕获两端的后端，⌘Z / ⌘⇧Z 按捕获的后端执行，不再按
//! 「当前这一页在不在远程」猜。这里用内存假后端钉三条主干：
//!
//! * **复制可逆**：撤销 = 删掉目标侧副本（源不动）；重做 = 原样再传。
//! * **移动可逆**：撤销 = 反向传输，源位置回来、目标侧副本消失。
//! * **改名落点可逆**：冲突卡选「改名」时，撤销删的是**真实落点**
//!   （`a 2.bin`），不是被顶掉的原名——这是提交方预去重（`unique_remote_path`）
//!   存在的理由。
//!
//! 端点直接传 `Endpoint::Remote(假后端)`，不需要 SessionRegistry 基建。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mo_app::{AppState, ConflictDecision, Endpoint, TransferOutcome};
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

/// 内存假后端：与 conflict.rs 的 RecFs 同款，但 `remove_dir` 是**真递归**
/// （撤销复制要删掉一整棵目标子树，桩不递归的话断言不出删干净）。
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

    fn exists(&self, path: &str) -> bool {
        let m = self.inner.lock().unwrap();
        m.files.contains_key(&PathBuf::from(path)) || m.dirs.contains(&PathBuf::from(path))
    }

    /// 轮询等 `f()` 成立（最多约 5s）——撤销 / 重做走后台任务，提交即返回。
    async fn wait_for<F: Fn() -> bool>(&self, f: F, what: &str) {
        for _ in 0..250 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("等了 5s：{what}");
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
        self.read_dir_blocking(path)
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
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

    /// 递归删：自身与所有以它为前缀的子项一并清掉（trait 约定的语义）。
    async fn remove_dir(&self, path: &Path) -> Result<(), MoError> {
        let mut m = self.inner.lock().unwrap();
        if !m.dirs.remove(path) {
            return Err(MoError::Other(format!("不是目录：{}", path.display())));
        }
        let prefix = format!("{}/", path.to_string_lossy());
        m.files.retain(|p, _| !p.starts_with(&prefix));
        m.dirs.retain(|p| !p.starts_with(&prefix));
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

/// 同一条远程会话内复制：源 `/a.bin`，落点目录 `/dst`。
fn copy_scene() -> (AppState, RecFs) {
    let app = common::isolated("undo-remote", AppState::new);
    let fs = RecFs::default();
    fs.put("/a.bin", PAYLOAD);
    (app, fs)
}

/// 复制 → 等落点出现。
async fn copy_to_dst(app: &AppState, fs: &RecFs) {
    let outcome = app
        .transfer_between(
            vec![PathBuf::from("/a.bin")],
            Endpoint::Remote(Arc::new(fs.clone())),
            Path::new("/dst"),
            Endpoint::Remote(Arc::new(fs.clone())),
            false,
        )
        .await;
    assert!(
        matches!(outcome, TransferOutcome::Started(_)),
        "无冲突应当直接提交"
    );
    fs.wait_for(
        || fs.bytes("/dst/a.bin").as_deref() == Some(PAYLOAD),
        "复制落点",
    )
    .await;
}

/// 撤销复制：目标侧副本消失，源原样不动。
#[tokio::test]
async fn undo_deletes_the_remote_copy() {
    let (app, fs) = copy_scene();
    copy_to_dst(&app, &fs).await;
    assert!(app.can_undo(), "远程复制后应可撤销");

    app.undo();
    fs.wait_for(|| !fs.exists("/dst/a.bin"), "撤销应删掉目标侧副本")
        .await;
    assert_eq!(
        fs.bytes("/a.bin").as_deref(),
        Some(PAYLOAD),
        "源文件原样不动"
    );
}

/// 撤销后再重做：副本原样回来。
#[tokio::test]
async fn redo_recreates_the_remote_copy() {
    let (app, fs) = copy_scene();
    copy_to_dst(&app, &fs).await;

    app.undo();
    fs.wait_for(|| !fs.exists("/dst/a.bin"), "撤销删副本").await;

    assert!(app.can_redo(), "撤销后应可重做");
    app.redo();
    fs.wait_for(
        || fs.bytes("/dst/a.bin").as_deref() == Some(PAYLOAD),
        "重做应把副本传回来",
    )
    .await;
}

/// 撤销移动：反向传输，源位置回来、目标侧副本消失。
#[tokio::test]
async fn undo_moves_the_remote_file_back() {
    let (app, fs) = copy_scene();
    let outcome = app
        .transfer_between(
            vec![PathBuf::from("/a.bin")],
            Endpoint::Remote(Arc::new(fs.clone())),
            Path::new("/dst"),
            Endpoint::Remote(Arc::new(fs.clone())),
            true,
        )
        .await;
    assert!(matches!(outcome, TransferOutcome::Started(_)));
    fs.wait_for(
        || fs.bytes("/dst/a.bin").as_deref() == Some(PAYLOAD) && !fs.exists("/a.bin"),
        "移动完成：目标有、源没有",
    )
    .await;

    app.undo();
    fs.wait_for(
        || fs.bytes("/a.bin").as_deref() == Some(PAYLOAD) && !fs.exists("/dst/a.bin"),
        "撤销移动：源回来、目标副本消失",
    )
    .await;
}

/// 冲突卡选「改名」后撤销：删的是**真实落点** `a 2.bin`，被顶掉的原名不动。
///
/// 撤销记录若写提交时的请求名（`/a.bin`），⌘Z 会把目标端**原有的**那份删掉——
/// 这是预去重（`unique_remote_path`）钉住的边界。
#[tokio::test]
async fn undo_after_conflict_rename_deletes_the_actual_landing_spot() {
    let app = common::isolated("undo-remote-rename", AppState::new);
    let src = RecFs::default();
    src.put("/a.bin", PAYLOAD);
    let dst = RecFs::default();
    dst.put("/a.bin", FOREIGN);

    let pending = match app
        .transfer_between(
            vec![PathBuf::from("/a.bin")],
            Endpoint::Remote(Arc::new(src.clone())),
            Path::new("/"),
            Endpoint::Remote(Arc::new(dst.clone())),
            false,
        )
        .await
    {
        TransferOutcome::NeedsConflictConfirmation(p) => *p,
        _ => panic!("目标有更长的同名文件，应当弹冲突卡"),
    };
    let ids = app
        .resolve_conflict(pending, ConflictDecision::Rename)
        .await;
    assert_eq!(ids.len(), 1);
    dst.wait_for(
        || dst.bytes("/a 2.bin").as_deref() == Some(PAYLOAD),
        "改名落点出现",
    )
    .await;

    app.undo();
    dst.wait_for(|| !dst.exists("/a 2.bin"), "撤销应删掉改名的副本")
        .await;
    assert_eq!(
        dst.bytes("/a.bin").as_deref(),
        Some(FOREIGN),
        "被顶掉的原名是别人的文件，撤销不得动它"
    );
}

/// 撤销一次「上传」：本地 → 远程的复制，撤销删掉远端那份，本地源不动。
///
/// 这条顺带钉住「本地一侧的后端也被捕获」——撤销拿的是记录里的
/// `LocalFileSystem` / 会话后端，两端都不能靠当时视图猜。
#[tokio::test]
async fn undo_deletes_an_uploaded_copy() {
    let base = std::env::temp_dir().join(format!("mo-undo-upload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let local = base.join("up.bin");
    std::fs::write(&local, PAYLOAD).unwrap();

    let app = common::isolated("undo-upload", AppState::new);
    let fs = RecFs::default();
    // dest 是**目标目录**：源按原名（up.bin）落到 `/` 下。
    let outcome = app
        .transfer_between(
            vec![local.clone()],
            Endpoint::Local,
            Path::new("/"),
            Endpoint::Remote(Arc::new(fs.clone())),
            false,
        )
        .await;
    assert!(matches!(outcome, TransferOutcome::Started(_)));
    fs.wait_for(
        || fs.bytes("/up.bin").as_deref() == Some(PAYLOAD),
        "上传落点",
    )
    .await;

    app.undo();
    fs.wait_for(|| !fs.exists("/up.bin"), "撤销应删掉远端副本")
        .await;
    assert_eq!(
        std::fs::read(&local).unwrap().as_slice(),
        PAYLOAD,
        "本地源文件不动"
    );

    let _ = std::fs::remove_dir_all(&base);
}
