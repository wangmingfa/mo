# 回收站统一：系统废纸篓 + Mo 账本（双入口收敛）

2026-09-24。起因是 UX：右键菜单同时有「移到废纸篓」和「移到系统废纸篓」，
用户每次都要想「该选哪个」——把实现细节泄漏给了用户。Finder / Explorer /
Files 都只有一个废纸篓概念。

## 探针实测（决定方案的证据）

三个实验，全部当场验证：

1. **`trashItemAtURL:resultingItemURL:error:` 返回落点**。Swift 小脚本确认
   `resultingItemURL` 给出 `~/.Trash/<name>`。这是自记账的通路：系统负责搬
   （重名自动改名、外接卷进卷上 `.Trashes/<uid>`），Mo 拿回落点记账。
   ⚠️ JXA（osascript -l JavaScript）桥接出参会在打印时段错误，别用；用 Swift。
2. **非 Finder 删除没有 put-back 记录**。`trashItem` 删的两个文件在
   `~/.Trash/.DS_Store` 里**零** `ptbL`/`ptbN` 记录；同文件里 477 条存量记录
   全是 Finder 删除写的。→ Finder 对 Mo 删的文件也放不回去，Mo 账本是唯一
   可靠的还原依据。
3. **跨卷落点系统自理**。HFS+ DMG 上 `trashItem` 删除 →
   `/Volumes/<vol>/.Trashes/<uid>/`，落点照样由 resulting URL 返回。

附：`.DS_Store` put-back 记录格式（仅存档，生产不用）：
`[u32 名长][UTF-16BE 名][ptbL|ptbN][ustr][u32 字符数][UTF-16BE 值]`；
`ptbL`=原目录（无前导 `/`），`ptbN`=原名（进篓被改名时记**原名**）。
Finder AppleScript 探针被 TCC 挡（-10004），未 live 验证写入时机。

## 最终设计

UI 永远只有一个「移到废纸篓」/ 一个回收站面板。平台后端分治：

| 平台 | 删除 | 账本 | 还原 |
|---|---|---|---|
| macOS（本轮） | `mo_platform::recycle_one`（`trashItemAtURL`） | Mo 账本 `index.json`（记 `trashed` 实际落点） | 照账本搬回 |
| Windows（phase-5） | 系统回收站 | `$I` 文件 / `System.Recycle.*` 属性，公开 | 系统/`$I` 原路径 |
| Linux（phase-5） | FreeDesktop trash | `.trashinfo`，规范字段 | 读 `Path` 搬回 |

「自记账」是 macOS 实现的内部细节，不是产品语义；Windows/Linux 纯系统、
零 Mo metadata。`.DS_Store` 解析**不做**：面板只显示 Mo 账本（用户决定：
不显示系统回收站相关内容）。

## 代码落点

* `mo-platform::recycle_one(path) -> Result<PathBuf>`：单条送系统废纸篓并
  返回落点（替代批量 `recycle`，逐条返回让「部分成功」可记账）。
* `mo_operations::Trash` 双模式：
  * 隔离模式（默认 / 测试）：`trash()` 自己 `move_path` 进
    `<root>/<uuid>/<原名>`，旧行为不变；
  * 系统模式（`Trash::with_mover`）：搬移交给注入的 `TrashMover`
    （生产 = `recycle_one`），`root` 只放 `index.json`。
  * restore/purge/empty 的父目录清理加 `is_isolated` 守卫——旧条目的
    `<uuid>/` 空壳可以整目录删；系统条目的父目录是 `~/.Trash` /
    `.Trashes`，**绝不能动**。
* `AppState::build(.., system_trash: bool)`：`new()` 生产 = 系统模式；
  `with_trash` / `with_sessions` / `with_staging`（全部测试用）= 隔离模式，
  头less 测试永远不碰真实废纸篓。
* 删除 `recycle_to_system` + 菜单项「移到系统废纸篓」+ 命令面板
  `RecycleToSystem`（mo-ui app.rs / context_menu.rs）。

## 兼容性

* 旧版 `~/.mo-trash/<uuid>/<原名>` 条目照常列出 / 还原 / 清理（`is_isolated`
  按路径前缀判定）；无需迁移。
* 账本悬空（用户从 Finder 清倒废纸篓）：restore/purge 里 `remove_file` 都是
  `let _`，索引照常抹掉——语义是「目的已达一半」，不报错。
* 外接卷 / 网络盘：系统就地进卷上 `.Trashes`，不再「搬回本机」（旧方案的大
  文件慢点消失了）。

## 测试

* `mo-operations`：系统模式两条新用例（假搬移器）——落点记账、purge 不动
  废纸篓目录本身、restore 照账本回搬、empty 保留目录。
* `mo-platform`：`recycling_takes_the_file_out_of_place` 改断言返回落点且
  落点可删（真 AppKit，`NSFileManager` 不要求主线程所以敢进单测）。
* `remote_local::host_only_actions_refuse_remote_paths` 收窄为只测
  reveal（system trash 通道已删）。
