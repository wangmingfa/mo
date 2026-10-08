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

## 7. 冲突确认卡（2026-09-29，97d669c）

远程目标重名不再静默改名——提交前探测分类 + `Modal::ConfirmConflict` 三选一：

* **分类**（`transfer_between`，与续传共用同一次 `metadata` 探测）：
  `0 < 已传 < 源大小` → 续传候选；其余已存在（完整同名 / 更大 / 目录）→ 冲突。
  注意「目标比源**短**的完整文件」按续传语义处理而不是冲突。**冲突优先于续传**：
  一批里两种都有时只弹冲突卡。
* **引擎**（mo-operations）：`TransferOpts.overwrite`；`remove_source / resume /
  overwrite` 收成 `TreeFlags` 把 `transfer_tree` 压回 clippy 7 参内。覆盖 = 不
  `free_path`、首块 TRUNCATE 照原名重写；目录是**合并**语义——`create_dir` 撞
  已存在且 overwrite 时继续往里传，其它失败原样上报。
* **应用层**：`resolve_conflict(pending, ConflictDecision)` 三选一（Overwrite /
  Rename / Skip，取消 = 丢弃请求）；`submit_transfer_entry` 的两个布尔收成
  `SubmitMode` 三态压回 7 参。非冲突文件挂 Resume，批里并存的部分完成文件仍可
  续写（`can_resume` 兜底）。
* **UI**：与续传卡同外壳同交互（Esc / 点遮罩什么都不提交）；按钮顺序把危险的
  「覆盖」放最右、不给主色，主色与 Enter 都给安全的「改名」；决策提交后同款挂
  完成即刷新（§6 的 `watch_then_refresh_task`）。
* **测试**：`mo-app/tests/conflict.rs`（探测不提交 + 三决策；桩补 `offset==0`
  TRUNCATE 语义——纯区间替换桩会让覆盖写残留旧尾，四个真实后端都是清的）；
  mo-ui headless 用例。反向验证：禁用探测 → 4 条全红。

## 8. 远程撤销模型（2026-09-29，e6e5b5f）

⌘Z 不再对远程无效——`Reversible` 增加三个远程变体，撤销记录在**操作发生时**
把那一端的后端捕获进来（`Arc<dyn FileSystem>`，克隆只是引用计数，会话生命周期
仍归 `SessionRegistry`）：

* **变体**：`RemoteRename`（同一后端上正逆 rename）、`RemoteCopy`（撤销 = 删掉
  目标侧副本，`remove_dir` 递归；重做 = 原样再传）、`RemoteMove`（正逆都是
  「反向再移一遍」，走 `TransferOperation`——进度与取消白拿）。`apply_reversible`
  按变体分流，原来的「`goes_through_remote` 猜 + 远程 Move 特判」整段删掉：
  那个判据只答得**当前这一页**，撤销往往发生在切走之后。
* **推送方向 bug（顺手钉住）**：`rename_many` 本地分支把可逆项按 from/to **交换**
  推——undo 算出来的恰是正向改名（旧名已不在，rename 静默失败），⌘Z 看上去
  什么都不做。没有测试守着所以一直没发现。现在一律按**正向**存，逆操作由
  `apply_reversible` 反转；`undo_reverts_a_rename` 钉住，反向验证红过。
* **只记普通提交**：`submit_transfer_entry` 记远程可逆项，但 `SubmitMode` 为
  Resume / Overwrite 时不记——部分文件是上次取消留下的、被覆盖的旧内容已经
  没了，撤销会把不是这次产生的东西一并抹掉。宁可 ⌘Z 无效，不做错事。
* **预去重**：撤销记录必须写**真实落点**，而引擎是在 `run()` 时才去重的——
  冲突卡选「改名」后实际落在 `a 2.bin`，记录若写请求名，⌘Z 会删掉被顶掉的
  **原名**（别人的文件）。提交方先 `unique_remote_path`（与引擎 `free_path`
  同一套 `stem N.ext` 命名、同一个「先探原名」顺序）探出落点再传。
* **刷新**：撤销 / 重做提交的传输完成后刷新受影响目录（`watch_remote_undo_refresh`
  订阅自己的总线等 `OperationFinished`，与 §6 的 UI 层任务同套路——撤销只发生在
  发起方这一个 `AppState` 里，不用跨端）。
* **远程删除仍不可撤销**：服务端没有回收站，删掉的内容无处还原，不记变体。

测试：`undo_remote.rs` 五条（复制撤销 / 重做、移动撤销、改名落点、上传撤销）；
`remote_local.rs` 远程重命名撤销（含**切回本地之后**再 ⌘Z 那条——捕获后端的
存在理由）；旧守卫「远程传输不记可逆项」按新语义翻转（上传后可撤销、撤销发
`remove_file`、本地源不动）。假后端 `apply_log` 改成按序重放 rename——
「只看最后一次」的旧模型还原不了改名回退。

## 9. 远程目录轮询泵（2026-09-29，f4f4ec1）

* **缺口**：传输完成的自动刷新（§6）只覆盖「自己发起的操作」；另一台设备写入、
  本进程外部的操作，用户停在远程目录上时仍然看不见。远程协议（FTP / SFTP /
  WebDAV）都没有事件推送，唯一可行的是**轮询**。
* `spawn_remote_poll_pump`（与 `spawn_watcher_pump` 配对启动，四个标签页构造点
  + `run()`）：5s 一拍，只处理**当前远程目录**（`browsing_remote()` 不过就跳过，
  本地目录有 watcher，一拍都不花）。
* **有差异才重读**：每拍一次列目录往返做「路径 + 类型 + size + mtime」集合比对
  （`BTreeSet<(PathBuf, bool, u64, u64)>`），集合没变直接返回——闲着 = 一次往返、
  零 UI 动作；变了才整目录 `refresh`。
  ⚠️ 比对不再只看待「路径 + 类型」：**原地改写内容**（大小 / mtime 变了、条目集合
  没变）现在也抓得到**——因为 SFTP / FTP / WebDAV 的目录列表本就一并返回 size /
  mtime，比对的「新值」直接从列表拿、「旧值」来自 `load_path` 时 seed 进条目的
  size/mtime，零额外网络往返**；不必逐条 metadata（每条目一次网络往返，轮询干不起）。
  间隔 5s 与闲置探活 30s 不同量级，互不干扰。
* **三种跳过**：目录在读中（`loading`，没有可比的稳定快照）、读失败（连接抖了，
  不把坏连接刷成报错弹窗，自愈交给 `load_path` 的 revive 路径）、比对通过后用户
  已切走（refresh 前再验一次 `current_path`，别替别人白读一趟）。
* 测试：`remote_poll.rs` 四条，直接驱动 `poll_remote_listing_once` 这一轮
  （泵的 5s 节拍 headless 等不起）：外部新建出现 / 外部删除消失 / 没变不重读
  （读计数：有差异 = 比对+重读 2 次，无差异 = 每拍只 +1）/ 读失败不炸、自愈后
  照常抓到。假后端的列表放在共享 `Mutex` 里，测试在两拍之间改它扮演「另一台设备」。
* ⚠️ 测试坑（单跑全绿、一轮全红的典型）：**会话表必须每用例独占**——
  `AppState::new` 用进程级 `SessionRegistry`，同端点 + 同用户名被去重成一行，
  并行用例后装的假后端把先装的顶掉，「外部改动」就互相窜了。要
  `AppState::with_sessions(trash, Arc::new(SessionRegistry::new()))`。

## 待办

* 大文件**整份进内存**已修（2026-09-29 起，2026-09-30 收尾四个后端）：`FileSystem` 加
  `read_file_chunk` / `write_file_chunk`，并加 `finalize_file_chunk`（默认空操作，仅 WebDAV
  覆写做「攒本地临时文件 → 整份 PUT 一次」）。四个后端写侧都**只按块进内存**（峰值 = 一个
  CHUNK_SIZE）：**local** seek 写、**ftp** 首块 STOR / 后续 APPE 顺序流、**sftp**
  `open`+seek+`write_all` 真随机写、**webdav** 各块落本地临时文件 + `finalize` 时单 PUT（不再
  每块 O(n²) 重传）。读侧 local/sftp/ftp 走 range/整份切片；**webdav 读侧也已加 Range GET**
  （`read_file_chunk` 发 `Range: bytes=…` 的 GET，206 即这一块；服务器不支持 Range 时回 200
  整份，按区间切出、行为仍正确但内存退回整份——服务器限制）。断点续传已做（§5，重提弹确认卡，无 journal、靠磁盘上的部分文件判断，进程内 / 重启后都能续）。
* ~~远程传输的**冲突策略**只做到「目标名去重」~~ 已做（§7，批级三选一 + 取消；
  ~~本地对的冲突对话框仍没有~~ 2026-09-29 也已做（`38d711a`，见 §10）——两端都
  本地时探测照样跑，撞同名也弹那张卡；逐个文件决策仍没有（批级三选一是刻意粒度）。
* ~~远程端点上的**撤销**（含删除）没有模型；`Reversible` 全是本地路径语义~~
  已做（§8：三个远程变体捕获后端；删除仍不可撤销——服务端没有回收站）。
* ~~远程目录没有 watcher（改完必须重读）——其它改动仍要手动刷新~~
  列表级 + 内容级都已做（§9：5s 轮询比对当前远程目录，键含 size/mtime，有差异才
  重读；「原地改写内容」不再靠手动刷新兜底）。远程 `ReadDirEntry` 现带 size/mtime，
  `load_path` 时 seed 进条目，模型侧与列表侧同源比对、epoch 一致不会误判。
* ~~SMB / NFS 仍走系统挂载（`mo_remote::mount`），挂载点对 `mo-fs` 来说就是本地路径，
  不需要 `TransferOperation` 这条链~~ **已做（§12）**：挂载点虽是本地路径，但落在网络
  文件系统上，传输现已接入 `TransferOperation`——分块 / 续传 / 统一进度 / 跨文件系统移动安全。

## §10：本地对的冲突对话框（2026-09-29，38d711a）

* **症状（同一动作两种反馈）**：远程 leg 撞同名会弹冲突卡（覆盖 / 改名 / 跳过），
  本机复制 / 移动撞同名却**静默改名**成 `a 2.txt`——用户只看到多出一个副本，
  不知道发生过冲突。
* **根因**：`resolve_transfer_leg` 对 `(Local, Local)` 返回 `None`（本地对没有
  leg，走本机队列 `CopyOperation` / `MoveOperation`），而探测循环写成
  `let Some((src_fs, dst_fs, _)) = resolve_transfer_leg(..) else { continue; }`——
  本地对整条探测被跳过，落到 `ConflictPolicy::default()` = `Rename`。
* **修法**：
  1. 探测扩到本地对：没 leg 时用 `LocalFileSystem` 探测两端。**只判冲突不判续传**——
     本机复制不落「部分完成」的中间文件（要么没写、要么写完整），把「目标比源
     短」判成续传会误伤用户本来就有的同名小文件。
  2. 覆盖必须真生效：`submit_transfer_entry` 的本地分支原来恒用 `Rename`，
     选了「覆盖」照样改名。改成按 `SubmitMode` 选 `ConflictPolicy::Overwrite`。
  3. 覆盖提交**不记可逆项**（本地与远程同一条理由）：落点在本次操作之前就存在、
     旧内容已经没了，撤销只能把不是这次产生的东西抹掉——覆盖复制的逆操作是
     「删掉目标」，用户要旧内容、拿到的是文件消失。
* **测试**：`tests/conflict.rs` 加本地对四条（整批扣住 / 覆盖 / 改名 / 跳过），
  真临时目录 + 真 `LocalFileSystem`（覆盖这条用假后端测不到）。
  反向验证：撤探测修复 → 四条全红；撤 `policy` 修复 → 覆盖那条红。
* **UI 侧零改动**：`handle_transfer_outcome` 早就处理 `NeedsConflictConfirmation`，
  本地对只是以前**不返回**这个结局。

## §11：远程内容级刷新落地（2026-10-08）

* **根因**：`poll_remote_listing_once` 的「有差异才重读」只比对 `(path, kind)` 集合，
  列表级变化（增 / 删 / 改名）能抓，「原地改写内容」（size / mtime 变了、集合没变）
  抓不到。devlog 原记「逐条 metadata 轮询干不起」——但那是**列表之外再逐个 stat**。
  远程目录列表（SFTP `readdir` / FTP `LIST/MLSD` / WebDAV `PROPFIND`）本就一并返回
  size / mtime，被 `read_dir` 构造 `ReadDirEntry` 时丢掉了。
* **修法**：
  1. `mo-fs/src/reader.rs`：`ReadDirEntry` 加 `size: u64` / `modified: Option<SystemTime>`
     （`new` 签名不变，加 `with_metadata` builder 以免波及既有调用点）。
  2. 三个远程后端在列目录时就填：`sftp` `collect_entries` 取 `DirEntry::metadata()`
     的 `size`/`mtime`；`ftp` `entries_in` 取 `File::size()`(usize)/`modified()`(SystemTime)；
     `webdav` `entries_from` 取 `prop.content_length`/`last_modified`（与各自 `metadata()`
     同款映射）。本地后端不填（走后台 scheduler 逐条 stat），行为不变。
  3. `mo-app/src/lib.rs` `load_path`：条目带 size/modified 时用它 **seed `Entry.metadata`**
     ——既让远程大小 / 日期能显示（原本 `MetadataScheduler` 用 `std::fs::metadata` 对远程
     路径必失败、远程元数据一直是 Loading），又给轮询泵一个可比对的「旧值」。
  4. 轮询泵比对键扩成 `(path, kind, size, mtime_secs)`，size 或 mtime 变即触发整目录
     `refresh`。模型侧旧值取自 seed 的元数据、列表侧新值取自本轮 `read_dir`，**同源、
     epoch 一致不会误判**；零额外网络往返。
* **验证**：`mo-app/tests/remote_poll.rs` 加 `poll_detects_in_place_content_change`
  （同路径 size/mtime 变、集合没变 → 比对 + 重读两拍、刷新后拿新 size）；原有 4 条
  仍绿。fmt/clippy（`-D warnings`）干净。related：`resume`/`undo_remote`/`conflict`/
  `remote_local` 共 37 例、`sort_metadata`/`thumbnail_carry`/`navigation`/`productivity`/
  `column_refresh` 全绿。
* **剩余账**：mtime 按秒比对，亚秒级改写（极少见）要等下一拍；FTP `LIST` 的 mtime 来自
  文本解析、个别服务器时区/精度不稳，可能偶发冗余刷新（无害，只是多一次重读）；本地
  目录仍走旧 scheduler 路径，不受影响。

## §12：SMB / NFS 挂载接入 TransferOperation 链（2026-10-08）

* **根因**：`mo_remote::mount` 把 SMB / NFS 交给系统挂载，挂载点对 `mo-fs` 来说就是本地
  路径；`endpoint()` 对挂载点返回 `Endpoint::Local`，于是传输走本机 `CopyOperation` /
  `MoveOperation`，不走 `TransferOperation`。两点由此吃亏：① 网络盘是网络文件系统，
  **移动**用 `std::fs::rename` 跨文件系统必败；② 拿不到 `TransferOperation` 的分块流式
  （大文件不整份进内存）、断点续传、统一进度 / 暂停 / 取消。
* **修法**：给 `Endpoint` 加 `NetworkMount(Arc<dyn FileSystem>)`（承载 `LocalFileSystem`——
  挂载点读写仍走 `std::fs`），传输分发里把「落在已挂载网络盘下的路径」升级成该变体：
  1. `mo-remote/src/mount.rs` 加 `is_under_mounted_share(path, shares)`：按组件前缀匹配
     `mounted_shares()` 的挂载点（`/Volumes/share` 不误命中 `/Volumes/shareX`）。
  2. `mo-app/src/lib.rs` 加 `net_endpoint(base, path, shares, local)`（路径在网络盘下 →
     `NetworkMount`，否则原样返回），`transfer_between` / `resolve_resume` /
     `resolve_conflict` 的探测与提交循环都按路径逐条重分类。~~shares 走
     `mounted_network_shares()` 权威读（绕过侧边栏 TTL 缓存）~~ **订正（同日晚，
     全量 CI 抓的）**：权威读是 `spawn_blocking` 跑 `mount` 子进程——传输链的 await
     一旦挂上真线程，`run_until_parked` 不等它（§45 老坑），`column_drag` 六条墙钟
     轮询测试当场红了五条。改走 [`network_shares`] 缓存（纯锁读、零 IO；侧栏每帧
     都在问、过期自动后台单飞，应用内挂载 / 卸载另有 `invalidate_net_shares` 立即
     作废）——用户能对挂载点做传输的前提是他先在界面上看到它，缓存那时早已刷新。
  3. `resolve_transfer_leg` 把 `NetworkMount` 与 `Remote` 同等待遇（`Local↔NetworkMount`
     标「上传 / 下载」、两块挂载之间 / 与远程混搭标「复制」），于是走 `TransferOperation`。
     `submit_transfer_entry` 的 leg 分支照旧：记录 `Reversible::RemoteCopy/Move`（用
     `LocalFileSystem` 删挂载点上的落点，撤销正确）、`record_history(remote=true)`。
  4. `MoveOperation` 的跨文件系统 rename 失败被彻底绕开：`TransferOperation` 是「复制后
     删源」，对网络盘天然安全；`same_session(NetworkMount, Local)` 返回 false → 拖放默认
     复制（保守、不丢数据）。
* **验证**：`mo-remote` 加 `path_under_mounted_share_detects_network`（命中 / 挂载点本身 /
  同名前缀不误判 / 无关路径不命中）；`mo-app` 加 `transfer_net_tests` 两个（`network_mount_
  routes_through_transfer_operation`、`network_mount_is_not_same_session_as_local`）。
  `remote_local`(27) / `resume`(5) / `conflict`(8) / `undo_remote`(5) / `drag_semantics`(6)
  全部仍绿；fmt / clippy（`-D warnings`）干净。
* **剩余账**：本机对（两端都真本地）仍走 `CopyOperation` / `MoveOperation`——这是对的，本机
  复制不需要 `TransferOperation` 的开销；只有「任一端落在网络挂载下」才升级。网络盘与真
  远程会话的语义差异（`Remote` 用各自 1-worker runtime、`NetworkMount` 用 `LocalFileSystem`
  走系统挂载点）在传输层已统一为同一套分块 / 续传 / 进度逻辑。
