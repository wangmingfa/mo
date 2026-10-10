# 引擎逻辑与测试方法的坑

日期：2026-09-17。

---

## 1. 两个有序 Map 拼 keys 后 `dedup_by` 失效（mo-diff 文件夹比较计数翻倍）

* **现象**：文件夹比较时同名条目被处理两次，identical 计数翻倍。
* **根因**：`ls.keys().chain(rs.keys())` 得到的序列里，两侧行的重复项**不相邻**（左侧行先全出来，右侧行再全出来），`dedup_by` 只去相邻重复，等于没去。
* **修法**：并集老实用 `BTreeSet`：`let names: BTreeSet<_> = ls.keys().chain(rs.keys()).collect();`
* **守卫**：`mo-diff` 的 `identical_trees` 等测试锁计数。

## 2. headless 布局测试方法（不依赖 GPU / 真实窗口）

* **通道**：`gpui-kit` 开 `test-support` feature → `TestAppContext::single()` + `cx.open_window(...)` + `VisualTestContext::from_window` + `window.render_frame(cx)`。
* **探针**：元素挂 `.debug_selector(|| "name".into())`（release no-op），测试里 `cx.debug_bounds("name")` 拿**真实布局矩形**。
* **可断言的东西**：区域几何关系（上下左右贴合、高度恒等、右缘对齐）、元素存在性。
* **量不到的东西**：AppKit 画的内容（如红绿灯）——它们不参与 GPUI 布局，只能靠常量推导 + 实机验收。
* **推荐实践**：布局回归（如「地址栏必须在工具栏内」「大小列贴行右缘」）都写成探针测试；改样式前先跑一遍留下基线。

## 3. 像素级验收（截图测量）的坑

* **教训**：在沙箱/自动化环境里靠 `screencapture` + Pillow 测窗口位置不可靠：后台起的 GUI 进程可能静默死、窗口可能在另一块屏或另一个 Space、截屏色彩配置文件会让颜色阈值偏移。
* **结论**：**优先读框架源码拿公式**（如红绿灯定位公式），实测只作为最后的验证手段，且要在用户前台会话里做。

## 4. 零依赖引擎的分层收益

* **实践**：mo-diff / mo-core 等纯逻辑 crate 不依赖 GPUI / tokio，测试秒级跑完且可在 CI 任意平台跑。UI 相关的坑全部隔离在 mo-ui 的探针测试里。

## 5. 缓存文件名用 `FileId` 的 `Display`，Windows 上整棵缩略图缓存写不下去

* **现象**：`cargo test -p mo-thumbnails` 在 Windows 上 5 条红，全是
  `Io("参数错误。 (os error 87)")`；应用里表现为网格 / 画廊永远只有图标、没有图片
  预览，快速预览的降采样副本也静默不生成（那条路径失败返回 `None`，所以不报错，
  只是「优化没生效」，更难发现）。
* **根因**：`ThumbnailCache::cached_path` 拼的是 `format!("{}.png", id)`，而
  `FileId` 的 `Display` 是 `{volume}:{id}`——**冒号在 Windows 是文件名非法字符**
  （NTFS 用它表示盘符与备用数据流），`CreateFile` 直接回 `ERROR_INVALID_PARAMETER`
  （87）。macOS / Linux 上冒号合法，所以这段代码在开发机之外从没被测过。
* **修法**：给 `FileId` 加 `cache_key()`（`{volume}-{id}`，纯数字加一个分隔符，
  两段都是数字不会歧义），缓存文件名只用它；`Display` 保留冒号形态给日志与 UI。
  回归断言放在两处：`mo-core::file_id::tests` 钉「键里没有 Windows 非法字符、
  不同 ID 不撞键」，`mo-thumbnails/tests` 钉「`cached_path` 的文件名合法」。
* **教训**：凡是**把 ID 拼进文件路径**的地方，`Display` 形态就是可移植性边界——
  日志里好看的分隔符（`:`、`#`）未必能进文件名。Windows 文件名非法集是
  `< > : " / \ | ? *`，另外还不能以点或空格结尾、不能有控制字符。
  另一头：改键之后旧缓存（`volume:id.png`）在 macOS / Linux 上成了孤儿文件，
  缩略图可再生、不用迁移，用户清一次缓存目录即可。

## 6. 要看条目内容的布局断言，别依赖 headless 里的真实 Home 目录

* **现象**：`tests/layout.rs` 偶尔挂一条，panic 落在
  `gpui-pre-scheduler/src/test_scheduler.rs:193`（`end_test` 里的
  `non_determinism_error`），不是断言本身失败；机器越忙越容易撞。
* **根因**：`open_app` 走的是 `RootView::new` + `run_until_parked()`，而 Home 目录
  的加载是**真 tokio runtime 上的后台任务**——它什么时候落地、这一帧里到底有没有
  条目，取决于真实时钟与负载。gpui 的测试调度器正是为此设了非确定性检测。
* **结论**：凡是「要看条目长什么样」的布局断言（留白、居中、行高），写在 in-crate
  的 `mo-ui::app::tests` 里，自己往 `Panel.window` / `visible_count` 塞假快照
  （见那里的 `seed_window`）——范围已被 `Panel::covered` 命中，`ensure_window` 不会
  派生取窗任务把快照冲掉。`tests/layout.rs` 只留不依赖条目内容的结构断言（谁在谁
  左边、吃没吃满高度）。
* **反面**：先写过一版放在 `tests/layout.rs` 里、目录空就 `return` 跳过的留白断言——
  在这台机器上它就走了跳过分支，绿得毫无意义。跳过式断言等于没有断言。

## 7. 三个 crate 各写一份扩展名表，同一个文件在两处自相矛盾（2026-09-27）

* **现象**（不是崩溃，是显示打架）：`.svg` 在「按类型分组」里躺在图片组，双击预览却是源码；
  `.avif` 反过来——预览当图片，分组落 `Other`。用户看到的是「列表叫它图片、点开是代码」。
* **根因**：同一个问题「这个后缀是什么」被回答了三次，三份表各长各的，加后缀的人只改自己
  那一处：`mo-core/src/view.rs` 的 `kind_group_of`（分组）、`mo-preview` 的
  `is_image`/`is_pdf`/`kind_by_ext`（预览）、`mo-app/src/icon.rs` 的
  `is_package_ext`/`is_per_file_ext`（图标能不能按类型共享）。
* **修法**：收进 `mo-core::types`（分组 / 预览 / 图标三个函数，一张表；这是插件系统 P1 的
  第一块地基，见 [plugin-system.md](plugin-system.md) §4.1）。**要求逐条行为不变**，
  所以等价性不靠肉眼：脚本把 `git show HEAD:` 的旧字面量按「匹配顺序 → 目标枚举」解析成集合，
  与新 const 数组逐条比（八张全等），再改调用点。
* **⚠️ 没有顺手把 svg / avif「统一」掉**：那是**改判据**，不是重构。`.svg` 预览确实喂不进
  `img()`（没有光栅器），退化成看源码比「一张破图」有用；`.avif` 分组没跟上纯属遗漏，
  但动它要连带动分组顺序表。两处现状由
  `types::tests::svg_and_avif_are_the_known_cross_axis_disagreements` 钉住——谁顺手统一，
  测试就红，逼他去看模块头那段注释再决定。
* **教训**：合并多份表时**别把矛盾一起抹平**。三问本就是三个不同的问题，收口要保证的是
  「答案出自同一处、冲突看得见」，不是「强行三问一致」。同理，收口后 `view.rs` 里那层
  只做转发的 `kind_group_of` 直接删掉——留着一个空壳函数，下一个读代码的人会以为它有自己的逻辑。
* **收口之后又翻出四处同类表，只有一处是真该并进来的**（P1-1 当天查完，都还没动）：
    * `mo-core/src/entry.rs` 的 `supports_thumbnail`（八个后缀）——这一处**问的就是解码器
      能不能解**，而真正的解码闸在 `mo-thumbnails/src/lib.rs` 的 `generate_to_with`：
      `matches!` 只放 `jpeg/png/gif/webp/bmp/ico` 六种，workspace 的 `image` 也确实是
      `default-features = false` 只开这六种（根 `Cargo.toml`）。于是 `.tif` 是个**假阳性**：
      任务照派、解码器回 `Unsupported`、列表仍旧显示通用图标。用户看不出问题，代价是一次
      白跑的取图任务。要修的是「两处引用同一份解码集合」，不是「把 tiff 删掉」。
    * `mo-ui/src/dialogs.rs`（类型色块）、`file_item.rs`（类型标签）、`icons.rs`（图标 glyph）
      三处**不该并进 `types::group_of`**：它们把视频和音频分开（三种颜色、三种标签、两张图），
      而分组只问出一个 `GroupKey::Media`——问的比 `group_of` 细一级，是另一张表而不是副本。
      写这篇的时候差点顺手把它们也「统一」掉；统一的结果是所有 `.mp4` 与 `.mp3` 变成同一格。

## 8. 反向验证「还原后仍红」：`mv` 保住了旧 mtime，cargo 复用了变异体的二进制（2026-09-27）

* **现象**：P1-2 反向验证——把 `crates/mo-ui/src/actions.rs` 挪走、塞进一份改坏的（默认槽位
  多加 `ContextFile`），跑测试如预期红一条；再把原文件 `mv` 回来重跑，**还是红的**。
  差点得出「这条测试根本没断言到东西」的结论。
* **根因**：同盘 `mv` 是 rename，保留**源文件的时间戳**。挪出去再挪回来，文件的 mtime 比那
  份改坏的还**旧**，cargo 的指纹比较认为源码没变，直接复用上一次的编译产物——跑的是变异体
  编出来的测试二进制。
* **修法**：还原后 `touch` 一下再跑。本轮真实结果：118 绿；只在变异体上红
  `actions::tests::everything_is_palette_only_for_now` 一条。
* **教训**：反向验证的可信度取决于「跑的那个二进制确实是这份源码编出来的」。凡是用**文件
  搬运**而非**编辑**切换源码版本的手法（mv / cp / 手工还原），都要显式把 mtime 推到当下。
  更省事的办法是压根不引入搬运：改坏 → 跑 → 用 Edit 改回 → 跑，一次只验一个变异体。

## 9. 只在整包并行时才红的一条：`layout.rs::trash_empty_asks_for_confirmation`（2026-09-27）

* **现象**（P2-2 那一轮，三次 `cargo test --workspace --all-features`）：第一次 101，红的
  是 `mo-ui --test layout` 的 `trash_empty_asks_for_confirmation`（34 过 1 红）；第二次也
  101，但**红的到底是哪条我没查到**——第一次的日志被我自己的 grep 截断了（见下面那条流程
  教训），第二次只留下 `EXIT=101`；第三次 `--no-fail-fast` 完整落盘 → **全绿（0）**。
  同时：单跑那一个测试绿，单跑整个 layout 二进制连跑三次绿。
* **为什么判定与 P2-2 无关**（判据写清楚，别只说「看起来不相关」）：这一轮的 diff 只碰
  ①`mo-config` 多一个数据字段、②`mo-app` 两个 `validate`、③`mo-ui/src/actions.rs` 里
  slots 的来源。回收站确认卡走的是 `sidebar` → `dialogs` → trash store，**一行都不经过
  投递层**；`contributed()` 只在开右键菜单与开命令面板时被叫到。
* **真正的可疑形状（还没修）**：`click("sidebar-trash")` 之后只有 `run_until_parked()` +
  **一次** `render_frame()`，紧接着就断言 `(2, false)`「前提：两条都在面板上」。回收站
  条目是从磁盘 `index.json` 真读回来的，而 `run_until_parked` 不等外部线程——与
  [windows-port.md §27](windows-port.md) 那次同一类。机器忙（整包并行、33 个二进制抢
  CPU）时窗口就窄，所以只在整包时复现。
  **修法**：这类「前提断言」前面接轮询（`panel_window_ready_for_tests` / `wait_for_panel_rows`，
  行数**精确**相等），别在每个测试里各摆一次 `render_frame`。别用 sleep 糊。
* **顺记一条流程教训**：`out=$(cargo test … 2>&1); echo "$out" | grep -E "^test result|^error" | head -40`
  会把 `---- xxx stdout ----` 后面的断言文本整段丢掉，于是「红是哪条、为什么红」都不知道，
  只凭一个印象就差点把「全量绿」报出去。判绿的正确姿势：**完整日志落盘**
  （`cargo test … > /tmp/ws.log 2>&1; echo EXIT=$?`），再看 `EXIT`，再按名字查断言。
  与 §7 那条「管道会吞掉退出码」是一对：一个吞的是码，一个吞的是原因。
* **已修**（同一轮）：`layout.rs` 加 `wait_for_trash_state()`（100 轮 `run_until_parked` +
  `render_frame`，等到想要的 `(条目数, 确认卡开着)` 才返回，返回最后看到的值交给调用方断言，
  失败时报得出实际看到什么），该测试**每一次**状态断言前都走它。改完
  `cargo test --workspace --all-features --no-fail-fast` 连跑两次都是 0
  （单二进制连跑三次也全绿，35 条）。这是**测试侧**的改动，产品代码一行没动。

## 10. 「打在渲染链路上」的一句话，藏了一条空壳测试（2026-09-27，P2-3）

新加的那条 headless 断言最初写成：种一个 fixture 扩展 → 导航到一个真有 `.srt` 的目录 →
调 `mo_ui::panel_kind_labels_for_tests(root)` 读回「种类」列的文案。听起来是在断言渲染，
实际上那个访问器自己调了 `AppState::type_labels()` + `file_item::kind_label()` **重算一遍**。

反向验证一跑就露馅：把 `file_list::render` 传给 `view()` 的那份表换成空的（产品行为从此
是「种类列永远显示内置答案」），测试**照绿**。断言与产品代码之间根本没有共用那条链。

* **判据**：一条断言渲染的测试，必须能被「只断渲染那一头」的变异体打死。接线类的改动
  （A 把数据交给 B，B 画出来）至少各变异一次 A→B 的传递，红才算数。
* **修法（这一轮采用的）**：让被断言的那个元素把文案带进它的 debug 选择器
  （`mo-kind-cell-字幕`），测试查 `vcx.debug_bounds("mo-kind-cell-字幕").is_some()`。
  `rendered_frame.debug_bounds` 是一张「选择器 → 矩形」的表，**由 paint 阶段登记**，
  所以命中就等价于「这句话真的被画出来了」。选择器构造放在 `debug_selector` 的闭包里，
  release 不登记选择器，那次 `format!` 也就不发生。
  代价：该列没有稳定的选择器可抓（文案每行不同），所以只对需要断言文案的列这么做，
  日期/大小两列保持固定选择器。
* **删掉那个访问器**，别留着——它会诱使下一个人再写一条空壳断言。
* 顺带：`mo_core::types` 那条「判据一律小写、不含点」的规矩，用第二个变异体验过
  （把查表的钥匙漂成 `to_uppercase()` → `contributed_type_label_wins_over_builtin` 红，
  报 `left: "SRT 文件" right: "字幕"`）。凡是「两边各折一次大小写」的地方都值得这么打一发。

## 11. 面板刚开那一拍的按键，在 headless 下偶发不生效（2026-09-28）

`app::tests::palette_selection_follows_keyboard_scrolling` 只在**整包并行**时红（单跑约
20%，全量约 50%），报 `left: 63 right: 64`——按了 `n-1` 次 ↓，高亮行停在倒数第二行。

### 定位过程（每一步都排掉了一个想当然的猜测）

* **先确认不是本次改动引入的**：`git worktree add /tmp/x HEAD` 在**基线**上跑同一条，
  5 次红 3 次。这一步每次都值得先做——否则会拿自己的改动当嫌疑人查半天。
* 探针打在「面板 ↓」分支里，记录每次进入时的 `(palette_index, palette_len)`：
  失败样本是 `[(0,65), (0,65), (1,65), ...]` ——**第一次按键进了分支、行数也对
  （65 > 0）、闭包里 `+1` 也执行了**（赋值后再读是 1，`scroll` 后仍是 1），
  但下一次按键时它又成了 0。整段慢一拍。
* 于是怀疑「有人写回 0」：给全部 43 处 `palette_index = 0;` 打上「仅当从非 0 变 0 时
  记一条行号」的标记——**一条都没记**。开面板那条路径也只命中一次，不是重入。
* 又怀疑「首帧未绘制」：按键前先 `window.painted_quads()` 预热——无效。
* 实体 id 全程只有一个（`EntityId(1v1)`），不是两个视图各按各的。
* 成功的样本里 `n` 是 65、失败的样本里是 66（`user_commands` 按当次选中项过滤，
  列表异步加载中），但**两个方向都出现过失败**，不是同一个原因。

### 结论与处理

写入丢失发生在 `entity.update` 之后、下一次事件之前，发生在 gpui 那一层而不是 Mo 的
逻辑里（Mo 侧没有任何一处把 `palette_index` 写回 0）。**真机不受影响**：人不可能在面板
开出的同一帧里按下 ↓。所以测试侧先空按一次把这一拍消化掉，不断言它：

```rust
cx.simulate_keystrokes("down");   // 消化「刚开那一拍」，两种情况最终都停在最后一行
for _ in 0..n - 1 { cx.simulate_keystrokes("down"); }
```

按 `n-1` 次必须停在 `n-1` 的断言强度没变（预热那次若生效，末尾照样 clamp 到最后一行），
验证：改动前全量 5 次里 3 次红，改动后 6 次全绿。

### 两条方法论

* **探针要用内存记录，不要用 `eprintln!`**：加了打印之后这条**再没复现过**（IO 改变了
  时序）。改成往 `static Mutex<Vec<..>>` 里 push，测试末尾再 dump，开销小到不影响时序。
  ⚠️ 配合 libtest：**只有失败的测试才会把捕获的输出打出来**，所以 dump 要写在断言之前。
* **先建基线 worktree 再查**：这条测试是在别的功能开发中撞到的，花在「是不是我引入的」
  上的怀疑成本，一次 worktree 就能清零。

## 12. 进程级 env 的另一半：设了 `MO_CACHE_DIR` 也照样互相踩（2026-09-28，P3）

`provider_host.rs::manager_end_to_end_classifies_stores_and_forgets` 概率性挂在
`manager.cache().expect("缓存应能打开")`——只在 `cargo test -p mo-app`（多个测试二进制
并行）时红，单跑 `provider_host` 永远绿。

这条用例其实**自己设了** `MO_CACHE_DIR`（指到自己那个 tmp 目录），看着是隔离的。但：

* `MO_CACHE_DIR` 是**进程级**环境变量，而 `provider_host` 一个二进制里有 9 个用例并行跑，
  各自 `set_var` 到各自的 tmp——谁最后设谁生效，别的用例打开的就是别人的库；
* `spec_dir()` 开头还有一句 `remove_dir_all`：A 正在用的缓存目录可能被 B 连锅端掉，
  于是「开库」真的会失败。

所以「设了隔离变量」不等于隔离：变量是进程共享的，而**磁盘目录是各自删的**。修法是
把整条用例包进 `common::isolated`（它用 `env_lock` 把「设变量 + 跑用例」串起来），
而不是只在用例里 `set_var`。判据同 §6：凡是要落盘的用例，隔离必须包住**整个用例**。

（顺带排除掉的错误猜测：给 sqlite 加 `busy_timeout` 并不治这条——而且 `MetadataCache`
的打开在热路径上，等锁会有卡主线程的风险，别顺手加。）

## 测试隔离目录的 TTL 清扫（2026-09-29，a2581f4）

* **现象**：TEMP 里 `mo-test-config-*` / `mo-test-cache-*` 堆到 400+ 个——测试
  进程被 kill / panic / 断电时没机会自清，而 Rust 测试二进制没有可靠的退出钩子
  （libtest 直接 `exit`，析构不保证跑）。
* **修法**：放弃「退出时清」，改成**下一次测试跑起来时扫**——
  `isolate_user_dirs_for_tests` 首次建目录时顺手 `sweep_stale_isolated_dirs`：
  只认自己的两个前缀、只删修改时间早于 **24h** 的（活着的并行测试进程的目录
  都是新鲜的，碰不到；读不出 mtime 的一律当新鲜，宁留勿误删）。
* **两个坑（首跑 CI 全抓到）**：
  1. sweep 测试 TTL=0 会把**本进程**正牌隔离目录（`-{pid}` 结尾）也删掉——同一
     测试二进制里并行跑的其它测试被抽地板。守卫：目录名以 `-{当前pid}` 结尾的一
     律跳过（测试里钉住）。
  2. `entry.file_name().to_str()` 链在 let-else 里：OsString 临时值活不过本条
     语句（E0716），先 `let raw_name = entry.file_name();` 绑出来。
* **守卫**：`isolate_sweep_tests::sweep_removes_only_mo_test_prefixed_dirs`——
  TTL=0 下我们的前缀必删、前缀不匹配不碰、本进程目录不碰。
* 残留的存量目录不用手动清：24h 前的下一次测试跑起来就扫掉了。

* **补记（同日 69655e6）**：清扫下沉成 `mo_fs::sweep_stale_temp_dirs(prefixes, ttl)` 共用——mo-app 的 `mo-app-store-*` / `mo-trash-*` 也是大户（一次全量留 400+），由 `tests/common::store()` 进程级一次性扫；mo-ui 的实现删除、委托过来。行为钉子也跟着搬进 mo-fs。

## 13. 等行轮询在整包并行下仍假红：`run_until_parked` 会瞬间空转，必须让出 CPU 给真 IO（2026-09-30）

* **现象**：全量 CI 里 `layout.rs` 又红两条（`contributed_type_label_shows_in_the_kind_column`
  与 `clicking_blank_below_the_list_clears_the_selection`），都是「导航后 N 行没画出来」。
  单跑 12 次全绿——只在整包并行（几十个二进制抢 CPU）时复现。
* **根因**：§9 那轮加的轮询（100 轮 `run_until_parked` + `render_frame`）在并行负载下
  **毫秒级空转完**——`run_until_parked` 只排 GPUI 自己的任务队列，不推进真实时钟也不等
  `spawn_blocking` 线程；机器忙时那条真读盘的 worker 被线程池饿着，100 轮烧完它还没回来。
  于是「轮询」实际没给 IO 任何时间窗。
* **修法**（`wait_for_panel_rows` / `wait_for_trash_state` 同步升级）：
  1. 每轮结构 `run_until_parked` → `render_frame` → **再** `run_until_parked`（第 2 次
     把 `uniform_list` 派生的补窗任务 drain 掉，否则撞「`list_count` 已就位、`window`
     还空」的中间态）；
  2. 没就绪就 `std::thread::sleep(5ms)` **让出 CPU**——这是关键一步，OS 先跑完被饿的
     真读盘，下一轮才查得到。别信「轮询」三个字：不 sleep 的轮询在忙机器上等于忙等；
  3. 轮数 100 → 600（≈3s 真实等待上限）。插桩实测收敛都在第 1 轮（0.3–1.9s），
     说明 sleep 换来的时间窗足够，600 只是上限保险。
* **同轮抓到的另一个信号——整包 SIGABRT**：一轮并行跑 21 条过后**整个测试进程**
  `signal: 6` 直接 abort，没有 `test result:` 行、没有任何 FAILED——这就是 §6 那个
  `end_test` 非确定性 panic 的进程级形态（机器越忙越易撞，背景真 IO 落地晚于测试结束）。
  排查注意：libtest 默认捕获输出，abort 后探针全丢，**必须 `--nocapture` 重跑**才看得到
  panic 文本；且判绿要 grep `test result: ok`（小写）而不是看退出码——abort 轮的输出里
  连 panic 都没有，先见到「没 result 行」就该怀疑进程级死亡。
* **验证**：`--nocapture --test-threads=4` 连跑 12 次全绿（36/36）；全量 CI 重跑不再出
  「N 行没画出来」。

## 11. 点击类 flake 硬化（2026-10-10）

* **根因（区别于 §9/§10 的 SIGABRT）**：`clicking_a_row_after_scrolling` /
  `grid_press_and_release_is_a_click_that_still_selects` / `new_folder_goes_into_the_selected_directory`
  这类测试，首点（press+release 同点当 click）在 headless 调度与真线程竞争下偶发整段丢失——
  事件派发了但 `run_until_parked` 那一拍没处理完，断言时选中态还是 0。**这是点击丢失，不是
  进程级 SIGABRT**（后者是 gpui 调度器清理阶段的随机 panic，落在哪条测试名都随机，测试侧无解，
  见 run-ci.sh 文件头与 §9/§10）。
* **修法（照 `list_source_panel` 范式，但加 toggle 保护）**：点击后轮询期望状态，确认仍 0
  （首次确已丢失）时才在 round 30 补点一次。**点击是 toggle——补点前必须确认仍 0**，否则
  0→1→0 反而取消选中，比不补还糟；不靠 round 60 二次补点（极延迟的首次 click 与补点重叠会
  来回 toggle，只有 `list_source_panel` 那种「开模态幂等」才敢补两次）。
* **涉及测试**：`tests/layout.rs::clicking_a_row_after_scrolling_still_selects_it`、
  `tests/grid_drag.rs::grid_press_and_release_is_a_click_that_still_selects`、
  `src/app.rs::new_folder_goes_into_the_selected_directory`。
* **边界（必须说清）**：本批只消点击丢失类 flake。SIGABRT 框架级 panic 在满载机器上仍会概率性
  出现，本地跑 `bash scripts/run-ci.sh --retry-flaky` 用整步重跑兜底；CI 上概率性红是已知的、
  与 `ci.yml` 严格一致，不靠测试侧硬编码掩盖。
* **验证**：三个目标测试 + `grid_drag` 全集（5）+ `new_folder` 系列（3）在 `--test-threads=1`
  下全绿；低负载下整包 mo-ui lib（183）亦全绿。
