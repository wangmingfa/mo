//! 跨文件系统的传输：上传（本地 → 远程）、下载（远程 → 本地）、远程之间复制，
//! 以及这些方向上的移动。
//!
//! 为什么不复用 [`crate::CopyOperation`]：它内部的 `fs_util::copy_tree` 走 `std::fs`，
//! 而远程路径**在本机上根本不存在**（`Path::exists()` 恒 false，`std::fs::read` 必失败）。
//! 这里的两端各自是一份 [`FileSystem`] 实现，字节按「源读整份 → 目标写整份」搬，
//! 进度 / 取消 / 暂停复用同一套 `OpInner` 约定——所以进度面板、估速、暂停按钮
//! 对传输操作**不用任何特判**就能用。
//!
//! ⚠️ 远程后端各自带一份 1-worker runtime（见 `mo-remote` 的模块注释），它们的方法
//! 只经 `Handle::spawn` 派回自己的 runtime 驱动。本操作的 `run()` 跑在 blocking
//! 池线程里（`AppState::submit_operation`），在那里 `block_on` 驱动这串 future 是
//! 安全的（blocking 线程不在 async 上下文）；换成 async 任务里 block_on 会 panic。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use mo_core::MoError;
use mo_fs::FileSystem;
use parking_lot::Mutex;

use crate::fs_util::wait_if_paused;
use crate::{OpInner, Operation, OperationStatus};

/// 装箱的递归 future（`async fn` 递归自身编译不过，只能手写 `Box::pin`）。
type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// 传输时的分块大小（4 MiB）。整份读写会大文件整份进内存，分块后内存峰值降到这一档；
/// 进度也按块累加，估算速度仍然平滑。
const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

/// 跨文件系统的传输操作（上传 / 下载 / 远程间复制）。
pub struct TransferOperation {
    id: u64,
    src_fs: Arc<dyn FileSystem>,
    dst_fs: Arc<dyn FileSystem>,
    src: PathBuf,
    dst: PathBuf,
    /// 递归搬运的三个开关（删源 / 续传 / 覆盖）。
    flags: TreeFlags,
    /// 描述里用的动作词（上传 / 下载 / 复制 / 移动），由调用方按方向给出。
    label: &'static str,
    state: Arc<Mutex<OpInner>>,
}

/// 构造 `TransferOperation` 时一次性给出的选项，用来把构造函数压在 clippy 的
/// 7 参数上限以内（`remove_source` / `label` / `resume` / `overwrite` 一组）。
pub struct TransferOpts {
    /// 传完删源（「移动」语义）。
    pub remove_source: bool,
    /// 描述里用的动作词（上传 / 下载 / 复制 / 移动），由调用方按方向给出。
    pub label: &'static str,
    /// 断点续传：true 时 `transfer_tree` 跳过 `free_path`、按目标已传字节续写。
    pub resume: bool,
    /// 冲突卡上用户选了「覆盖」：目标存在也照原名写（`write_file_chunk` 首块
    /// TRUNCATE，不 `free_path` 改名）。目录则是合并语义（往同名目录里传）。
    pub overwrite: bool,
}

/// 递归搬运用的三个开关，收成一个 Copy 结构体把 `transfer_tree` 压在
/// clippy 的 7 参数上限以内。
#[derive(Clone, Copy)]
struct TreeFlags {
    remove_source: bool,
    resume: bool,
    overwrite: bool,
}

impl TransferOperation {
    /// 新建一次跨文件系统传输（不续传）。
    ///
    /// 两个端点相同时（远程 → 远程）也走这条路：读整份再写整份，不去赌协议自带的
    /// COPY 指令（FTP 没有，WebDAV 的实现各家不一致）。
    pub fn new(
        id: u64,
        src_fs: Arc<dyn FileSystem>,
        dst_fs: Arc<dyn FileSystem>,
        src: PathBuf,
        dst: PathBuf,
        remove_source: bool,
        label: &'static str,
    ) -> Arc<Self> {
        Self::with_resume(
            id,
            src_fs,
            dst_fs,
            src,
            dst,
            TransferOpts {
                remove_source,
                label,
                resume: false,
                overwrite: false,
            },
        )
    }

    /// 选项版本：`opts.resume` / `opts.overwrite` 分别控制续传与覆盖（见
    /// [`TransferOpts`] 与 `transfer_tree` 的判据）。
    pub fn with_resume(
        id: u64,
        src_fs: Arc<dyn FileSystem>,
        dst_fs: Arc<dyn FileSystem>,
        src: PathBuf,
        dst: PathBuf,
        opts: TransferOpts,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            src_fs,
            dst_fs,
            src,
            dst,
            flags: TreeFlags {
                remove_source: opts.remove_source,
                resume: opts.resume,
                overwrite: opts.overwrite,
            },
            label: opts.label,
            state: Arc::new(Mutex::new(OpInner::new())),
        })
    }
}

impl Operation for TransferOperation {
    fn id(&self) -> u64 {
        self.id
    }

    fn describe(&self) -> String {
        format!(
            "{} {} → {}",
            self.label,
            self.src.display(),
            self.dst.display()
        )
    }

    fn status(&self) -> OperationStatus {
        self.state.lock().status
    }

    fn progress(&self) -> (u64, u64) {
        let s = self.state.lock();
        (s.done, s.total)
    }

    fn cancel(&self) {
        self.state.lock().cancel = true;
    }

    fn pause(&self) {
        self.state.lock().pause = true;
    }

    fn resume(&self) {
        self.state.lock().pause = false;
    }

    /// 真有可停的地方：每个条目、每个文件前后都是检查点（与本地复制同一条约定）。
    fn pausable(&self) -> bool {
        true
    }

    fn run(&self) -> Result<(), MoError> {
        {
            let mut s = self.state.lock();
            if s.cancel {
                s.status = OperationStatus::Cancelled;
                return Ok(());
            }
            s.status = OperationStatus::Running;
        }

        // 见模块注释：本函数跑在 blocking 池线程，block_on 不违例。
        let rt = tokio::runtime::Handle::current();
        let result = rt.block_on(transfer_tree(
            self.src_fs.clone(),
            self.dst_fs.clone(),
            self.src.clone(),
            self.dst.clone(),
            self.flags,
            self.state.clone(),
        ));

        let mut s = self.state.lock();
        match result {
            Ok(()) => {
                s.status = OperationStatus::Completed;
                s.done = s.total;
                Ok(())
            }
            Err(e) => {
                // 取消是「用户要求的失败」，与真错误分开记：进度面板显示「已取消」，
                // 不要把 io 错误的红字糊上去。
                if s.cancel {
                    s.status = OperationStatus::Cancelled;
                    s.error = None;
                    return Ok(());
                }
                s.status = OperationStatus::Failed;
                s.error = Some(e.to_string());
                Err(e)
            }
        }
    }
}

/// 递归搬运：目录建目录再逐个子项，文件整份读写；每个检查点都尊重暂停 / 取消。
fn transfer_tree(
    src_fs: Arc<dyn FileSystem>,
    dst_fs: Arc<dyn FileSystem>,
    src: PathBuf,
    dst: PathBuf,
    flags: TreeFlags,
    state: Arc<Mutex<OpInner>>,
) -> BoxFut<Result<(), MoError>> {
    Box::pin(async move {
        if wait_if_paused(&state) {
            return Err(MoError::Cancelled);
        }

        if src_fs.is_dir(&src).await {
            // 续传 / 覆盖时目录不改名（直接用原目标路径），子项各自判断。
            let dst = if flags.resume || flags.overwrite {
                dst
            } else {
                free_path(&dst_fs, &dst).await
            };
            // 覆盖时同名目录已存在（合并语义）：create_dir 报错但目标确实在，
            // 就当建好了继续往里传；其它失败（权限等）原样上报。
            if let Err(e) = dst_fs.create_dir(&dst).await {
                let exists = flags.overwrite && dst_fs.metadata(&dst).await.is_ok();
                if !exists {
                    return Err(e);
                }
            }
            // 名字排一下序：递归顺序稳定，进度推进与「哪几个文件在传」都可复现。
            let mut children = src_fs.read_dir(&src).await?;
            children.sort_by(|a, b| a.name.cmp(&b.name));
            for child in children {
                transfer_tree(
                    src_fs.clone(),
                    dst_fs.clone(),
                    child.path.clone(),
                    dst.join(&child.name),
                    flags,
                    state.clone(),
                )
                .await?;
            }
            if flags.remove_source {
                src_fs.remove_dir(&src).await?;
            }
            return Ok(());
        }

        // 文件：先定 size（进度分母在写之前就该定），再决定能否续传。
        let size = src_fs.metadata(&src).await?.size;
        // 断点续传：仅当目标是「部分完成」（0 < 已传 < 源大小）才跳过 free_path 直接
        // 续写；否则退回常规行为（free_path 改名）。这样 Foreign/更大的文件绝不会被我们
        // 就地续写，避免旧尾部污染（与「永不静默覆盖」同一原则）。
        // 覆盖（冲突卡选了「覆盖」）也跳过 free_path：首块 TRUNCATE 照原名重写。
        let dst_size = if flags.resume {
            dst_fs.metadata(&dst).await.map(|m| m.size).ok()
        } else {
            None
        };
        let can_resume = flags.resume && matches!(dst_size, Some(s) if s > 0 && s < size);
        let dst = if can_resume || flags.overwrite {
            dst
        } else {
            free_path(&dst_fs, &dst).await
        };
        if let Some(parent) = dst.parent() {
            if !parent.as_os_str().is_empty() {
                // 目标父目录可能还不存在（拖到远程的某个新目录里）；建一次幂等。
                dst_fs.create_dir(parent).await?;
            }
        }
        let already = if can_resume { dst_size.unwrap() } else { 0 };
        {
            let mut s = state.lock();
            s.total += size;
            // 续传时把已传部分直接计入进度，进度条一开局就显示「已完成的比例」。
            s.done += already;
        }
        if size == 0 {
            // 空文件：写一块空内容把目标建出来（续传时空文件已存在，写空块无副作用）。
            dst_fs.write_file_chunk(&dst, 0, &[]).await?;
        } else {
            // 从已传字节处接着写，不再重传前面的部分。
            let mut offset: u64 = already;
            while offset < size {
                if wait_if_paused(&state) {
                    return Err(MoError::Cancelled);
                }
                let want = (size - offset).min(CHUNK_SIZE);
                let chunk = src_fs.read_file_chunk(&src, offset, want).await?;
                if chunk.is_empty() {
                    // 源端返回的块比预期短（不该发生，防御一下避免死循环）。
                    break;
                }
                dst_fs.write_file_chunk(&dst, offset, &chunk).await?;
                let written = chunk.len() as u64;
                {
                    let mut s = state.lock();
                    s.done += written;
                }
                offset += written;
            }
        }
        // 收尾：WebDAV 把攒在临时文件的整份 PUT 出去；本地 / FTP / SFTP 是空操作。
        // 必须在删源之前——传输语义是「先确保目标完整，再删源」。
        dst_fs.finalize_file_chunk(&dst).await?;
        if flags.remove_source {
            src_fs.remove_file(&src).await?;
        }
        Ok(())
    })
}

/// 目标已存在时改名（`a.txt` → `a 2.txt` → `a 3.txt`），**永不静默覆盖**。
///
/// 判据走**目标端点自己的** `metadata`：远程路径在本机 `exists()` 恒 false，
/// 与本地复制的 `ConflictPolicy::Rename` 是同一套命名约定，只是探测方式不同。
async fn free_path(dst_fs: &Arc<dyn FileSystem>, dst: &Path) -> PathBuf {
    if dst_fs.metadata(dst).await.is_err() {
        return dst.to_path_buf();
    }
    let Some(parent) = dst.parent() else {
        return dst.to_path_buf();
    };
    let stem = dst
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = dst
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 2..10_000 {
        let candidate = parent.join(format!("{stem} {i}{ext}"));
        if dst_fs.metadata(&candidate).await.is_err() {
            return candidate;
        }
    }
    dst.to_path_buf()
}
