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
