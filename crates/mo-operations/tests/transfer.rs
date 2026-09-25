//! 跨文件系统传输的端到端契约：**上传用真本地 → 内存目标**跑一遍完整链路
//! （走 `Operation::run`，即生产路径上 `spawn_blocking` 里那一段）。
//!
//! 为什么目标用内存实现而不是再开一个临时目录：这里要守的是「两个端点各是一份
//! `FileSystem`」这件事——真本地 → 真本地会掩盖端点选择、`is_dir` 判据、去重探测
//! 里任何一处「其实偷偷用了 `std::fs`」的错误。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mo_core::{EntryKind, FileId, FileMetadata, MoError, Permissions};
use mo_fs::{FileSystem, LocalFileSystem, ReadDirEntry};
use mo_operations::{Operation, OperationStatus, TransferOperation};

/// 内存文件系统：把目录与文件都放在一张表里，行为对齐 `LocalFileSystem`
/// （`write_file` 拒绝覆盖、`metadata` 对不存在的路径报错）。
#[derive(Clone, Default)]
struct MemFs {
    inner: Arc<Mutex<Mem>>,
}

#[derive(Default)]
struct Mem {
    dirs: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

impl MemFs {
    fn put(&self, path: &str, bytes: &[u8]) {
        let mut m = self.inner.lock().unwrap();
        for dir in parents(path) {
            m.dirs.insert(dir);
        }
        m.files.insert(PathBuf::from(path), bytes.to_vec());
    }

    fn keys(&self) -> Vec<String> {
        let m = self.inner.lock().unwrap();
        let mut out: Vec<String> = m
            .files
            .keys()
            .chain(m.dirs.iter())
            .map(|p| p.display().to_string())
            .collect();
        out.sort();
        out
    }

    fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.inner
            .lock()
            .unwrap()
            .files
            .get(&PathBuf::from(path))
            .cloned()
    }

    fn dirs(&self) -> Vec<String> {
        let m = self.inner.lock().unwrap();
        // 远程后端拿路径会把 `\` 规范化成 `/`（见 sftp.rs 的 `remote()`），fake 对齐这个行为。
        m.dirs
            .iter()
            .map(|p| p.display().to_string().replace('\\', "/"))
            .collect()
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
impl FileSystem for MemFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        let m = self.inner.lock().unwrap();
        if !m.dirs.contains(path) {
            return Err(MoError::Other(format!("不是目录：{}", path.display())));
        }
        let mut out = Vec::new();
        for p in m.files.keys().chain(m.dirs.iter()) {
            if p.parent() == Some(path) && p != path {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let kind = if m.dirs.contains(p) {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                };
                out.push(ReadDirEntry::new(
                    FileId::synthetic(p),
                    name,
                    kind,
                    p.clone(),
                ));
            }
        }
        Ok(out)
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        let m = self.inner.lock().unwrap();
        let mut out = Vec::new();
        for p in m.files.keys().chain(m.dirs.iter()) {
            if p.parent() == Some(path) && p != path {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let kind = if m.dirs.contains(p) {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                };
                out.push(ReadDirEntry::new(
                    FileId::synthetic(p),
                    name,
                    kind,
                    p.clone(),
                ));
            }
        }
        Ok(out)
    }

    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError> {
        let m = self.inner.lock().unwrap();
        if let Some(bytes) = m.files.get(path) {
            return Ok(FileMetadata {
                size: bytes.len() as u64,
                modified: None,
                created: None,
                permissions: Permissions::default(),
            });
        }
        if m.dirs.contains(path) {
            return Ok(FileMetadata {
                size: 0,
                modified: None,
                created: None,
                permissions: Permissions::default(),
            });
        }
        Err(MoError::Other(format!("不存在：{}", path.display())))
    }

    async fn create_dir(&self, path: &Path) -> Result<(), MoError> {
        let mut m = self.inner.lock().unwrap();
        m.dirs.insert(path.to_path_buf());
        Ok(())
    }

    async fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), MoError> {
        let mut m = self.inner.lock().unwrap();
        // 与 LocalFileSystem 的 `create_new` 同语义：已存在必须失败，绝不静默覆盖。
        if m.files.contains_key(path) {
            return Err(MoError::Other(format!("已存在：{}", path.display())));
        }
        m.files.insert(path.to_path_buf(), contents.to_vec());
        Ok(())
    }

    async fn remove_file(&self, path: &Path) -> Result<(), MoError> {
        self.inner.lock().unwrap().files.remove(path);
        Ok(())
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), MoError> {
        let mut m = self.inner.lock().unwrap();
        m.dirs.retain(|d| !d.starts_with(path));
        m.files.retain(|f, _| !f.starts_with(path));
        Ok(())
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), MoError> {
        let mut m = self.inner.lock().unwrap();
        if let Some(v) = m.files.remove(from) {
            m.files.insert(to.to_path_buf(), v);
        }
        Ok(())
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, MoError> {
        self.inner
            .lock()
            .unwrap()
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| MoError::Other(format!("不是文件：{}", path.display())))
    }

    async fn is_dir(&self, path: &Path) -> bool {
        self.inner.lock().unwrap().dirs.contains(path)
    }
}

/// 造一个真本地目录：`a.txt`(3B) + `sub/b.txt`(5B)，返回根路径。
fn local_tree(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("mo-transfer-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.txt"), b"abc").unwrap();
    std::fs::write(root.join("sub/b.txt"), b"12345").unwrap();
    root
}

/// 生产路径上 `run()` 是在 blocking 池线程里跑的（`AppState::submit_operation`），
/// 这里照抄那个上下文——`run()` 内部的 `Handle::current().block_on` 在 async
/// 上下文里会 panic，测试直接 await 就跑偏了。
async fn run_in_blocking(op: Arc<dyn Operation>) -> Result<(), MoError> {
    tokio::task::spawn_blocking(move || op.run())
        .await
        .expect("blocking 任务不应 panic")
}

/// 上传：目录递归、字节进度、父目录自动补齐。
#[tokio::test]
async fn upload_walks_the_tree_and_reports_bytes() {
    let root = local_tree("upload");
    let mem = MemFs::default();
    // 目标端先放一份无关文件，顺带把 `/dst` 建出来（上传脚本要能在已有目录里落地）。
    mem.put("/dst/keep.txt", b"old");

    let op = TransferOperation::new(
        1,
        Arc::new(LocalFileSystem),
        Arc::new(mem.clone()),
        root.clone(),
        PathBuf::from("/dst/sub-dir"),
        false,
        "上传",
    );
    run_in_blocking(op.clone()).await.expect("上传应成功");

    assert_eq!(op.status(), OperationStatus::Completed);
    assert_eq!(op.progress(), (8, 8), "3 + 5 字节应全部计入进度");
    assert_eq!(
        mem.bytes("/dst/sub-dir/a.txt").as_deref(),
        Some(&b"abc"[..])
    );
    assert_eq!(
        mem.bytes("/dst/sub-dir/sub/b.txt").as_deref(),
        Some(&b"12345"[..])
    );
    assert!(
        mem.dirs().contains(&"/dst/sub-dir/sub".to_string()),
        "子目录应在目标端建出来：{:?}",
        mem.dirs()
    );
    assert_eq!(
        mem.bytes("/dst/keep.txt").as_deref(),
        Some(&b"old"[..]),
        "目标端原有文件不该被动"
    );
    // 源还在（复制语义）。
    assert!(root.join("a.txt").exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// 目标目录本身已存在时按约定改名（`/dst` → `/dst 2`），**不合并覆盖**——
/// 与本地复制的 `ConflictPolicy::Rename` 同一行为（Finder 复制同名文件夹也是这样）。
#[tokio::test]
async fn existing_target_dir_is_renamed_not_merged() {
    let root = local_tree("dir-rename");
    let mem = MemFs::default();
    mem.put("/dst/inside.txt", b"KEEP");

    let op = TransferOperation::new(
        6,
        Arc::new(LocalFileSystem),
        Arc::new(mem.clone()),
        root.clone(),
        PathBuf::from("/dst"),
        false,
        "上传",
    );
    run_in_blocking(op).await.expect("上传应成功");

    assert_eq!(
        mem.bytes("/dst/inside.txt").as_deref(),
        Some(&b"KEEP"[..]),
        "原有目录内容不该被动"
    );
    assert_eq!(mem.bytes("/dst 2/a.txt").as_deref(), Some(&b"abc"[..]));
    let _ = std::fs::remove_dir_all(&root);
}

/// 目标已有同名文件时改名，**绝不覆盖**（与本地复制的 ConflictPolicy::Rename 同约定）。
#[tokio::test]
async fn existing_target_is_renamed_not_overwritten() {
    let root = local_tree("rename");
    let mem = MemFs::default();
    // 目标端已有一份同名文件（内容不同）。
    mem.put("/dst/a.txt", b"KEEP");

    let op = TransferOperation::new(
        2,
        Arc::new(LocalFileSystem),
        Arc::new(mem.clone()),
        root.join("a.txt"),
        PathBuf::from("/dst/a.txt"),
        false,
        "上传",
    );
    run_in_blocking(op).await.expect("上传应成功");

    assert_eq!(
        mem.bytes("/dst/a.txt").as_deref(),
        Some(&b"KEEP"[..]),
        "原有文件不能被覆盖"
    );
    assert_eq!(
        mem.bytes("/dst/a 2.txt").as_deref(),
        Some(&b"abc"[..]),
        "新内容应落到改名后的路径：{:?}",
        mem.keys()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 移动语义：传完删源（含整棵目录）。
#[tokio::test]
async fn remove_source_deletes_the_tree_after_transfer() {
    let root = local_tree("move");
    let mem = MemFs::default();

    let op = TransferOperation::new(
        3,
        Arc::new(LocalFileSystem),
        Arc::new(mem.clone()),
        root.clone(),
        PathBuf::from("/dst"),
        true,
        "移动",
    );
    run_in_blocking(op).await.expect("移动应成功");

    assert!(!root.exists(), "移动后源目录应被删掉");
    assert_eq!(mem.bytes("/dst/sub/b.txt").as_deref(), Some(&b"12345"[..]));
}

/// 取消：一字节都不该落地，状态是 Cancelled（不是 Failed）。
#[tokio::test]
async fn cancelled_before_start_writes_nothing() {
    let root = local_tree("cancel");
    let mem = MemFs::default();

    let op = TransferOperation::new(
        4,
        Arc::new(LocalFileSystem),
        Arc::new(mem.clone()),
        root.clone(),
        PathBuf::from("/dst"),
        false,
        "上传",
    );
    op.cancel();
    run_in_blocking(op.clone()).await.expect("取消不算错误");

    assert_eq!(op.status(), OperationStatus::Cancelled);
    assert!(
        mem.keys().is_empty(),
        "取消后不该写出任何条目：{:?}",
        mem.keys()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 下载方向同样走这条路：内存源 → 真本地目标。
#[tokio::test]
async fn download_writes_into_local_tree() {
    let mem = MemFs::default();
    mem.put("/remote/hello.txt", b"hello");
    let root = std::env::temp_dir().join(format!("mo-transfer-dl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let op = TransferOperation::new(
        5,
        Arc::new(mem.clone()),
        Arc::new(LocalFileSystem),
        PathBuf::from("/remote/hello.txt"),
        root.join("hello.txt"),
        false,
        "下载",
    );
    run_in_blocking(op).await.expect("下载应成功");

    assert_eq!(std::fs::read(root.join("hello.txt")).unwrap(), b"hello");
    let _ = std::fs::remove_dir_all(&root);
}
