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
