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

## 功能补缺一批（2026-10-08，①索引排除 ②语法高亮 ③Markdown 渲染 ④7z 创建 ⑥剪贴板历史 ⑦搜索基准）

一次把能力面上剩下的几处「明显的空」补齐。⑤（最近连接 / 服务器收藏）核对后发现
**早已落地**：`mo_config::SavedServer` + `Config::remote_servers` + 连接弹窗列表
（`connect_dialog_lists_remembered_servers`），不是缺口，本轮不动。

### ① 索引排除规则（可配 glob）
* **根因**：爬取的既有判据只有「隐藏条目不进索引」（`crawl.rs`），`.git` 跳了，但
  `node_modules` / `target` / `dist` / `build` 这些**不点开头**的目录照样整棵收进来
  ——项目根里它们能占九成条目，索引体积与爬取时间全耗在没人会搜的依赖上。
* **修法**：`mo-config` 加 `Config::index_exclude`（默认 `node_modules`/`target`/
  `dist`/`build`/`__pycache__`/`.venv`/`DerivedData`，**写进配置文件**好让用户看得见
  能改）；`mo_search::crawl` 加 `exclude` 参数，命中即整棵子树跳过（连目录名自己也不
  进，否则「搜得到目录、搜不到里面」更费解）；返回 `CrawlStats { indexed, excluded }`，
  `AppEvent::IndexUpdated` 带出 `excluded`，状态栏「已索引 N · 排除 M」（0 不显示）。
* ⚠️ 语义是**搜不到**不是**看不到**：列表照旧显示这些目录。
* 判据复用 `content::name_matches` 的 glob，不另起一套。匹配按**名字**不按全路径。

### ② 预览语法高亮（`mo-preview::highlight`）
* `PreviewKind::Code` 的注释原本就写着「本阶段仍是纯文本」——现在不是了。
* **手写扫描器**，不引 `syntect`/`tree-sitter`：那两家带几 MB 语法表与构建脚本，为
  一个「瞄一眼」的浮窗不值当。只认五类记号（注释 / 字符串 / 数字 / 关键字 / 标点），
  关键字表是几门常见语言的并集；认不出的一律 `Plain`——**认错比不认更糟**。
* 上限 128KB / 300 行（300 是因为渲染侧一行一个容器、一段记号一个元素，再往上光建
  元素就顿）。块注释跨行靠 `in_block` 在行间传状态。
* ⚠️ 踩坑：`str::find` 给的是**字节**偏移，而扫描下标走的是字符——中文注释里直接加
  会把下标推错（`highlight.rs` 已注）。

### ③ Markdown 预览渲染（`mo-preview::markdown`）
* 此前 `.md` 显示的是**源码**（`#`、`**`、反引号原样糊在屏幕上）。
* 认块级（标题 1~6 / 围栏代码 / 引用 / 有序无序列表 / 分隔线）与行内（行内代码 /
  链接 / 加粗）；**不认** `*斜体*`：`*` 同时是列表标记，误判代价大于收益。未闭合的
  记号按纯文本原样显示，不吞掉后面整行。
* 输出与 ② 同一套 `Token`（按行分组），渲染侧只多几个取色分支。

### ④ 归档创建补 7z / tar.bz2 / tar.xz
* 解压早就认 7z / rar，创建只有 zip / tar / tar.gz——写 `x.7z` 拿到的是 zip 内容。
* 加 `external_create_format()`（与 `external_extract_format` 成对，**仍然坚持「内置
  判据 / 外部判据」两条不合并**）；`create_archive` 先判外部再走内置。
* ⚠️ 打包时 `current_dir` 切到源所在目录、只传文件名：`7z a` / `tar -c` 会把传进去的
  路径**原样存进归档**，传绝对路径解开会多套一整串目录。
* ⚠️ 退出码 `127`（命令不存在，转发 shim 的真身缺席）要**继续试下一个候选**二进制，
  而不是当成打包失败。

### ⑥ 剪贴板历史
* 与暂存区的分工写进 `ClipHistoryEntry` 的文档：**暂存区 = 主动凑一批（可增删），
  剪贴板历史 = 被动留最近 20 条（不可编辑）**。
* 只记 Mo 自己的复制 / 剪切；采纳**系统**剪贴板不进历史（会随每次前台切换反复触发，
  几秒占满 20 条）。连着复制同一批只留一条（否则「刚才那批」被挤到第二行）。
* 粘贴走 `paste_history_entry` → **复用 `paste_clipboard` 那条路**（源端点、剪切一次性
  消耗、冲突 / 续传确认、可逆项与 ⌘Z 全在上面），不另写一遍。
* 面板：↑↓ 选、**Enter 粘**（双击也可；单击只移光标——粘贴会真动文件，单击就动手太
  容易误触）。「清空」只清历史，**不动**当前剪贴板里那批。

### ⑦ `cargo bench --bench search`（新增）
* 原有 `directory` / `cache` / `thumbnail` 三个基准，缺搜索——FTS5 那次优化此前只有
  一个手测的 ~150ms，没有长期回归哨兵。
* 实测（20 万条内存索引，`--sample-size 10`）：
  | 查询 | 20k | 200k |
  |---|---|---|
  | 中缀 `o-0001`（trigram 主场） | 210 µs | **511 µs** |
  | 前缀 `photo-000`（优化前基线） | 218 µs | 2.55 ms |
  | 单字符 `7`（LIKE 回退） | — | **12.7 ms** |
  | 批量写 `upsert_batch` | 10k=105 ms | — |
* 两个值得记的现象：**前缀比中缀慢**（`photo-000` 命中量大，成本按命中行数线性增长，
  索引查找本身不是瓶颈；已于 ⑧ 优化）；**单字符回退 12.7ms**（全表扫，符合预期，
  已随 §46 后台化，不压主线程）。

### ⑧ 搜索前缀查询优化（2026-10-09）
* **现象**：bench 实测前缀 `photo-000`（20 万条）**2.55ms**，中缀 `o-0001` 仅 0.52ms，差近 5×。
* **根因**：`search()` 的 `ORDER BY ... length(path)` 建在 `files_fts MATCH` 的**全部命中行**上——
  前缀 `photo-000` 命中上千行，每行做一次 rowid 查表 + 排序，成本按命中量线性增长；中缀
  `o-0001` 只命中百来行所以快。索引查找本身不是瓶颈，是「读了多少行」。
* **修法**：把 FTS MATCH + JOIN 包成子查询，内层 `LIMIT (limit+500)`（至少够外层 `LIMIT`、
  再多 500 行给相关性排序留余量，上限 2000）把候选截住，外层再排序取前 `limit`。
* **效果**：前缀 `photo-000` 从 2.55ms → **0.23ms**（≈11×），与中缀基本持平；中缀 / 单字符回退
  数字不变（单字符仍走 LIKE 全表扫，已后台化）。`cargo bench --bench search` 为回归哨兵。

### ⑨ 索引排除加设置入口（2026-10-09）
* **此前**：`Config::index_exclude` 只能手改 `config.json`（① 落地时写进了配置，但没 UI）。
* **现在**：设置窗口新增「搜索」标签页，单文本输入框编辑排除规则（逗号 / 空格分隔，实时写回
  `config.json`）。`AppState::save_config` 改为 `pub` 供 UI 复用；设置页新增 `SettingsTab::Search`
  （插在 `ALL` 末尾，不破坏「界面 / 外观 / 快捷键」的循环顺序与既有测试）。

### ⑩ 左上角「应用菜单」浮层（2026-10-09）
* **需求**：工具栏最左加一枚按钮，点开吸附在旁边的 popover，里面放设置 / 扩展程序 / 命令面板
  等全局入口（Chrome 风格的应用菜单）。
* **布局归属**：按钮在 `toolbar::render` 最左（28×28，汉堡图标）；浮层本体**不挂在按钮里**，
  由 `app.rs` 的 render 在根容器**末尾**以窗口坐标挂上（`toolbar::APP_MENU_X/Y` = 8 / 98，
  对准工具栏底缘）——绝对定位画在最上层、命中链最前，与右键菜单 / 橡皮筋同套路（gpui 无
  z-index，浮层挂在前面会被正文盖住）。视觉复用 context_menu 常量（MENU_W / ITEM_H / 圆角 /
  surface 底 + 阴影），三行：命令面板（⇧⌘P）/ 扩展程序 / 设置（⌘,）。
* **开合语义（关键坑）**：浮层的 `on_mouse_down_out` 在「点按钮」那一下也会触发（按钮在浮层
  外），若按钮用朴素 toggle，按下（关）+ 抬起（开）互相抵消 →「怎么点都关不掉」。修法是
  按钮两段式：按下（`note_menu_press`）记下「按下时是否开着」并立即收起；抬起（`app_menu_clicked`）
  只在「按下时是关着的」才打开。Esc / 点外面收起复用既有路由（`close_menu_popover`）。
* **测试**：`app_menu_popover_anchors_below_the_toolbar_without_disturbing_layout`（锚点 / 宽度 /
  三行都画出 / 不挤动布局）、`app_menu_button_press_close_click_open_do_not_cancel_out`（两段语义）、
  `app_menu_rows_open_their_targets`（点行开对门：设置行 → `Modal::Settings`、面板行 →
  `Modal::CommandPalette`，且浮层先收起）。行 id 用动作判别值（`AppMenuAction as usize`），
  与展示顺序解耦——测试里拿展示序 position 去点会点错行（踩过一次，断言带出实际 modal 才定位）。

### ⑪ 修 provider 握手对 JS 系插件必然超时（2026-10-09）
* **现象**：srt-tools（bun 实现）在「最近字幕」面板恒报「调用超时（进程已被强制结束）」，
  host.log 一片空白；换终端起 Mo（排除 PATH 问题）依旧。
* **根因**：握手帧 id 用 `u64::MAX`。JS 系 provider 的 number 是 double，超过 2^53 的
  整数被舍入——回包 id 变成 18446744073709552000，宿主 `roundtrip` 按「id 对不上」
  把正确回包丢掉继续等，直到超时 kill。插件本身 38ms 就答了（独立探针实测）。
* **排查法**：mo-app 集成探针（真实 `Host` 直打已装扩展）稳定复现超时；外部 bun 直拉
  稳定通过 → 收敛到宿主代码路径；再让夹具回显 id 才现形（浮点舍入不留任何日志）。
* **修法**：握手 id 起点改为 `JS_SAFE_ID`（2^53-1，`Number.MAX_SAFE_INTEGER`），
  「与调用帧不撞车」的初衷不变（调用从 0 递增，实际时间尺度够不到 9×10^15）。
  协议夹具加 `js_id` 模式（应答 id 过一遍 f64），`handshake_survives_js_number_rounding`
  回归钉死；`plugin-system.md` §5 协议段同步改。
* **教训**：跨语言协议里「宿主自留的哨兵值」不能假设对端能无损表示——u64 空间里
  超过 2^53 的任何值对 JS 系实现都是另一个数。

### ⑫ 扩展管理器改版：主色安装按钮 + Chrome 式宫格（2026-10-09）
* **需求**：①「从磁盘安装 / 从 .moext 安装」从灰行改成有色按钮；②已装扩展从「行+展开」
  改成 Chrome 扩展管理同款宫格卡。
* **做法**：按钮置顶一行、`selected_bg`+`selected_text` 主色底；卡片 basis 300 + grow 随宽
  换行（560 宽窗一行一张），选中卡亮 accent 边框、贡献清单摊进卡内；启用态胶囊改主色底
  （扫一眼知道哪家在生效）。元素 ID（ext-row/toggle/detail/uninstall）与点击语义一字未动
  ——既有 7 条扩展测试就是这次的回归网。
* **踩坑（重要）**：宫格让内容变高，6+ 家扩展超出 560×520 窗口——headless 实测视口外的卡
  `click` 报 not visible / 部分可见时点 bounds 中心落空（p29 测试并行必红由此而来）。
  修法三件套：卡片紧凑化（p8 / gap3）+ 页内 `overflow_y_scrollbar` + 窗口加高到 640。
* **排查法**：临时探针测试种 N 家扩展逐家打印 bounds + 点击后 ext_index——注意探针自身
  的清单若是坏清单（`"menu": ["context"]` 不是合法槽位，合法值 palette / context:file /
  context:blank / sidebar）会进 broken 列表，让「期望下标」整体错位，别把自己的错当成
  布局的错。
* **环境注**：全量 CI 期间 `layout.rs::clicking_blank_below_the_list_clears_the_selection`
  稳定红，经 stash 基线 + 上午全绿提交的 worktree 双对照确认与本改动无关——headless
  **drag 派发**在该时段系统性失效（扩展页的 click 派发同窗全绿），属环境抖动，稍后重跑。

### ⑬ 扩展卡片：图标 / 开关 / 已禁用文案 / 一键刷新（2026-10-09）
* **需求**：①卡片参考 Chrome 加插件图标，manifest 必须提供 icon，否则不允许安装；
  ②「从磁盘安装」的插件加刷新按钮，从原来源目录重装（开发阶段免反复卸装）；③启停
  改成开关样式，禁用的卡上显示「已禁用」；④安装时对必填项校验，不过不装。
* **做法**：
  - `Manifest` 新增 `icon`（相对扩展目录）。`validate` 三道门禁：非空、安全相对路径
    （绝对路径 / `..` / 反斜杠都拒）、扩展名在 ICON_EXTS（png/jpg/jpeg/webp/gif/bmp/ico，
    与 workspace image 解码 feature 同口径）；`install_from` 再验**文件真的在来源目录**
    （`validate` 看串、安装看盘）。没写 icon 的清单 → broken，报错带修法。
  - 刷新：账本 `installed.json` 的 `source` 是判据（`dev_source_of`），`reinstall(id, root)`
    顺序有讲究——来源清单先**完整过门禁**（validate + 图标在盘），过了才删旧目录，
    来源改坏就地拒、旧的分毫不动；装回来**保留原启停状态**（开发启用着，刷十次也开着）。
    AppState 包装补 `forget_ext`（provider 内存账重装后清，与卸载同一条收尾）。
  - UI：卡头改「图标 32×32（丢文件画内置拼图占位）｜名字+版本（停用加一行灰「已禁用」）
    ｜刷新（ROTATE_CW，仅带账本的卡）+ 开关 34×20」。胶囊→开关沿用原 `ext-toggle-<id>`
    （既有测试即回归网）；刷新按钮 `ext-refresh-<id>` 点了止泡。禁用卡的概要行改
    「已禁用 · 贡献 N 项」。
* **测试**：mo-app 5 条新（icon 三种坏法 / 装时图标文件缺失 / 刷新保留启用 / 来源坏或
  手摆拒刷）；mo-ui 1 条新 headless（`refreshing_reinstalls_from_the_source_and_keeps_enabled`：
  装的卡有刷新、手摆的没有，改来源→点刷新→面板当场新名字、仍停用）。既有 fixture 全部
  补 `"icon": "icon.png"`（安装类还要真落一个文件）。
* **坑**：`Div` 的 `.when` 不在 gpui_kit 根，要 `use gpui_kit::prelude::FluentBuilder as _`
  （progress_panel.rs 早有先例）。

### ⑭ 无效扩展的「删除」按钮（2026-10-09）
* **需求**：扩展加载失败只显示原因、删除还得自己去文件管理器端目录——不友好。要在
  扩展管理器里显示「无效」+ 原因，并给一颗删除按钮。
* **做法**：坏清单行文案「（加载失败）」改「（无效）」，标题行右侧加警示红「删除」；
  新确认卡 `Modal::ConfirmRemoveBrokenExt`——身份带**目录完整路径**而不是 id（坏清单
  可能压根解析不出来，id 无从谈起，目录是唯一稳定坐标）。落点
  `extensions::remove_broken_extension(dir)`：护栏=目录里必须有 `manifest.json` 才动手
  （与 `load_report` 收 broken 的判据同口径），失败扩展从未注册 provider，无需清缓存。
  键位/遮罩/主窗口三条路由全部与「卸载」确认卡同款接入（Esc/Enter/点遮罩）。
* **测试**：mo-app 1 条（护栏拒绝无清单目录 / 整删 / 已删再删报错）；mo-ui 1 条 headless
  （行+按钮在 → 点删除弹卡带目录路径 → 取消目录分毫不动 → 再删确认 → 目录没了、行当场
  消失、模态清空）。既有 34 条扩展测试零改动全绿。

### ⑮ 汉堡按钮挪到标签条行末 + rgba 位序修复（2026-10-09）
* **用户报**：①汉堡（应用菜单）放地址栏左边不合适，应挪到标签页那一行最后，但要兼容
  Windows（别跟最小化/最大化/关闭重叠）；②删除按钮还是看不清。
* **根因（②）不是样式是位序**：gpui 的 `rgba(u32)` 按 **RRGGBBAA** 大端解析，
  `rgba(0xd70015)` 六位被读成 `[00,d7,00,15]` = **绿色 + 8% 透明度**——实心红全画成了
  半透明浅绿。全仓 7 处同病（卸载/启用确认键、删除按钮、进度条成败色 0x248a3d 同样是
  半透明蓝）。统一补 `ff`。
* **①的做法**：按钮抽成 `toolbar::app_menu_button`，`render_top_row` 插在标签条之后、
  `drag_strip`/`window_controls` **之前**（Win 上就在窗口控制按钮左边，永不重叠）；
  macOS 上按下时 `stop_propagation`（顶栏整行挂着 attach_titlebar_drag，按下汉堡不该
  顺手拖窗口）。浮层锚点改右对齐：`app_menu_x(viewport_w) = vw - MENU_W - 8`，
  `render_app_menu` 收视口宽（render 里有 window 可取）。
* **测试**：锚点测试改右对齐期望；`titlebar_drag_filler` 断言改为「带子顶到汉堡左缘
  （≤5px 容差）+ macOS 上汉堡贴行末（≤8px）」。app_menu 三条与 layout 顶栏三条全绿。
### ⑯ 并行安装 zip 撞 staging 目录（2026-10-09）
* **现象**：`installs_from_archive` 单测偶发红——同一进程并行两条 zip 安装，同一毫秒拿到
  同一个 staging 目录（macOS `SystemTime` 纳秒 API 实为**毫秒级粒度**），flat42 顶层清单
  混进 p28 的解压根，`locate_source` 认错布局，装出来是别家的 id。
* **修法**：`unique_suffix` 改「进程内 `AtomicU64` 单调计数 × 4096 + 纳秒」，时间戳不再
  单独当唯一值用。教训：时间戳当唯一值先查时钟粒度；「install Ok 但文件缺」先怀疑同名
  staging 被两家共用。

### ⑰ 列表源面板入场点击补重试（2026-10-09）
* 满负载并行下 `list_source_panel` 的入场点击偶发整段丢失（headless 确定性调度与真线程
  竞争，同 e006581 书签点击重试的家族病），轮询 100 轮也等不来行。
* 修法：轮询到第 30 / 60 轮还没见行就补点一次——模态没开时这是唯一入场路径，模态开了
  再点也只是重开同一个面板，幂等。另把「满载下单跑红且稳定也不可信、判回归先看 uptime」
  写进长期判据。

