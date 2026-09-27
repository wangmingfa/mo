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
* 落地进度与形状差异：`menu` 见 §4.6（挂在命令自己身上，不是独立的 `menu` 数组），
  `types` 见 §4.7（只收 `ext` + `label`，`group` / `icon` 未收）。

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
pub(crate) enum Slot { Palette, ContextFile, ContextBlank }   // P2-2 起是 mo_app::MenuSlot 的别名
pub(crate) enum ActionKind { User(usize), Workflow(usize) }
pub(crate) struct ActionSpec { pub title: String, pub category: String,
                               pub kind: ActionKind, pub slots: Vec<Slot> }
pub(crate) fn contributed(users, workflows) -> Vec<ActionSpec>  // slots 从 P2-2 起读每条的 menu 声明
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
这一步当时**没做**（菜单还吃不到它，为一个不存在的消费者在每次右键时扫一遍配置目录不划算）
→ **2026-09-27 已做，落地形状见 §4.5。**

P2 剩下要做的：~~把清单 `where` 里的槽位灌进 `slots`~~（✅ 2026-09-27 P2-2，落地形状与
一次形状偏离见 §4.6——**没有**做成独立数组，而是命令自带 `menu` 字段）；
要引用内置动作时给 `ActionKind` 加一臂 `Builtin(&'static str)`（`dispatch_action` 已经按
字符串 id 匹配，是现成的）；`toolbar` 槽位。侧栏那一层已经接好了（§4.4 / P1-3），P2 只欠把清单的 `sidebar` 字段
映射成一条 `Row`。

**验收**：`cargo test -p mo-ui` 118 绿（新增 5 条：注册表 3 + 菜单 2）；反向验证是把
`contributed` 的默认槽位加上 `ContextFile` → `everything_is_palette_only_for_now` 变红。
（这一轮还被 `mv` 恢复旧 mtime 骗了一次，cargo 复用了变异体的产物、绿的其实是上一个二进制，
详见 [engine-testing.md §8](engine-testing.md)。）
*⚠️ 那条靶子测试的名字随 P2-2 变了*：`everything_is_palette_only_for_now` 当时测的是
「一律只投面板」，投递改成看声明之后它不再成立，拆成了 `no_declaration_stays_palette_only`
（没写 = 仍只投面板，向后兼容）+ `declared_slots_reach_exactly_the_listed_surfaces`
（写了 = 精确投递，这一条才是今天的反向验证靶子）。

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

### 4.5 菜单自带一份贡献表 —— ✅ 已落地（2026-09-27，P2-1）

§4.2 那条 ⚠️ 接线做掉了，但落地的形状不是「在 `open_context_menu` 里重取一下镜像」，
而是**重取的结果连着载荷一起进 `ContextMenu`**：

```rust
pub(crate) struct Contributions { specs, commands, workflows }   // actions.rs
impl Contributions { fn of(users, workflows) -> Self; fn payload(kind) -> Option<Payload> }
pub(crate) enum Payload { User(mo_app::UserCommand), Workflow(mo_app::Workflow) }
// context_menu.rs：字段 contributions，items() 不再收 &[ActionSpec]
```

两个理由，都是「这张表被谁读」：

1. **一次右键只有一个事实来源**。渲染读 `menu.contributions`，点击执行的也是。
   收口之前是「渲染时现取一张表，点击时再按下标回 `RootView` 的镜像里捞载荷」——
   中间隔着一次可能被命令面板重取的镜像，正是不对称的地方。
2. **下标得有人兜住**。`Contributions` 把「第 i 条」和「第 i 条是什么」绑在同一份数据上；
   查不到就明确报「已不在当前列表里，未执行」，**绝不按位置猜一条顶上**——按位置猜
   等于静默执行用户没点的那条命令。§4.2 说的「载荷带身份而不是下标」到这一步才算还清。

判据：`selected_ext_names(panel)` 里抽出 `ext_names_of(&[PathBuf])`，两处共用；
右键的输入是**本次目标 ∪ 当前选区**。目标一般已在选区内（右键会先单选它），但条目
可能刚好不在可见窗口（watcher 刚换过列表），所以显式并进来。

执行侧：`run_user_command_at` / `run_workflow_at` 留作**下标入口**（面板那条路——打开时
重取、Enter 立刻执行，中间没有别的写入者），本体拆成 `run_user_command(cmd)` /
`run_workflow(wf)`。两个入口一条执行路径，占位符守卫与输出回显不分叉。

**代价照实记**：`AppState::user_commands()` 每次都读配置目录 + 全部清单（磁盘 IO），
现在**每次右键**付一遍。开一次命令面板本来就是同样的开销，右键量级相同，没加缓存——
加缓存就要处理「用户刚改过清单」的失效，而那件事面板今天也靠每次重取解决。
P2 真让动作进菜单之后，若右键有可感延迟，再考虑按 `mtime` 失效的一层。

⚠️ 这一轮踩到的一次「单跑绿、整包跑红」（与 [windows-port.md §27](windows-port.md)
同一类）：fixture 一开始种在**私有**配置目录、临时改 `MO_CONFIG_DIR`。那是进程全局的，
同进程并行的测试各自调 `isolate_user_dirs_for_tests()` 会把变量换回去，于是本测试读不到
自己的 fixture。改成种进**共享**隔离目录的 `<MO_CONFIG_DIR>/extensions/mdstats/`、
测完删掉、全程不动环境变量，就自洽了。**教训**：测试要改的是「目录里有什么」，
不是「目录是哪个」。

**验收**：`cargo test -p mo-ui --lib` 131 绿（本轮新增 2 条：`actions.rs` 的下标自足、
`app.rs` 的真接线）。`app.rs` 那条是本阶段第一处**真从磁盘读扩展清单**的 UI 测试：
种一个 `when_ext: [".md"]` 的 fixture 扩展 → 右键 `README.md` 的快照里有
「Markdown 统计 · 统计字数」、右键 `notes.txt` 没有、且面板镜像仍是空的。
**反向验证**：把 `contributed_for` 改回读 `self.user_commands`（收口前的写法）→ 第一条
断言红（`.md` 那条进不了快照）。另加一条投递层的反向靶子仍在 §4.2 那两处。
（*P2-2 之后这一段口径已经升级*：那两条 UI 测试断言的是**渲染出来的菜单行**，fixture
也带上了 `menu` 声明，测试本身改名叫 `context_menu_renders_contributions_by_slot_and_target`
——见 §4.6。）


### 4.6 槽位写在命令自己身上 —— ✅ 已落地（2026-09-27，P2-2）

§4.5 之后菜单已经带着一份贡献表了，但那份表里**每一条的 slots 都是硬编码的**
`vec![Slot::Palette]`——收口时故意留的，为的是「先让改行为变成改数据」。这一轮就是
把那句硬编码换成数据。

**与 §3 设计稿不一致，而且要说清为什么**：设计稿里 `menu` 是清单上的一个**独立数组**
（`{ "action": "srt-tools.stat", "where": [...] }`，动作靠 `<ext-id>.<name>` 字符串引用）。
落地改成**命令自带一个 `menu` 字段**：

```jsonc
{ "name": "统计字数", "shell": "wc -w {file}", "menu": ["palette", "context:file"] }
```

1. 命令今天已经有两个来源（`config.commands[]` 与扩展清单 `commands[]`），两边都是同一个
   `UserCommand`。投递落点是**这条命令**的属性，做成独立数组就得再造一套「引用哪条命令」
   的 id 解析——而 `<ext-id>.<name>` 是**展示名**（带 `扩展名 · ` 前缀，见 `flatten`），
   拿它当外键，打错一个字就是静默失配。
2. 独立数组真正的用武之地是**给内置命令重排界面**（「把压缩也加进右键菜单」）。那需要
   `ActionKind::Builtin(&'static str)` 这一臂，注册表今天没有。等到真有这需求时再引入
   数组，两种形状可以共存——`where` 指向内置动作用字符串、指向扩展命令时干脆不用写。

语义（`MenuSlot`，在 `mo-config`）：

* **缺省 / 空 = 只进命令面板**，与 P2 之前逐条一致（`MenuSlot::defaults()`）。老配置、
  老清单一个字都不用改。
* **写了 = 精确投递**，不是追加。一条只写 `context:file` 的命令从此不出现在面板里。
  这一点最容易反直觉，所以 `contributed()` 只认这一条规则，别处不再补默认值。
* 写法宽容：`context:file` 与 `context-file` 等价（冒号是设计稿的形状，连字符是手改 JSON
  时更顺手的形状），大小写与首尾空白不敏感。回写用的规范写法是冒号那种（`as_str`）。
* **认不出的名字 = 整条不加载**，报错把可用的三个列出来。判据与「空 shell」同级：
  宁可少一条命令，也不要「用户写了 `context:flle`，结果什么都没发生、也没人说一句」。

**模型里存字符串而不是枚举**（一次值得记下的偏离）：`Config::load` 任何一处反序列化失败，
`AppState::config()` 都会 `unwrap_or_default()` 把**整份**配置回落成默认值——一个字母打错的
槽位名会清空用户所有设置。所以：`menu: Vec<String>` 存原样，读侧 `slots_of` 尽量可用，
`validate` 负责明确报错。以后往 config 里塞任何新字段都照这条办。

三处各自的判据（一处一个职责，不重复判断）：

| 位置 | 职责 |
|---|---|
| `mo-config` | `MenuSlot`（`parse` / `as_str` / `defaults`）、`slots_of`、`first_bad_slot`、`slot_error` |
| `mo-app` | `usercmds::validate` 与 `workflows::validate` 各调一次 `slot_error`。**扩展清单不用另写一处**：`extensions::validate` 本来就逐条走 `usercmds::validate`；扩展带的工作流经 `AppState::workflows()` 里 `sanitize` 的第二遍 |
| `mo-ui` | `Slot` 从「本 crate 自定义枚举」改成 `mo_app::MenuSlot` 的**别名**；`contributed()` 读 `u.slots()` / `w.slots()` |

UI 侧不再有任何「默认投到哪儿」的判断——那是声明的事。`context_menu.rs` 的
`push_contributed` 与 `commands_in` 那两处**一行都没改**：P1-2 收口时它们查的就是
`for_slot(specs, …)`，这一轮只是让那张表里的 slots 第一次真的不一样。

**照实记的缺口**（不是遗漏，是这一轮没做的）：

* **空白处右键也吃选区的 `when_ext`**。`open_context_menu` 算判据用的是
  `选区 ∪ target`，右键空白时 target 是 `None`、选区还在。于是「刚才选中一个 `.md`、
  现在对着空白右键」会让 `when_ext: [".md"]` + `menu: ["context:blank"]` 的命令出现。
  讲得通（条件看的是用户手里有什么），但 §3 里 `when_ext` 的意图是给「针对文件的动作」，
  `context:blank` 该不该受它约束，等真有插件提出来再定。
* `ContextFile` 不区分文件与目录，也还没有 `toolbar` 槽位（§4.2 列的下一项）。今天没有
  需要它的作者，先不加没人为之付维护成本的槽位。
* 改这个字段**只能手写 JSON / 清单**：全仓没有任何界面写 `Config::commands`（grep
  `.commands =` / `.commands.push` 只命中读侧），扩展管理器也还没展示「这条投到了哪儿」。
* 一条只投 `context:file` 的命令在面板里搜不到。如果之后有用户觉得这是消失了的 bug，
  解法应当是「显式声明就按声明走 + 界面上给一句说明」，而不是偷偷补一个 Palette。

**验收**：`cargo test --workspace --all-features` 全绿（完整日志：`mo-config` 7、
`mo-app` lib 54、`mo-ui` lib 132、`--test layout` 35，其余各集成套件全过）。
新增 8 条（`mo-config` 3：宽容解析 / 缺省回落 / 空字段不写进 JSON；`mo-app` 4：命令、
工作流、扩展清单各一条「认不出就整条丢掉」、一条「摊平不丢声明」）、升级 3 条
（清单加载那条现在同时验「写了的按声明走」与「写错的被丢掉」；`actions.rs` 与 `app.rs`
各一条按新语义重写）。
**反向验证**：把 `contributed()` 里用户命令那臂的 `slots: u.slots()` 换回 P2-1 的硬编码
`vec![Slot::Palette]` → 两条红，且各红在自己在的层：
`actions::tests::declared_slots_reach_exactly_the_listed_surfaces`（注册表里就没有
`ContextFile` 这一项）与
`app::tests::context_menu_renders_contributions_by_slot_and_target`（后者报出的正是整张菜单
只剩内置那 15 行，一句 `menu` 声明什么也没换来）。改回后 `touch` + 重跑：132 绿。

⚠️ 这一轮的**测试环境**记录了一条与投递无关的偶发红（整包并行时 `layout.rs` 的
`trash_empty_asks_for_confirmation` 红过两次，单跑与第三次整包都绿），连同判据一起记在
[engine-testing.md §9](engine-testing.md)。同一轮里顺手把那条断言改成轮询
（`wait_for_trash_state`，与 `wait_for_panel_rows` 同一手法），改完
`cargo test --workspace --all-features --no-fail-fast` 连跑两次都是 0。


### 4.7 清单 `types` 的标签灌进「种类」列 —— ✅ 已落地（2026-09-27，P2-3）

§3 草案里 `types` 有四个键（`ext` / `group` / `icon` / `label`），这一轮**只收 `ext` +
`label`**：`group` 要动分组的排序路径（`mo_core::view` 那条），`icon` 要动图标 atlas 的
键空间，两个都不是一句清单能安全改出来的。规矩是「收一个字段就投一个字段」——不收
「解析了却没人读」的字段，那种字段的代价是作者写了、界面上没有，还没人告诉他。

```json
"types": [ { "ext": [".srt", ".vtt"], "label": "字幕" } ]
```

* **投递点**：列表与回收站的「种类」列（`mo_ui::file_item::kind_by_ext`），**先于**内置
  那张表查。这与 §4.1 第三条规则同一个方向：作者更懂自己的格式，允许覆盖内置答案。
* **判据同 `mo_core::types`**：小写、不含点、按 `Path::extension()` 的口径（= 最后一段）。
  所以 `.tar.gz` 在 `validate` 阶段就被拒——写了永远命不中（实际命中的是 `gz`），
  「写了不生效」比「写不出」难查得多。`*` 之类的 glob 同样拒。
* **冲突**：同一份清单里两条抢同一个后缀 → 整份清单不加载（与「命令重名」同一条纪律）；
  不同清单抢同一个后缀 → **先到先得**（`load` 按目录名排过序，所以是确定的）+ 告警。
* 关掉的扩展（`enabled: false`）整体消失，包含它的类型标签——留着就是「停用了还在改我的显示」。

**为什么要一份缓存（这一轮真正的形状问题）**：「种类」是**每帧每行**都要答的一问，
而答案的上半截在磁盘上的清单里。`AppState::extensions()` 每次调用真读盘（右键一次
读一次是无价的，见 §4.5），每帧读一次就是把 customization.md §10 那条坑再踩一遍。
所以 `AppState::type_labels()` 走 [`extensions::fingerprint`] 签名缓存：

| 谁 | 付什么 |
|---|---|
| `file_list::render` | 每帧一次 `type_labels()`（签名一致时 = 一次 `read_dir` + 每份清单一次 `stat`），行循环里只做 `BTreeMap` 查询 |
| `extensions::fingerprint` | 扩展个数与目录名 + 每份清单的 mtime 与长度 → 一个 `u64` |
| `AppState::type_labels` | `Mutex<(签名, Arc<表>)>`，签名没变就只 clone 一个 `Arc` |

作废判据用签名而不是 TTL（`net_shares` 那类用的是 TTL）：用户手改清单下一帧就是新文案，
不必重启，也不用在 TTL 到点前看着旧文案。盲区写进函数注释了：同 mtime 同长度的原地改写
（NTFS/APFS 精度下基本不可能）会缓住旧表。

**验收**：mo-config 7 / mo-core 39 / mo-app lib 57（+3：坏写法、摊平与先到先得、签名跟随
改动）/ mo-ui lib 133（+1：贡献标签先于内置、目录不参与）/ `layout` 36（+1 条 headless：
fixture 扩展的「字幕」真的出现在渲染出来的一格上）。

⚠️ **这一轮先把一条空壳测试改掉了**：第一版的 headless 断言用的是新增的
`panel_kind_labels_for_tests` 访问器，它自己调 `AppState::type_labels()` + `kind_label()`
重算一遍。反向验证（把 `file_list::render` 传进 `view` 的那份表换成空的）之后**测试照绿**
——接线断了没人知道。现在的做法是种类列的 debug 选择器**带上那一格的文案**
（`mo-kind-cell-字幕`，`file_item::meta_cell` 的 `selector_text`，只在闭包里 `format!`，
release 不登记选择器所以不付这份分配），断言打在 `debug_bounds` 命中与否上，也就是打在
渲染产物上；那个访问器删了。日期/大小两列不带文案（每行都不同，带了等于没有稳定把手）。
两条变异体都真跑过：① `file_list` 递空表 → layout 那条红；② 查表钥匙漂成
`to_uppercase()`（即 `mo_core::types` 那条判据被破）→ `contributed_type_label_wins_over_builtin`
红，报 `left: "SRT 文件" right: "字幕"`。两处还原后各自重跑绿（还原是**写文件**、mtime 跟着变，
所以不会复用变异体那份二进制——用 `mv`/`cp` 还原时才需要补 `touch`）。

已知缺口（下一轮别当意外）：
* 「种类」文案仍是 `mo_ui::file_item::kind_by_ext` 里那张**独立的**扩展名表——P1-1 只收了
  分组/预览/图标三问。贡献接缝开在 UI 侧而不是 `mo_core::types`，因为这一问的答案是
  运行时字符串（`&'static str` 表装不下），也不该在热路径上查表。真要给类型知识收尾，
  把这张表搬进 `mo_core::types::label_of` 是第一步。
* 分栏 / 网格 / 画廊视图不显示「种类」列，所以它们的行不受 `types` 影响（不是漏接）。
* 点文件（`.gitignore` 这种）`Path::extension()` 是 `None`，插件无法给它起种类名。
* `group` / `icon` 未收（见上）。
* 缓存只在**重绘时**才检查签名：列表停着一动不动时改了清单，界面不会自己变（滚动一下就有了）。

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
  * **P2-1 ✅（2026-09-27）**：菜单按本次右键目标重取贡献表并自带载荷（§4.5）。
    这一步**没有任何界面变化**（投递策略仍是一律 `Slot::Palette`），它是那条验收标准
    的前置——fixture 扩展的命令现在只跟着目标过滤，不再跟着「上一次开面板的选区」。
  * **P2-2 ✅（2026-09-27）**：清单 / 配置里的 `menu` 声明真的决定投递（§4.6）。形状与
    §3 的独立 `menu` 数组**不同**（命令自带字段），理由与语义（缺省=只进面板、写了=精确
    投递、认不出=整条不加载）都记在那一节。自此 §8 那条验收标准的前半句成立：
    **fixture 扩展能往右键菜单里加一项**，headless 断言打在渲染出来的菜单行上。
  * **P2-3 ✅（2026-09-27）**：清单 `types` 的 `label` 决定「种类」列（§4.7）。自此 §8
    那条验收标准的第二半句成立：**fixture 扩展能给一个类型起名字**（`group` / `icon`
    本轮明确不收）。顺带修掉了一条空壳测试——headless 断言从此打在渲染出来的那一格上。
  * 剩下：清单 `keybindings` / `sidebar` 两类字段（`types` 只落了 `label`，见 §4.7）、
    安装与权限框、扩展管理器展示。
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
