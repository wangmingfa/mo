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
use mo_operations::{Operation, OperationStatus, TransferOperation, TransferOpts};

/// 内存文件系统：把目录与文件都放在一张表里，行为对齐 `LocalFileSystem`
/// （`write_file` 拒绝覆盖、`metadata` 对不存在的路径报错）。
#[derive(Clone)]
struct MemFs {
    inner: Arc<Mutex<Mem>>,
    /// 记录分块传输里「单次读取请求的最大长度」，用来断言传输真的走了分块路径
    ///（而不是一次整份读）。
    max_chunk: Arc<Mutex<u64>>,
    /// 记录 `finalize_file_chunk` 被调用次数，用来断言传输循环确实在每块写完后收尾
    ///（WebDAV 的依赖：不收尾就永远不 PUT）。
    finalize_calls: Arc<Mutex<u64>>,
    /// 记录「源端被读的最小 offset」，用来断言续传真的从断点起读、没重读前面的部分。
    /// 初值 `u64::MAX`：没被读过时断言 `== half` 不会误中 0。
    min_read_offset: Arc<Mutex<u64>>,
}

impl Default for MemFs {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Mem::default())),
            max_chunk: Arc::new(Mutex::new(0)),
            finalize_calls: Arc::new(Mutex::new(0)),
            min_read_offset: Arc::new(Mutex::new(u64::MAX)),
        }
    }
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

    /// 测试桩访问器：返回分块传输里「单次读取请求的最大长度」。
    fn max_chunk(&self) -> u64 {
        *self.max_chunk.lock().unwrap()
    }

    /// 测试桩访问器：返回 `finalize_file_chunk` 被调用的次数。
    fn finalize_calls(&self) -> u64 {
        *self.finalize_calls.lock().unwrap()
    }

    /// 测试桩访问器：返回源端被读的最小 offset（u64::MAX 表示没被读过）。
    fn min_read_offset(&self) -> u64 {
        *self.min_read_offset.lock().unwrap()
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

    async fn read_file_chunk(
        &self,
        path: &Path,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, MoError> {
        {
            let mut m = self.max_chunk.lock().unwrap();
            if len > *m {
                *m = len;
            }
            let mut o = self.min_read_offset.lock().unwrap();
            if offset < *o {
                *o = offset;
            }
        }
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
        // 区间替换（覆盖写语义，与传输块写一致）；MemFs 本来就是整份在内存，
        // 这里不省内存但行为正确，且让分块路径有可断言的落点。
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

    async fn finalize_file_chunk(&self, _path: &Path) -> Result<(), MoError> {
        // MemFs 在 write_file_chunk 里就落好了，这里只是记一次调用——断言传输循环真的收尾。
        *self.finalize_calls.lock().unwrap() += 1;
        Ok(())
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

/// 大文件（超过两个分块）走分块读写：内容字节完全一致，且源端单次读取不超过
/// `CHUNK_SIZE`（证明没整份读进内存）。`CHUNK_SIZE` 是 `mo_operations::transfer` 的
/// 私有常量（4 MiB），这里用同一字面量断言。
#[tokio::test]
async fn large_file_transfer_is_chunked_and_byte_exact() {
    let payload = vec![0xABu8; 10 * 1024 * 1024];
    let mem = MemFs::default();
    // 源端用内存桩：便于在 src 上记录「单次读取请求的最大长度」，断言分块路径真被走。
    mem.put("/big.bin", &payload);

    let root = local_tree("chunked-dst");
    let dst = root.join("big.bin");

    let op = TransferOperation::new(
        7,
        Arc::new(mem.clone()),
        Arc::new(LocalFileSystem),
        PathBuf::from("/big.bin"),
        dst.clone(),
        false,
        "下载",
    );
    run_in_blocking(op.clone()).await.expect("下载应成功");

    assert_eq!(op.status(), OperationStatus::Completed);
    assert_eq!(
        op.progress(),
        (payload.len() as u64, payload.len() as u64),
        "10 MiB 应全部计入进度"
    );
    assert_eq!(std::fs::read(&dst).unwrap(), payload);
    // 分块路径真的被走：源端没一次整份读，单块上限 = CHUNK_SIZE（4 MiB，10 MiB 触发 4+4+2 三片）。
    let max_chunk = mem.max_chunk();
    assert!(max_chunk > 0, "传输应当走分块读");
    assert!(
        max_chunk <= 4 * 1024 * 1024,
        "单块读取不应超过 CHUNK_SIZE，实测 {max_chunk}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 传输循环对每个目标文件调一次 `finalize_file_chunk`：WebDAV 靠它在收尾时整份 PUT，
/// 不调就永远不落盘。这里用内存目标端记下调用次数断言（单文件 → 恰好 1 次）。
#[tokio::test]
async fn transfer_calls_finalize_once_per_destination_file() {
    let root = local_tree("finalize");
    let mem = MemFs::default();
    let op = TransferOperation::new(
        9,
        Arc::new(LocalFileSystem),
        Arc::new(mem.clone()),
        root.join("a.txt"),
        PathBuf::from("/dst/a.txt"),
        false,
        "上传",
    );
    run_in_blocking(op).await.expect("上传应成功");
    assert_eq!(
        mem.finalize_calls(),
        1,
        "单文件目标应恰好收尾一次：{:?}",
        mem.keys()
    );
    assert_eq!(mem.bytes("/dst/a.txt").as_deref(), Some(&b"abc"[..]));
    let _ = std::fs::remove_dir_all(&root);
}

/// 断点续传：目标已是「部分完成」时，从断点接着写，不重传已传部分，最终字节完全一致。
#[tokio::test]
async fn resume_continues_a_partial_destination() {
    let payload = vec![0xABu8; 10 * 1024 * 1024];
    let src = MemFs::default();
    src.put("/big.bin", &payload);

    // 目标端预置「半份」内容，模拟上次被取消留下来的部分文件。
    let half = payload.len() / 2;
    let dst = MemFs::default();
    dst.put("/big.bin", &payload[..half]);

    let op = TransferOperation::with_resume(
        10,
        Arc::new(src.clone()),
        Arc::new(dst.clone()),
        PathBuf::from("/big.bin"),
        PathBuf::from("/big.bin"),
        TransferOpts {
            remove_source: false,
            label: "下载",
            resume: true,
            overwrite: false,
        },
    );
    run_in_blocking(op.clone()).await.expect("续传应成功");

    assert_eq!(op.status(), OperationStatus::Completed);
    assert_eq!(
        op.progress(),
        (payload.len() as u64, payload.len() as u64),
        "进度应含已传部分（开局即半满）"
    );
    // 最终内容必须整份一致。
    assert_eq!(dst.bytes("/big.bin").unwrap(), payload);
    // 关键：源端从断点（half）起读，没重读前半——min_read_offset 应等于 half。
    assert_eq!(
        src.min_read_offset(),
        half as u64,
        "续传不应重读已传的前半部分"
    );
    // 源端单次读取仍 ≤ CHUNK_SIZE（分块路径没被破坏）。
    assert!(
        src.max_chunk() <= 4 * 1024 * 1024,
        "单块读取不应超过 CHUNK_SIZE，实测 {}",
        src.max_chunk()
    );
    // 收尾仍发生一次。
    assert_eq!(dst.finalize_calls(), 1);
}

/// 对照（反向）：不续传时，目标已存在会被 `free_path` 改名，源从 0 重读，
/// 部分文件原样留着——证明 `resume` 这一项确实改变了行为。
#[tokio::test]
async fn non_resume_renames_and_rereads_from_zero() {
    let payload = vec![0xCDu8; 10 * 1024 * 1024];
    let src = MemFs::default();
    src.put("/big.bin", &payload);

    let half = payload.len() / 2;
    let dst = MemFs::default();
    dst.put("/big.bin", &payload[..half]); // 部分文件

    // 用 `new`（resume=false）——与续传路径的区别就在这一项。
    let op = TransferOperation::new(
        11,
        Arc::new(src.clone()),
        Arc::new(dst.clone()),
        PathBuf::from("/big.bin"),
        PathBuf::from("/big.bin"),
        false,
        "下载",
    );
    run_in_blocking(op.clone()).await.expect("常规传输应成功");

    assert_eq!(op.status(), OperationStatus::Completed);
    // 部分文件原样留着（没被续写、也没被覆盖）。
    assert_eq!(dst.bytes("/big.bin").unwrap(), &payload[..half]);
    // 新文件是完整内容，按命名约定改名（`free_path` 用 `format!("{stem} {i}{ext})` → `/big 2.bin`）。
    assert_eq!(dst.bytes("/big 2.bin").unwrap(), payload);
    // 源从 0 重读（没走续传）。
    assert_eq!(src.min_read_offset(), 0, "不续传应从头重读");
}
