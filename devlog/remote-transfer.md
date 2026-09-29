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
  好了）。`finalize` 在删源之前，保证「目标完整才删源」。
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

## 待办

* 大文件**整份进内存**已修（2026-09-29 起，2026-09-30 收尾四个后端）：`FileSystem` 加
  `read_file_chunk` / `write_file_chunk`，并加 `finalize_file_chunk`（默认空操作，仅 WebDAV
  覆写做「攒本地临时文件 → 整份 PUT 一次」）。四个后端写侧都**只按块进内存**（峰值 = 一个
  CHUNK_SIZE）：**local** seek 写、**ftp** 首块 STOR / 后续 APPE 顺序流、**sftp**
  `open`+seek+`write_all` 真随机写、**webdav** 各块落本地临时文件 + `finalize` 时单 PUT（不再
  每块 O(n²) 重传）。读侧 local/sftp/ftp 走 range/整份切片；**webdav 读侧仍整份进内存**（无
  Range GET，待做）。**断点续传没做**（中断从头重传）。
* 远程传输的**冲突策略**只做到「目标名去重」，没有冲突对话框 / 覆盖选项。
* 远程端点上的**撤销**（含删除）没有模型；`Reversible` 全是本地路径语义。
* 远程目录没有 watcher（改完必须重读），跨端点传输完成后靠调用方主动 `refresh`。
* SMB / NFS 仍走系统挂载（`mo_remote::mount`），挂载点对 `mo-fs` 来说就是本地路径，
  不需要 `TransferOperation` 这条链。
