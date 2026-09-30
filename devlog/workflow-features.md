# devlog · 进阶工作流四件套（暂存区 / 磁盘地图 / 双栏差异 / 内容搜索）

四项是同一轮「发散特色功能」里挑出来的，按顺序 ③→①→④→② 实现。
测试全绿（`mo-ui` 118 + `mo-app` 全 + `mo-search` 19），clippy workspace 干净。

## ③ 暂存区 / 收集夹（2026-09-23）

### 痛点
「从 8 个文件夹各挑 3 个文件再统一操作」是文件管理器最别扭的场景。剪贴板是「替换 + 立即粘贴」，满足不了「先挑一堆、稍后统一处理」。

### 设计
- **进程级清单** `mo-app::Staging`（`staging()` 走 `OnceLock`，所有窗格/标签页共享同一份累加）。
  - `collect(from, items)` 按 `path` 去重；记录每条的 `from`（来源目录）。
  - 复制保留清单、移动清空——这是和剪贴板最本质的语义区别。
- **侧栏抽屉** `mo-ui::staging::render_tray`：常驻、占布局（不是浮层）。列出图标 + 名称 + 来源目录 + `×`（`stop_propagation` 防误触整行）。
- 所有动作路由到既有 `transfer()`，跨端点判定零重复。

### 键位 / 命令
- `cmd+shift+s` 收集当前选择（自动开抽屉；0 条时通知）。
- `cmd+alt+s` 开关抽屉。`CommandId::{StageSelection,ToggleStaging,ClearStaging,StagedCopyHere,StagedMoveHere}`。

### ⚠️ 进程级状态 vs 每窗格 UI 状态
清单是进程级（共享），抽屉开合是 `RootView` 每实例——别把抽屉开关写进 `Staging` 本身。

## ① 磁盘地图 Treemap（2026-09-23）

### 设计
- **squarified treemap**（Bruls/Huizing/van Wijk，`mo-app::treemap`）：递归切最长边，单行 worst aspect ratio 改善才继续加项。
- `Rect` **归一化 0..1**：布局与像素解耦，渲染时按容器尺寸缩放——这样单测能脱离窗口尺寸验证比例。
- `AppState::usage_tree`：`spawn_blocking`；节点上限 `USAGE_TREE_BUDGET=4000`；
  `symlink_metadata` 不跟软链；按大小排序保证确定性。
- UI 两视图：`usage_treemap`（绝对定位色块，`mo-usage-tile-{i}`；点击下钻、双击打开）
  与 `usage_bars`（原条形图）。`m` 键在两种模式间切。色块按类型上色（目录灰蓝 / 图片 / 视频 / 音频 / 压缩 / 代码 / 文档）。

### 单测
`areas_are_proportional_to_sizes` / `tiles_never_overlap` / `tiles_stay_reasonably_square`
/ `directories_expand_until_max_depth` / `zero_sized_entries_are_omitted` + 一个布局测试
（`disk_usage_treemap_tiles_are_proportional` 注入 600/300/100 的假树，断言面积比 ≈ 0.6/0.3/0.1）。

## ④ 双栏差异着色（2026-09-23）

### 关键决断：双栏是「两侧各视角」，不是中立报告
`mo_diff::compare_trees` 本来出一份 `TreeComparison`，但双栏浏览要把差异**染在各自的行上**：
- `LeftOnly` → 只染左栏（右栏压根没这一行，染上去就是凭空造记录）
- `RightOnly` → 只染右栏
- `Different` → 两侧都染
- `Identical` → 不染（整屏都染就没差异可言）

`compare_maps` 只取**直接子项**（`rel.components().count()==1`）；深层差异归到它所在的子目录上。

### 着色叠加顺序
行底色优先级：`selected` > `compare_tint`（低 alpha，叠在 zebra 之上但不盖选中/悬浮）> `zebra` > `surface`。
低 alpha 是刻意的——差异是「辅助信息」，不能喧宾夺主。

### 键位
- `compare.toggle` = `cmd+alt+c`（要求已分栏 + 两窗格）；`compare.jump_next/prev` = `cmd+alt+down/up`。
- 图例条 `compare_legend` 常驻状态栏上方、占布局，可一键关闭；`refresh_compare_if_stale` 从 `sync_panel` 钩子调（导航后失效重算）。
- 列表 / 网格 / 列（Miller）三视图都接了 `panel.diff`。

### 单测
`app::tests::compare_maps_tint_each_side_from_its_own_view` / `compare_tint_leaves_identical_rows_alone`
+ 布局测试 `compare_legend_occupies_a_row_and_closes`。

## ② 内容搜索 grep（2026-09-23）

### 设计（`mo-search::content`）
- `search_content(root, &ContentQuery)`：`spawn_blocking` 递归扫子树。
  - 二进制嗅探跳过（`NUL` 或已知魔数），上限 `DEFAULT_MAX_FILE_BYTES` 跳大文件。
  - 四个开关：**不区分大小写（默认）/ 正则 / 整词 / 上下文行（1 行）**。
  - 命中行返回 `LineHit { line_no, text, spans:[(start,end)] }`——span 直接给前端高亮，前端零解析。
  - 结果上限 `MAX_HITS`，到顶置 `truncated`；报告带 `scanned/files_matched/hits/skipped_binary`。
- 自写两个轻量匹配器避免重依赖：`contains_ci`（大小写不敏感子串）、`glob_match`（glob 通配，给未来的 include/exclude 用）。

### 为什么「输入即搜」不做
真要读每个文件字节，每敲一字扫一遍整棵子树是把 IO 换回显、会把机器占死。
改为**只在回车跑**；一轮在跑时再回车先取消旧的再起新的（`content_stop` AtomicBool）。

### UI（`Modal::ContentSearch`）
- 范围**定在打开那一刻的当前目录**，不随导航漂移（否则点开结果一跳就把已搜结果作废）。
- 四个开关胶囊（`mo-content-opt-{i}`，`⌥+首字母` 切换）+ 搜索按钮（`mo-content-go`，
  跑着时置灰）+ 命中列表（`mo-content-row-{i}`，高亮 span + 行号，点击跳进文件并选中）。
- 依赖：给 `mo-search` 提了 `regex`（已在传递依赖树里，不新增下载）。

### 键位
- `search.content` = `cmd+shift+f`（`search.global` = `cmd+f` 的对称：「当前目录内 grep」）。
- `CommandId::ContentSearch`；模态内 `↑↓` 选、`enter` 重搜/跳转（靠 `content_dirty` 区分）、`esc` 关闭。

### ⚠️ `gpui_kit::*` 遮蔽 `std::path::Path`
`use gpui_kit::*` 导出同名场景 `Path`，裸 `&Path` 签名会解析错、报「missing generics」很迷惑。
凡路径签名一律写 `std::path::Path`。

### 单测
`mo-search` 19 个（含 finds_the_lines / skips_binary / regex / glob / context 等）+ 布局测试
`content_search_panel_renders_its_skeleton`。

## 操作历史面板（2026-09-29，b8052ae）

* **现象**：`AppState::history_snapshot()`（环形 200 条：复制 / 移动 / 删除 /
  重命名 + 落点目录）从一开始就记，mo-ui 里却**一个消费者都没有**——数据躺在
  内存里没人看。命令面板加「操作历史…」开出列表，点一条跳到那次操作的落点。
* **跨标签页汇总**：`AppState` 是**一页一个**（`new_tab` 就 `AppState::new()`），
  历史也就记在各自那一份里——只看当前页会漏掉别的页做过的事（复制在 A 页、人
  在 B 页翻历史）。归并收口成纯函数 `merge_history`（稳定排序按 `at` 倒序、
  截断到 `HISTORY_ROWS`），好单测。
* **跳转走哪个后端由记账时写下的 `remote` 决定**：面板里的条目可能来自任何一
  页，此刻「在看远程吗」答不对这个问题（`goes_through_remote` 只答得出一页内
  的情况）。远程落点 `open_directory`、本机落点 `open_local`；失败给一句话
  （远程会话可能已经断了）。
* **`dest` 一律记目录**：重命名记**新名字所在目录**而不是新名字那条路径——
  否则跳过去会拿一个文件路径当目录打开。落点算法收口在
  `HistoryEntry::landing_dir()`（有 dest 用它，否则退回第一个源所在目录）。
* 面板带「清空」：`AppState::clear_history()`，所有标签页一起清（只清当前页会
  留下「面板上还有一半」的假象）。

## 会话恢复（2026-09-29，ab48b46）

* **现象**：重开 Mo 一律回到 Home，上次开的标签页（含远程连接）全丢。
* **存哪**：`session.json`，与 `config.json` **同目录但不同文件**。为什么不进
  config：那是给人手改的设置，而会话是机器态、每次导航都变，两边共用一份文件
  会互相抹掉（都是「读整份 → 改一个字段 → 写回」）。
* **什么时候写**：`sync_panel` 是「任一标签页状态变了」的唯一漏斗，在那儿标脏、
  去抖 1 秒（`SESSION_SAVE_DEBOUNCE_MS`）——传输进度 150ms 一拍，不去抖就是每秒
  好几次无谓写盘；只到退出才写则崩了全丢，所以关窗（`window.on_window_should_close`）
  与 `cx.quit()` 那两条路（关最后一个标签页、⌘Q）也各存一次。
* **远程标签页**（用户选的语义）：后台重连，连上回到当时那个目录；连不上 / 要
  输密码的留成空标签页，最后**汇总**弹一条提示（`Arc<AtomicUsize>` 计数，最后
  一个跑完的负责提示）。启动不弹登录框——那会一次弹出好几个，且没人看着。
* ⚠️ **顺序坑**：`restore_session` 必须在默认那一页的 `tab_loop` **起来之前**
  跑。那个循环把首页的 `AppState` 同步进 `panes[0].tabs[0]`，而恢复后那一格已经
  是别的 `AppState` 了（两个 app 往同一格写，界面来回跳）。所以 `RootView::new`
  先看有没有会话，有就恢复、没有才走 Home 起步。
* ⚠️ **测试隔离**：`isolate_user_dirs_for_tests` 里要先删 `session.json`——pid
  复用会让两次测试撞进同一个目录，上次写下的会话会被这次的 `RootView::new`
  读走。另外同进程**只开一个窗口**（两个窗口会互相覆盖种子）。
