# 插件系统（扩展包 / 能力分层 / provider 协议）

> **状态：设计稿。P1-1（`TypeRegistry`）已落地，其余未实现。** 本篇不遵守 devlog
> 「只记已验证结论」的约定（见 [README.md](README.md)），记的是**将要做的**东西与其依据。
> 已完成的部分在正文里逐条标注（并附 devlog 条目），没标的都还是纸面上的。
>
> 日期：2026-09-27。工具链 rustc 1.98.1。作者决策：**按完整三层设计（含 provider），实现一起排**。

---

## 0. 目标与非目标

**目标**：用户能装一个插件，让它——

* 告诉 Mo「这类文件是什么」（分组、图标、显示名、预览方式）；
* 往命令面板 / 右键菜单 / 快捷键 / 侧边栏里加东西；
* 为文件出**预览数据**、为一栏列表出**条目**；
* 崩溃、卡死、写错清单都不该让 Mo 打不开或丢数据。

**非目标**（明确不做，且理由要留档）：

* **插件画自己的 UI**。Mo 今天所有界面部件是具体 enum + 硬编码 vec（`crates/mo-ui/src` 里 `dyn`
  零命中），要塞第三方控件等于先给 gpui 造一层组件运行时。
* **往 Mo 进程里加载本机代码**（`.dll` / `.dylib`）。Rust 没有稳定 ABI，理由与后果见
  [customization.md §8](customization.md)。
* **插件直接改用户文件**。要改只能贡献一条 command，由用户显式触发，走现有
  `usercmds` 执行层（占位符守卫见 customization.md §7）。

## 1. 现状：已经有地基，缺的是「插不进界面」

已有的扩展系统（`crates/mo-app/src/extensions.rs`）：一个扩展 = `<配置目录>/mo/extensions/<id>/manifest.json`，
声明 `commands` / `workflows`，带 `{dir}{file}{files}` 占位符、`when_ext` 条件生效、启停、
id 命名空间校验（`valid_id` :66、`validate` :74）、单目录坏 JSON 不连坐（`load` :118）。
UI 侧有扩展管理器（`mo-ui/src/app.rs:3426 render_extensions`）与命令面板接入
（`CommandId::User(usize)` / `Workflow(usize)` 逃生口，app.rs:315）。

**它的天花板：只能进命令面板。** 其余界面全是硬编码：

| 想挂的地方 | 今天的形状 | 位置 |
|---|---|---|
| 右键菜单 | ~~`enum MenuAction` 约 30 变体，**无** `User(_)` 逃生口~~ **已有一臂收容进来的动作**（P1-2，2026-09-27）：`MenuAction::Contributed(ActionKind)`，条目仍 `push` 出来，但贡献项走 `actions::for_slot` | `mo-ui/src/context_menu.rs`、`actions.rs` |
| 侧边栏一项 | ~~无数据模型，六段手写 div~~ **已是数据驱动**（P1-3，2026-09-27）：`sidebar::sections()` 交 `Vec<Section>`，加一项 = 多一条 `Row` | `mo-ui/src/sidebar.rs` |
| 新面板 | `enum Modal` 约 25 态 + render 手写三档 match | `app.rs:174`、`6739` |
| 类型知识 | ~~三份各自独立的扩展名字符串匹配~~ **已收成一张表**（P1-1，2026-09-27）：`mo-core/src/types.rs`，三问各自一个函数 | 旧三处见 [engine-testing.md §7](engine-testing.md) |
| 新文件协议 | `trait FileSystem`（`Arc<dyn>`，**已经是真抽象**）但协议表硬编码 | `mo-fs/src/lib.rs:34`、`mo-remote/src/lib.rs:194,202` |
| 键位 | `BINDINGS: [Binding; 40]` + `dispatch_action(match id: &str)`，配置已能改键 | `mo-ui/src/keys.rs:404`、`app.rs:4273` |

两个好消息决定了这套设计的形状：`dispatch_action` 按**字符串 id** 匹配、
`SessionRegistry` 的 `Connector` 是 `Arc<dyn Fn>`（`mo-app/src/lib.rs:404`）——
这两处是天然挂载点，不需要新发明一层命令总线。

## 2. 边界：三条钉死的规则

1. **插件只交数据，像素由 Mo 画。**（预览交结构化的文本 / 图片文件路径 / 表格行。）
2. **插件不在列目录热路径上被等待。** 这条不是性能偏好，是既有事实：`read_dir_blocking`
   一次要拿「类型 + 隐藏位」，两万条目录多两万次 syscall 就是肉眼可见的停顿
   （customization.md §10）。插件的答案一律走缓存 + 后台泵：**先画内置结论，答案回来再局部刷新**，
   与图标泵 / 缩略图泵 / 刷新泵同一个模式。
3. **清单只从用户自己的配置目录读，绝不扫正在浏览的目录。**（否则「打开别人给的文件夹」
   就等于装了它带的扩展；customization.md §7/§8。）

## 3. 清单 schema（在现有 `Manifest` 上扩，不推翻）

```json
{ "id": "srt-tools", "name": "字幕工具", "version": "1.0.0", "min_mo": "0.1.1",
  "enabled": true, "capabilities": ["read-names"],
  "commands": [], "workflows": [],

  "types":  [ { "ext": [".srt", ".vtt"], "group": "document", "icon": "subtitle", "label": "字幕" } ],
  "menu":   [ { "action": "srt-tools.stat", "label": "统计字数",
                "where": ["context:file", "palette"], "when_ext": [".srt"] } ],
  "keybindings": [ { "action": "srt-tools.stat", "key": "cmd+shift+w" } ],
  "sidebar":  [ { "section": "字幕", "action": "srt-tools.panel", "icon": "subtitle" } ],

  "provider": { "run": ["bin/srt-tools"], "methods": ["classify", "preview"],
                "startup_timeout_ms": 2000, "call_timeout_ms": 800 } }
```

* `id` 规则、id 必须等于目录名、展示名带扩展前缀（`扩展名 · 命令名`）三条沿用现有实现。
* **动作 id 是 `<ext-id>.<name>` 字符串**，与内置命令同进一张表；内置那批继续用现有 `'static str`。
* 声明层四类（`types` / `menu` / `keybindings` / `sidebar`）都是**纯数据**，可静态校验、
  向后兼容便宜；带逻辑的一律走 `provider`。

## 4. 宿主侧三张注册表（P1 的主体，本身即重构收益）

### 4.1 `TypeRegistry`（mo-core）—— ✅ 已落地（2026-09-27，P1-1）

今天「`.png` 是什么」这件事在三个 crate 各写一份字符串匹配，会互相矛盾（详见
[engine-testing.md §7](engine-testing.md)）。收成一张表，落在 `crates/mo-core/src/types.rs`：

```rust
pub enum PreviewClass { Image, Pdf, Markdown, Json, Code, Text }
pub enum IconShare { ByType, ByPathPackage, ByPathExecutable }
pub fn group_of(ext: &str) -> GroupKey;       // 分组：未知 = Other
pub fn preview_of(ext: &str) -> PreviewClass; // 预览：未知 = Text
pub fn icon_share_of(ext: &str) -> IconShare; // 图标能否按类型共享
```

**实际形状与原设计不同，而且更好**：没有做成一条 `TypeRule`（分组 + 图标 + 预览 + 标签
捆在一起），而是**一问一个函数、各自一张 const 表**。理由是覆盖的粒度——插件只改「预览
方式」而不改「分组」是很常见的需求，捆成一条 rule 就逼作者同时回答三问，答不出来的那些
反而会被内置值顶掉。等 P2 的 `types` 清单字段进来时，注册表按
「某一问的覆盖列表」建，不再回到单一 struct。

三处调用点已改为查表：`mo-preview` 的 `class_of`/`text_kind`、`mo-core/view.rs` 分组处
（原来那层只做转发的 `kind_group_of` 删了）、`mo-app/icon.rs` 的 `is_package_ext` 与
`is_per_file_ext`（后者保留 cfg 外壳：那张表是跨平台事实清单，「快捷方式要不要按路径问」
是平台行为）。

保持不变的部分（P2 直接沿用）：判据一律是**小写、不含点**的扩展名；三个函数纯匹配、
**无 IO 无锁**（列目录热路径每条目都要问一次）；插件贡献的规则将来**先于**内置查。

⚠️ 这一步**逐条保持了现有答案**（脚本比对新旧八张集合全等，见 engine-testing §7），
刻意留下的两处跨轴矛盾（`.svg` 分组算图片 / 预览算文本；`.avif` 预览算图片 / 分组落
`Other`）由 `svg_and_avif_are_the_known_cross_axis_disagreements` 钉住。

### 4.2 `ActionRegistry` —— ✅ 已落地（2026-09-27，P1-2，`crates/mo-ui/src/actions.rs`）

原设计的形状（一条 `ActionSpec` 带 `id` / `when_ext` / `Builtin|Command|ProviderCall`）
**落地时改小了**，因为跑了一遍现状发现三件事：

1. **内置命令不搬**。`CommandId` 那 ~90 个变体各有 `run_command` 分支，**编译期穷尽**
   （少一个分支编译不过）。摊成数据表等于把编译期保证换成运行期查表，换不到好处。
   注册表只收「宿主事先不知道有几条」的那批：用户命令 / 扩展命令 / 工作流。
2. **`when_ext` 不在这一层判**。`AppState::user_commands(&exts)` 交上来的清单**已经**按
   各扩展 `when_ext` 过滤过了；这里再判一次就会出现「面板看得到、菜单看不到」这类
   对不上号的差异（与 engine-testing §7 三份表同一个病根）。判据只放一处。
3. **载荷带身份而不是下标**。落地的是 `MenuAction::Contributed(ActionKind)`，
   `ActionKind::User(usize) | Workflow(usize)` 直接装在菜单动作里——原设计的
   `Plugin(usize)` 是「注册表下标」，而注册表按当前选中项的扩展名**每次重建**，
   点开菜单到点击之间下标会飘。今天两个面（面板 / 菜单）都各留一份快照，就没有这个问题。

实际接口：

```rust
pub(crate) enum Slot { Palette, ContextFile, ContextBlank }
pub(crate) enum ActionKind { User(usize), Workflow(usize) }
pub(crate) struct ActionSpec { pub title: String, pub category: String,
                               pub kind: ActionKind, pub slots: Vec<Slot> }
pub(crate) fn contributed(users, workflows) -> Vec<ActionSpec>  // 今天一律 slots = [Palette]
pub(crate) fn for_slot(specs, slot) -> Vec<&ActionSpec>
```

三个落地点都已接上：`commands_in()` 尾部那两个 for 循环换成查表（顺序 / 分类逐条不变，
`registry_preserves_order_and_categories` 钉住）；`context_menu::items()` 多收一个
`&[ActionSpec]`，把命中本槽位的追加在**末尾并给第一条带前导分隔线**（内置那批的分组节奏
是设计过的，用户自己起的名字混进去会读散）；`run_menu_action` 那两臂直接映射到既有的
`run_user_command_at` / `run_workflow_at`，**不另开执行路径**（占位符守卫、输出回显都在那边）。

⚠️ **P2 开工前必须先补的一处接线**：`RootView::user_commands` 这份镜像目前只在
**开命令面板**时刷新（`keys` 的 `palette.open`），也就是「先开过一次面板，扩展命令才存在」，
而且它反映的是**上一次开面板时那批选中项**的 `when_ext` 过滤结果。今天看不出问题（贡献项
只投面板），但一旦有动作进右键菜单，判据就必须跟着这一次右键的目标走——
`open_context_menu` 里重取 `user_commands(&selected_ext_names(...))` / `workflows()`。
这一步现在**没做**：菜单还吃不到它，为一个不存在的消费者在每次右键时扫一遍配置目录
不划算。

P2 剩下要做的：把清单 `where` 里的槽位灌进 `slots`；要引用内置动作时给 `ActionKind`
加一臂 `Builtin(&'static str)`（`dispatch_action` 已经按字符串 id 匹配，是现成的）；
`toolbar` 槽位。侧栏那一层已经接好了（§4.4 / P1-3），P2 只欠把清单的 `sidebar` 字段
映射成一条 `Row`。

**验收**：`cargo test -p mo-ui` 118 绿（新增 5 条：注册表 3 + 菜单 2）；反向验证是把
`contributed` 的默认槽位加上 `ContextFile` → `everything_is_palette_only_for_now` 变红。
（这一轮还被 `mv` 恢复旧 mtime 骗了一次，cargo 复用了变异体的产物、绿的其实是上一个二进制，
详见 [engine-testing.md §8](engine-testing.md)。）

### 4.3 `ProviderHost`

起进程、握手、按调用超时、退避停用、结果缓存。UI 侧只在后台线程调用（沿用 mo-app 现有
的泵模式），**任何 `block_on` 都不许出现在渲染路径上**。

### 4.4 `Vec<SidebarItem>`（侧栏）—— ✅ 已落地（2026-09-27，P1-3，`crates/mo-ui/src/sidebar.rs`）

设计稿里写的是「侧栏改数据驱动，`Vec<SidebarItem>`」。落地成了**两层**，比一条 `SidebarItem`
枚举更贴合这里的现实：

```rust
// 数据层（纯函数，不碰 gpui、不读 AppState）
pub(crate) struct Sources { locations, connections, active_connection, shares, drives, bookmarks }
pub(crate) fn sections(src: &Sources, current: Option<&Path>, trash_active: bool) -> Vec<Section>
pub(crate) struct Row { id: ElementId, label, icon, active, truncate,
                        activate: Activate, trailing: Option<Trailing>, drop: Option<DropOn> }
pub(crate) enum Activate { Open { path, fallback_to_current_backend, failure },
                           Connection { id }, TrashPanel }
// 渲染层（一种行、一处样式）
pub fn render(app: &AppState, current: &Option<PathBuf>, trash_active: bool, entity) -> impl IntoElement
```

三个与草案不同的决定，都是读现状代码读出来的：

1. **输入是投影（`Sources`），不是 `&AppState`**。mo-ui 依赖 `mo-platform` 但**不**依赖
   `mo-remote`，所以根本叫不出 `NetworkShare` / `LiveConnection` 这两个名字；就算叫得出，
   数据层测试也得会构造一个合法远程 URL 才能跑。投影成「显示名 + 路径 + 协议字符串」之后
   数据层只认字符串与路径，代价是 `render` 开头那几个 `.map()`。
2. **行尾挂的是「意图」而不是回调**。草案允许塞 `Box<dyn Fn>`，这里换成 `Activate` /
   `PowerAction` / `DropOn` 三个枚举：渲染层那个 `match` 因此是**编译期穷尽**的——新增一类
   语义必须同时在渲染层补一臂，不会出现「数据有了、点了没反应」（回调版本一定会出现）。
   P2 的清单 `sidebar` 落进来时也只是多一条 `Row`、多一个 `Activate::Contributed(usize)`。
3. **元素 ID 由数据层给，选择器从 ID 派生**。`debug_selector` 写的就是 `format!("mo-{id}")`，
   于是元素 ID 与测试选择器不可能分叉；行尾按钮是行 ID 的 `ElementId::NamedChild`
   （`sidebar-loc-0` → `sidebar-loc-0-power`），**按构造**不会跨行撞车，不用再手工编号。
   既有选择器逐字符不变（`mo-sidebar-loc-0` / `mo-sidebar-trash`），老的 headless 断言不用改。

⚠️ 顺手抓回一个**会炸但平时看不见**的坑：五个区的手写标题原本各是一句字面量
`text!("快捷访问")`，五个不同调用点，天然不撞；合成一个循环后变成**一个调用点渲染四个兄弟
节点**，而 `text!` 的默认 ID 是「调用点位置的哈希」，标题外层那些 div 又都没有元素 ID
（它们不可交互）——这四个的 a11y NodeId 会全等于同一个。这就是 5562d2e 修过的那类
debug-only `0xc0000409`（屏幕朗读 / 检查器一挂上才炸）。标题两句都改成
`text!(id = format!("mo-head-{}", section.title), …)`。行内标签没这问题：外层行 div 各带唯一 ID。

**验收**：`cargo test --workspace --all-features` 全绿（`TEST_EXIT=0`，mo-ui lib 129 条，
其中 sidebar 11 条：区序 / 空区隐藏 / 三套高亮判据各一条 / 拖放落点 / 行尾按钮归属 /
分流标记归属 / 推不动的盘不画按钮且行序对齐 / 每行带自己的按钮 ID / 全侧栏 ID 查重）。
headless 侧新增 `layout.rs::clicking_a_sidebar_bookmark_opens_that_folder`——网络 / 位置 /
远程三区要真挂载点、真卷宗、真会话才出得来行，headless 造不动，拿可预置的**书签**当这四区
的代表（收口前它们的行在测试里根本选不出来）。反向验证两次各红一次，且红在该红的那句话上：
① 去掉行末的 `.test_support()` → `click` 报 `missing ElementId Name("sidebar-bm-0")`
（注意 `debug_bounds` 仍然读得到它，只有点击需要被观察，所以这一变体测的正是 `test_support`）；
② 把 `bookmark_rows` 的 `path` 换成临时目录 → 「点了书签行，那 3 个文件没画出来」变红。

## 5. provider 协议（stdio）

* **传输**：换行分隔 JSON（JSON Lines）。请求 `{id, method, params}`，响应 `{id, result | error}`。
  刻意**不用** LSP 的 `Content-Length` 头，也不用完整 JSON-RPC：这里只有「一问一答」、
  不需要并发帧、不需要通知——头部与通知只增加插件作者的出错面。
* **生命周期**：`initialize{protocol:1}` → `{name, version, methods[]}`；`methods` 必须是清单
  声明的子集，否则只信清单。退出先发 `shutdown`，500 ms 不退就 kill。
* **Windows**：子进程一律带 `CREATE_NO_WINDOW`（GUI 构建的 exe 接 stdio 会冒黑窗）。

| 方法 | 入参 | 可答 | 硬约束 |
|---|---|---|---|
| `classify` | `path`、`name`、`size`；仅在授予 `read-contents` 时附 `head_b64`（前 4 KiB） | `group` / `icon_key` / `label` / `columns[{name,value}]` | 只能回答自己 `types.ext` 命中过的路径 |
| `preview` | `path`、`name`、`kind`、`max_bytes` | `text` / `markdown` / `json` / `code` / `image-file` / `rows[[cell]]` / `unsupported` | `image-file` 必须是插件私有临时目录里的**路径**，由 Mo 用现有图片渲染器读；绝不接收插件直接递来的位图字节 |
| `list` | `source`、`query?`、`cursor?` | `rows[{id,name,path?,icon?,subtitle?}]`、`next?` | 第一版**只读**：要么整行映射成一条普通条目（点击仍走 Mo 的导航 / 回收站 / 撤销），要么就是个不动的列表 |

**插件 → 宿主只有一个 `host.log`。** 刻意不做 `host.read_file` 这类反向请求：有了它，
capability 模型就是破的（插件想读什么自己发个路径即可）。

**超时与崩溃**：每次调用带 `deadline_ms`，超时直接 kill 该进程、按「这一问没回答」处理，
同一次 UI 操作内不重试；连续失败 3 次进指数退避、记 `disabled_until`，扩展管理器显示
「已停用 · 查看日志」，**内置预览照常**。

## 6. 安装、权限、卸载

* **安装 = 应用内一步**：扩展管理器加「从磁盘安装」→ 选一个含 `manifest.json` 的目录或
  `.moext`（zip）→ `validate` → 复制进 `<配置>/mo/extensions/<id>/` → **权限确认框**逐条列
  `capabilities` 与 `methods` → 写 `installed.json` 记录来源与每个文件的 sha256。
* **这一轮不做下载 / marketplace**：那是签名与信任问题，不是插件架构问题；没有可信源之前
  做下载等于诱导用户跑陌生 exe。
* **capability**：`read-names`（默认给）、`read-contents`、`write`、`net`。安装时一次性征求，
  改权限要重装；provider 进程只能拿到入参里给的东西。
* **卸载** = 删目录 + 清它在 `<缓存>` 里的 classify / preview 行。启停状态仍按目录名记录。
* **兼容**：`min_mo` 不满足 → 装载但标灰并提示；协议版本对不上 → 保留声明层、不启 provider。

## 7. 缓存与性能

* `classify` 结果进 `<缓存>/plugin-classify.sqlite`，按 mtime 失效，与缩略图同一套失效逻辑。
* **懒启动 + 空闲回收**：只启动「声明了 provider 且当前界面用得上」的插件，空闲 30 s 回收。
* 派发判据先过 `TypeRegistry`：一个只覆盖 3 个扩展名的插件，不该为整棵树起进程。

## 8. 排期（每步独立可交付、可测）

* **P1 纯宿主重构，零插件概念：✅ 三步全部落地（2026-09-27）**——`TypeRegistry` 收三处
  扩展名表（§4.1）、`ActionRegistry` 收命令面板 + 右键菜单（§4.2）、侧栏改数据驱动
  （§4.4）。
  验收：全量测试绿 + 新增「三处类型答案出自同一条 rule」的一致性测试。
  （P1-1 那条的落地形态是「三问三函数 + 一张钉住已知矛盾的测试」，验收口径不变。）
* **P2 声明层生效**：清单四类 + 安装/权限框 + 扩展管理器展示「本扩展贡献了什么」。
  验收：tests 里一个 fixture 扩展能加一条右键菜单项、一个 `.srt` 标签、一个侧栏项，
  headless 断言渲染出现；**反向验证**（注释掉注册点必须变红）。
* **P3 provider 协议**：进程监管 + `classify`/`preview` + 缓存 + 超时 kill。
  验收：一个 example 插件当夹具；「provider 卡死 / 崩了，UI 不受影响」的确定性测试。
* **P4 `list` 列表源**：单独一轮（最依赖前三步，也最容易撞 Mo 列表的不变量）。

## 9. 现在就不看好的三点（留档，别到时候当意外）

1. **`list` 想接进正式列表会撞一整套不变量**：选中 / 分组 / 隐藏过滤 / 分页 / 回收站 / 暂存。
   第一版按只读做，是止损也是省命。
2. **`classify` 给不给文件头 4 KiB 是个两难**：不给，容器格式与编码判定答不了；给了，
   等于把内容读给插件看，而且用户会被反复弹权限。默认只给名字和大小，先按这个实现，
   看真实插件的反馈再调。
3. **`preview` 走进程意味着每次预览可能有一次 IPC**：`max_bytes` 与缓存能压住，但慢插件
   的观感一定不如内置渲染器；所以内置那批（图片 / PDF / 文本）**永远不进 provider**，
   插件只接管内置不会的那些类型。
