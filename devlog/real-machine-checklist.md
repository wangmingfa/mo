# 真机验证清单（2026-10-08 盘点）

> headless 判不了、只能人验的项，按平台分组。每条带出处（devlog §），
> 验完勾掉并注明日期。**此文件是工作清单，未提交。**

## Windows

- [ ] **拖出到资源管理器**（windows-port.md §25/§35 收账，欠得最久的一条）
  按住一个文件真拖到资源管理器上松手：① 落不落子；② 结论是不是复制（源文件应留存）；
  ③ ghost 在出手瞬间是否正确消失（§42 顺带项）。headless 做不到，得人来。

- [ ] **拖拽 ghost 观感**（windows-port.md §42「人工实感清单」）
  ① ghost 字色 / 阴影在浅色与深色主题下是否协调；② 快速拖动时淡染 + 高亮有无闪烁感；
  ③ 分栏（跨窗格）拖动的窗格淡染实际观感（headless 只验了单窗格）。

- [ ] **7z / rar 解压**（windows-port.md §47 附近欠账）
  本机装 7z 后：右键一个 .7z / .rar → 解压 → 条目数与内容对。CI 只验了 zip/tar。

## macOS

- [ ] **`pick_folder` 与「从磁盘安装」前半段**（plugin-system.md §4.12 欠账 5/6）
  扩展管理器 → 底部「从磁盘安装…」→ 应弹原生 NSOpenPanel → 选一个含
  `manifest.json` 的目录 → 装上、扩展卡出现。这段链 headless 弹不出原生对话框，
  objc 是照惯例写的、没在真机点过一次。

## Linux

- [ ] **`unshifted_key` 折键单测**（windows-port.md §22 Linux 段，2026-10-08 落地）
  `cargo test -p mo-platform unshifted_key`：`unshifted_key_follows_german_layout`
  在 `XKB_DEFAULT_LAYOUT=de` 下应过（`?`→`ß`）。本机无 libxkbcommon，没法代跑。

- [ ] **非 US 布局应用内键位**（同上，「整体真机行为」验收项）
  真机 cargo run 后切德语（或任一非 US）布局，按 `?` / `:` / `§` 等符号键：
  默认键位（删除、搜索焦点等）不应串键；顺带整体走一遍导航 / 视图切换
  （gpui Linux 后端此前只编译过、没点过）。

## 今日四线专项（2026-10-08）

- [ ] **搜索 FTS5 旧库迁移**（windows-port.md §49）
  拿已有的旧索引库（58 万行那种）启动：① 首开应有一次两三秒的回填停顿（一次性）；
  ② 之后全局搜索中缀子串应秒回（原先 ~150ms）；③ 搜索应能命中「文件名中间」的词。

- [ ] **远程内容级自动刷新**（remote-transfer.md §11）
  连一台 SFTP/FTP/WebDAV，进入某目录后**在另一头**改写其中一个文件的内容
  （比如服务器上 `echo x >> file`）：① 5 秒内 Mo 应自动刷新出新大小；② 顺带确认
  远程文件的大小 / 日期列**有值**（原先一直空白——seed 修复顺带的收益）。

- [ ] **SMB/NFS 挂载传输**（remote-transfer.md §12）
  挂一块 SMB 或 NFS：① 本地 → 挂载点复制，应出传输面板（走 TransferOperation，
  不是静默直拷）；② 本地 → 挂载点**移动**，应完成「复制后删源」，不再报
  EXDEV / rename 失败；③ 传大件中途点取消，应停得下来；④ 刚挂载完立刻拖
  （验权威挂载读取，不吃侧边栏缓存陈旧值）。
