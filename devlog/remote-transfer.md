# 跨端点传输（远程 ⇄ 本机）

phase-4 远程协议的主线补齐：**复制 / 移动在两个不同后端之间怎么办**。
在这之前 `transfer()` 一律造本机 `CopyOperation` / `MoveOperation`（`std::fs`），
拖着一批远程条目去粘贴，要么报「找不到文件」，要么更糟——「成功」地把远程路径当本机
路径处理。相关主题见 [async-runtime.md](async-runtime.md)（runtime 约定）、
[transfer-badge.md](transfer-badge.md)（进度 / 估速 / 暂停）。

## 1. 端点不能照路径猜（`Endpoint`）

* **两条路都问不出真话**：
  1. `Path::exists()` / `is_dir()` 对远程路径在本机**恒为假**（远程路径是服务器上的
     绝对路径），问本机磁盘只会得到「不存在」；
  2. 「它是不是当前列表里的那一行」（`goes_through_remote`，删除 / 重命名 / 新建的
     分流判据）只答得**这一页**的情况——粘贴到**当前目录**时 `dest` 自己不是列表里的
     一行；分栏拖拽时目标那一头根本不在源 `AppState` 的列表里。
* 于是端点由**拥有那一头的那一页**回答：
  ```rust
  pub enum Endpoint { Local, Remote(Arc<dyn FileSystem>) }
  pub fn endpoint(&self) -> Endpoint          // self 当前所在的一端
  pub async fn transfer(&self, paths, dest, move_)            // 两端都按 self 算（粘贴 / 同窗格）
  pub async fn transfer_between(&self, paths, src_ep, dest, dest_ep, move_)  // 两端由调用方给定
  ```
  `transfer()` 就是 `transfer_between(paths, here, dest, here, move_)` 的薄壳。
* UI 侧（`mo-ui::RootView::run_transfer`）源端取**拖拽来源窗格**的 `AppState`、目标端取
  **落点窗格**的（`drop_on_entry` 传落点窗格号、`drop_on_pane` 传自己的编号），各问各的
  `endpoint()`；源 / 目标正好同窗格时两端自然相同，与旧行为一致。
* ⚠️ **最危险的误判形态**：远程路径撞上本机**真实存在**的同名目录。远程根是 `/`，
  服务器上的 `/pub` 在本机也可能真有 `/pub`——端点判错时「以为是下载」会静悄悄写进
  本机那个目录。守卫因此**故意**把目标目录建成本机真实存在的临时目录
  （`uploading_to_a_remote_endpoint_writes_through_the_backend`）。
* 反向验证：临时把 `transfer()` 改回「照 `goes_through_remote(dest)` 判目标那一头」，
  `copying_into_the_current_remote_dir_goes_through_the_backend` 立刻红
  （远程页里粘贴到当前目录会被判成「下载到本机」）。

## 2. `FileSystem` 补两个方法

```rust
async fn read_file(&self, path: &Path) -> Result<Vec<u8>, MoError>   // 默认：返回「不支持」
async fn is_dir(&self, path: &Path) -> bool                          // 默认：能不能列目录
```

* 默认实现带**错误**而不是 `unimplemented!`：只有真能读内容的后端才覆写，省得每个实现
  被迫写一遍「不支持」；`is_dir` 的默认探测对没有元数据接口的后端够用。
* 各后端实现要点：
  - **local**：`std::fs::read` / `std::fs::metadata`（走 stat，与 `read_dir` 同语义）。
  - **sftp**：`s.read(path)` / `s.metadata(path).map(|a| a.is_dir())`。
  - **webdav**：`client.get(&remote).await` → `resp.bytes()`；`is_dir` 用 PROPFIND
    Depth-0 看 `resource_type.collection`（复用 `ok_prop` 的解析）。
  - **ftp**：`conn.retr(path, closure)`。⚠️ 两个签名坑：
    1. 闭包是 `FnMut(TransferStream) -> Pin<Box<dyn Future<Output = FtpResult<(U, TransferStream)>>>>`
       ——**必须把 stream 还回去**，suppaftp 靠它收尾；
    2. `TransferStream` 是 tokio 异步 IO，要 `use tokio::io::AsyncReadExt as _` 才有
       `read_to_end`；错误只能用 `suppaftp::FtpError::ConnectionError`（`FtpError` 没有
       `Io` 变体）。
    `is_dir` 用 `conn.size(path).await.is_err()`（SIZE 对目录报错）。

## 3. `TransferOperation`（mo-operations/src/transfer.rs）

* 一次传输 = 一对 `(src_fs, dst_fs)` + 两个路径 + `remove_source`。方向只影响两端谁读谁写
  与进度条上那个动词（上传 / 下载 / 复制），操作本身一视同仁。
* **逐文件分块流式**：目录先 `create_dir` 再按名字排序递归子项；文件按 `CHUNK_SIZE`
  （4 MiB）循环 `read_file_chunk` → `write_file_chunk`，进度分母（`total`）在循环前按
  `metadata.size` 定下、`done` 逐块累加。不赌协议自带的服务端 COPY（三家语义各不相同）。
  空文件单独写一块空内容建出。
* 写侧收尾：`write_file_chunk` 是「落一块」语义，**WebDAV 没有可靠的部分 PUT**——它在
  `write_file_chunk` 中只把各块攒到本地临时文件（`temp_dir()/mo-webdav-staging`，按远端路径
  派生文件名），传输循环对每个目标调一次 `finalize_file_chunk` 才整份 PUT 出去（必要时先
  `truncate` 清旧尾）。本地 / FTP / SFTP 的 `finalize_file_chunk` 是默认空操作（它们逐块就落
  好了）。  `finalize` 在删源之前，保证「目标完整才删源」。
* 读侧分块：local/sftp/ftp 走 `seek`/`read_at` 或整份切片；**webdav 额外发 `Range: bytes=…` 的
  GET**（206 即这一块，服务器不支持 Range 时回 200 整份、按区间切出）。传输的源若是 webdav，
  读也不再整份进内存。
* ⚠️ `async fn` **不能递归**（编译期要求定长 future）→ `transfer_tree` 走
  `Box::pin` 递归（`type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>`）。
* ⚠️ `run()` 里拿 `Handle::current()` 再 `block_on`——**只有在 `spawn_blocking` 里才安全**。
  `submit_operation` 正是这么调 `op.run()` 的（见 async-runtime.md）；哪天把 `run()` 挪到
  worker 线程上直接调，`block_on` 会 panic。
* 目标去重：`free_path(dst_fs, dst)` 用 `dst_fs.metadata()` 探（**不能**用 `Path::exists()`，
  远程路径问本机恒假），占用时按 `stem N.ext` 往后试——与 `unique_path` 同一套命名习惯，
  但判据必须走后端。`ConflictPolicy` 依旧是「永不静默覆盖」。
* 进度：读到的字节累加进 `total`、写成功的累加进 `done`，复用已有的 150ms 节拍广播；
  `pausable() -> true`，`transfer_tree` 每个节点前过一遍 `wait_if_paused` 协作检查点。
* ⚠️ **刻意不记可逆项**：撤销模型里的路径都是本地路径（`Reversible::Copy { dest }` 的撤销 =
  删掉 dest），对远程端点既删不动也删不对——宁可让「撤销」对这一步无效，也不做错事。

## 4. 测试

* `mo-operations/tests/transfer.rs`（6 条，内存 `MemFs` 实现 `FileSystem`）：整棵树走下来
  且字节数对得上、目标目录重名改成**并排**而不是合并、目标文件重名不覆盖、`move` 走完删源、
  开始前取消则一个字节都不写、下载落进本机树。用 `spawn_blocking` 包 `run()` 复现生产调用
  方式（不是直接 `block_on`）。
* `mo-app/tests/remote_local.rs`（4 条新守卫）：
  - `endpoint_follows_the_side_the_tab_is_browsing`：`Endpoint` 跟着页走（本地 → 远程 →
    `open_local` 后回本地，连接仍留着）；
  - `copying_into_the_current_remote_dir_goes_through_the_backend`：目标是**当前目录**
    （自己不在列表里）也必须发给后端——这条是模板里那个洞的正中靶心；
  - `uploading_to_a_remote_endpoint_writes_through_the_backend`：目标目录**本机真实存在**
    时不能落到本机磁盘，且不记可逆项；
  - `downloading_from_a_remote_endpoint_writes_locally`：下载侧内容真的落到本机。
  （假后端 `FakeRemoteFs` 为此补了 `read_file`，只认它列表里那一条。）

## 5. 断点续传（2026-09-29，21f21bb + 9e6d060）

**引擎**（mo-operations，`21f21bb`）：`TransferOperation::with_resume`（选项收进
`TransferOpts`，压住 clippy 7 参数上限）；`transfer_tree` 文件分支探测目标已传字节，
仅当 **`0 < 已传 < 源大小`** 才跳过 `free_path` 按断点续写、进度预置已传部分。
Foreign / 更大的文件照旧改名——绝不就地续写旧尾，与「永不静默覆盖」同一原则。
测试桩加 `min_read_offset`（初值 `u64::MAX`），反向验证：强制 `can_resume=false`
时续传用例红。

**接线**（mo-app + mo-ui，`9e6d060`）：

* `transfer_between` 提交前探测：远程 leg 目标里有部分完成的就**整批不提交**，
  交回 `TransferOutcome::NeedsResumeConfirmation(Box<PendingResume>)`（载荷带整批
  路径 + **发起方的 AppState**——操作管理器按标签页各一份，决策必须交回原来那
  一份）；没有就直接 `Started`。只对远程 leg 探测——本地对的冲突归
  `ConflictPolicy`，与续传无关。
* `resolve_resume(pending, ResumeDecision)` 三选一：**继续**（部分文件走续传，
  是否真续仍由 `can_resume` 把守）、**重传**（全部照常改名）、**跳过**（部分文件
  不提交）。粘贴（移动）链路置 `clear_staging_on_resolve`，决策后清暂存区，
  避免下一次粘贴撞一堆失效条目。
* UI 收口 `RootView::handle_transfer_outcome`（拖拽 / 外部拖入 / 暂存粘贴 /
  剪贴板粘贴五处共用）：等决策弹 `Modal::ConfirmResume`。载荷不进 Modal 枚举
  （要保 `PartialEq, Eq`），放 `RootView::resume_pending`；卡上继续 / 重传 /
  跳过 / 取消四颗按钮，Esc / 点遮罩**什么都不提交**。
* 测试：`mo-app/tests/resume.rs`（探测不提交 + 三种决策，端点直传 `Endpoint::Remote`
  假后端、不需要 SessionRegistry）；mo-ui 内联 headless 用例（按钮齐全 +
  取消 / 跳过收卡清请求；「继续」会真提交 IO，headless 不点，由上面两条钉住）。

**已知边界**：探测对整批各多两次 `metadata`（远程 = 网络往返），大批量时有感
知成本；决策期间目标被别人动过 → `can_resume` 自动退回改名重传，不会续坏。

## 6. 传输完成自动刷新（2026-09-29，8d67d31）

跨端点传输完成后，远程目标端没有 watcher，等用户手动刷新才看得见新文件；原先两处
「提交完立刻 `refresh`」时机又太早（大文件还在传）。收口到 `handle_transfer_outcome`：

* `watch_then_refresh_task(src_app, ids, dest_app, dest)`：订阅**发起方**总线，等这批
  op 全部 `OperationFinished`（成败都发，lib.rs 的 `submit_operation` 发布）后对目标端
  `AppState::refresh_if_showing(&dest)`——只刷「当前目录 == 落点」的那一端，用户已切走
  就不白费 IO（切回来时本来就会重读）。为什么在 UI 层做：完成事件只发在发起方总线，
  目标端可能是另一条会话的 `AppState`，只有 UI 同时握着两端。
* 续传决策重提同款：弹卡时把 `(dest_app, dest)` 记进 `RootView::resume_dest`，
  `resolve_resume_decision` 拿到 `resolve_resume` 返回的 ids 后挂同一任务（跳过决策
  ids 为空则不刷）。
* 拖拽 / 外部拖入原先散落的 refresh 收编，`submit_os_drop` 的 `refresh` 参数删除。
* Lagged（广播掉队）按「齐了」处理——刷新是尽力而为，别为它挂死等待。

## 待办

* 大文件**整份进内存**已修（2026-09-29 起，2026-09-30 收尾四个后端）：`FileSystem` 加
  `read_file_chunk` / `write_file_chunk`，并加 `finalize_file_chunk`（默认空操作，仅 WebDAV
  覆写做「攒本地临时文件 → 整份 PUT 一次」）。四个后端写侧都**只按块进内存**（峰值 = 一个
  CHUNK_SIZE）：**local** seek 写、**ftp** 首块 STOR / 后续 APPE 顺序流、**sftp**
  `open`+seek+`write_all` 真随机写、**webdav** 各块落本地临时文件 + `finalize` 时单 PUT（不再
  每块 O(n²) 重传）。读侧 local/sftp/ftp 走 range/整份切片；**webdav 读侧也已加 Range GET**
  （`read_file_chunk` 发 `Range: bytes=…` 的 GET，206 即这一块；服务器不支持 Range 时回 200
  整份，按区间切出、行为仍正确但内存退回整份——服务器限制）。断点续传已做（§5，重提弹确认卡，无 journal、靠磁盘上的部分文件判断，进程内 / 重启后都能续）。
* 远程传输的**冲突策略**只做到「目标名去重」，没有冲突对话框 / 覆盖选项。
* 远程端点上的**撤销**（含删除）没有模型；`Reversible` 全是本地路径语义。
* 远程目录没有 watcher（改完必须重读）——传输完成的自动刷新已做（§6），但**其它**改动
  （另一台设备写入、本进程外部的操作）仍要用户手动刷新或重进目录。
* SMB / NFS 仍走系统挂载（`mo_remote::mount`），挂载点对 `mo-fs` 来说就是本地路径，
  不需要 `TransferOperation` 这条链。
