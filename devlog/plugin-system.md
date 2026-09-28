# 插件系统（扩展包 / 能力分层 / provider 协议）

> **状态：P1 + P2 全部落地（P2 至 2026-09-28 收口，见 §4.13）；P3（provider 协议）/
> P4（list 列表源）未实现。** 本篇不遵守 devlog
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
  `types` 见 §4.7（只收 `ext` + `label`，`group` / `icon` 未收），`keybindings` 见 §4.8
  （同样挂在命令自己身上，是 `key` 字段，不是独立数组），`sidebar` 见 §4.9（也并进 `menu`
  了：它是第四个槽位，不是第四类数组，`section` / `icon` 两个字段未收）。

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
   （真落地那一轮载荷换成了声明本体而不是 `usize`，理由见 §4.9 第 4 条：这批数据每帧重取，
   下标参照的那两份 vec 也每帧重取。）
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

### 4.8 清单里的 `key` 灌进键表 —— ✅ 已落地（2026-09-27，P2-4）

§3 草案把快捷键写成独立的 `keybindings: [ { action, key } ]`。这一轮**没有照这个形状做**，
改成 `UserCommand.key` / `Workflow.key`（字符串原样存，解析发生在 `mo_ui::keys` 那一侧）：

```json
"commands": [ { "name": "统计字数", "shell": "wc -w {file}", "key": "cmd+alt+shift+j" } ]
```

理由与 §4.6 完全同一条：独立数组要用 `<ext-id>.<name>` 当外键，而摊平之后的显示名带
「扩展名 · 」前缀（`flatten` 加的），名字又是给用户看的文案——**拿文案当外键，写错一个
字母就是一条静默失联的绑定**。字段挂在动作自己身上就只有两种状态：这条命令不存在
（加载阶段整份清单报错），或者它带着自己的键位进表。没有「命令在、键位对不上」这第三种。

**这一轮真正定下来的是五条语义**（不是代码量，是判据）：

1. **绑了键的命令不能受 `when_ext` 约束**——`extensions::validate` 阶段整份清单拒掉。
   组合键按下时**没有「本次右键的目标」这个上下文**，键表也不按选区过滤；绑上去就等于
   「选中 .txt 时按这串键也会去数字幕」。与其在按下时再判一次选区（那就是两处判据，
   于是「有时生效有时不生效」），不如在加载时拒掉，并把「要绑键就去掉 `when_ext`」写在
   错误文案里。
2. **内置那张（连同用户的改绑与显式解绑）先占位，贡献项后插**。撞了跳过 + `tracing::warn!`
   点名是谁想抢谁——插件永远抢不走 ⌘T，用户说这颗键什么都不该干时，扩展也插不进去。
3. **贡献项一律算「读当前选择」**（`Chord::acts_on_selection`）：它们靠 `{file}` / `{files}`
   工作，遮罩后面那个列表的选中项正是输入，所以模态打开时吞键，与 ⌘C 同一个尺度。
4. **`reset_all` 恢复内置默认，但保留贡献项**——「恢复默认」是用户对内置那张表的权力，
   不是对别人清单的权力。
5. **键表在启动时和每次 `palette.open` 重取**：手改清单里的 `key` 不必重启 Mo。

`Chord` 是 `Builtin(&'static str) | Command(UserCommand) | Workflow(Workflow)`——贡献那两支
**带载荷本体**而不是 id。宿主事先不知道有这条命令，它的可执行内容（命令行、来源）在清单里；
命中即执行，不留「拿个 id 回头再查一遍列表」的空档，那份列表随时会被重取（§4.2「载荷带身份
而不是下标」同一条纪律）。取命令那份用 `user_commands(&[])`（**不带选区过滤**的）而不是
命令面板用的镜像：镜像按当前选中扩展名筛过，用它就等于让键表跟着选区变。

**验收**：mo-config 7（`empty_menu_is_not_serialized` 现在也管 `key`：空不写进 config.json、
来回一致、`"   "` 不算绑定）/ mo-app lib 59（+2：`when_ext` + `key` 被拒、`key` 摊平后活着）/
mo-ui lib 139（+6：键表 5 条 + 1 条 headless 端到端）。

⚠️ **gpui 的 `simulate_keystrokes` 按 `-` 切键串**（`ctrl-alt-shift-j`），而 Mo 自己的
`KeyCombo::parse` 是「有 `+` 就只按 `+` 切，没有才退回 `-`」（因为 `-` 本身是 `view.zoom_out`
的主键）。两套解析器不是一把尺子，端到端测试里必须写 `{main}-alt-shift-j`。这句是我踩出来的：
断言先红在 `modal = None`，我另加了一次性探针确认键表查询本身是绿的，才定位到是**测试自己
拼错了串**而不是接线断了。同理，那条测试不起进程：命令的 `{file}` 占位符守卫在
`spawn_blocking` **之前**返回 Err，所以 headless 里只会看到「未执行」的信息卡。

**反向验证**（五条变异体，每条还原后各自重跑绿；还原都是**写文件**，mtime 自变，不需要补 `touch`）：

| 变异 | 红在哪 |
|---|---|
| M1 `keymap_from` 把命令那份换成空表 | `in_table` 那句：清单里那句 key 应当已经进表（带着那条命令自己） |
| M2 `dispatch_chord` 里 `Chord::Command(_) => {}` | 末尾匹配 `Modal::Info` 失败，实际 `modal = None` |
| M3 `Chord::acts_on_selection` 对贡献项返回 `false` | 「遮罩后面不该执行扩展绑的那条命令」（键表那条单元测试同时红） |
| M4 `validate` 里 `when_ext` + `key` 那条守卫短路 | `rejects_chord_on_ext_gated_command` |
| M5 `push_chord` 的占位检查形同不存在 | `bad_contributed_chords_are_dropped_not_guessed` 数条目 42 ≠ 41 |

⚠️ M5 **没有**杀掉 `contributed_chords_never_steal_builtin_slots`：查表返回的是**第一个**命中，
内置先插入所以照赢，于是那条守卫的可观测后果只剩「不产生重复条目 + 那句告警」。那两条测试
钉的是不同的事（一条钉插入顺序，一条钉不重复），别把它们当同一件事的两遍——留档免得下一轮
把「重复」这个洞合上时以为还有测试兜着。

已知缺口（下一轮别当意外）：
* 设置里的「快捷键」页只遍历 `BINDINGS`，**贡献进来的键位在界面上看不见**（要改只能改清单）。
  这一条和「扩展管理器展示本扩展贡献了什么」是同一个缺口，P2 收尾那轮一起做。
  （→ ✅ 2026-09-28 已做，见 §4.13；设置页是只读展示，改绑仍去清单。）
* 坏键串与撞车只有 `tracing::warn!`，界面上没有「这条绑定没生效」——与 §4.6 那条「认不出就
  整条不加载」是同一个未接的口子。
* 清单改完要开一次命令面板（或重启）键表才重取；只滚动列表不会刷新。
* 冲突告警里点名贡献命令用的是**摊平后**的名字（带扩展前缀）。界面口径是对的，但作者在
  清单里写的不是这个串，照着告警找不到自己那一行。

### 4.9 清单的 `sidebar` 灌进侧栏 —— ✅ 已落地（2026-09-27，P2-5）

§3 草案把侧栏项写成第四类独立数组：

```json
"sidebar": [ { "section": "字幕", "action": "srt-tools.panel", "icon": "subtitle" } ]
```

这一轮**没有照这个形状做**，改成了 `menu: ["sidebar"]`——也就是 `MenuSlot` 的第四个变体
（`palette` / `context:file` / `context:blank` / `sidebar`）。三处差异，各有各的理由：

1. **不是独立数组，而是一个投递落点**。独立数组要靠 `action: "srt-tools.stat"` 当外键，
   那个键在摊平之后是「扩展名 · 命令名」的展示文案（§4.6 为同一件事已经付过一次学费）；
   更麻烦的是它让**同一条命令**在两处声明投不投——菜单一份名单、侧栏一份名单，谁说了算
   没有答案。并进 `menu` 之后只有 `goes_to(slot)` 一处判据（`mo_ui::actions` 建注册表读
   的 `slots()` 是同一个来源）。
2. **没有 `section` 字段**。分区名直接用这条动作自己的 `category`（`UserCommand::group`
   / `Workflow::group`）——「这条动作归在哪一组」命令面板已经答过一遍，侧栏再问一遍就是
   同一个问题两处作答，两处迟早分叉。扩展命令的 `category` 缺省时 `flatten` 填扩展名，
   所以一个扩展在侧栏里自然聚成以自己命名的那一区，与草案 `section: "字幕"` 的效果同源。
3. **没有 `icon` 字段**。贡献项共用一颗 `icons::EXTENSION`（三格 + 右下角一颗「＋」，与
   `VIEW_GRID` 那四格填满刻意区分：侧栏里两种语义都有，撞成一个形状就没有分辨价值）。
   与 §4.7「收一个字段就投一个字段」同一条纪律——在有一张可信图标表之前，不引入「用户
   写了、界面上看不见」的字段。

**这一轮定下来的是五条语义**：

1. **投给侧栏的命令不能受 `when_ext` 约束**，`extensions::validate` 阶段整份清单拒掉。
   这条守卫的价值全在「不拦也不会报错，只是永远不出现」：侧栏取的是 `user_commands(&[])`，
   而 `flatten` 见 `selected_exts` 为空就把受约束的命令**整批滤掉**，所以「`when_ext` +
   `menu: ["sidebar"]`」是一份静默失效的声明。与 §4.8 快捷键同一条判据（这两个界面都没有
   「本次右键目标」这回事）。
2. **分区 = `category`，同区合并、按首次出现排**，贡献区永远追加在内置五区之后（内置区的
   顺序与位置一个字没动）。空声明一个区都不产出（`title_when_empty: false`）。缺省怎么填
   也是同一份答案：配置里的裸命令 `category` 空 → 「自定义」区，工作流没有 `category` 可写、
   恒进「工作流」区（`group()` 那三个实现就是这三句，侧栏不另立判据）。
3. **点击不导航**：`Activate::Contributed` 这一臂**不** `leave_secondary_view`——「在回收站
   面板里点了扩展的一行」应当是「跑那条命令、人还在回收站」，与命令面板那头同源。
4. **元素 ID 由声明派生**（`sidebar-ext-{cmd|wf}-{摊平后的名字}`），不用「侧栏第几行」那种
   位置号：这批数据每帧重取（启停一个扩展、改一条配置都会让顺序变），而测试和排错时手敲
   的选择器要能指着同一条。唯一性靠构造保证——同名命令在 `user_commands` / `workflows`
   各自去重过，命令与工作流再由类型段分开。载荷也带声明本体而不是下标（§4.2 那条纪律）。
5. **执行汇进现成那条路**：`run_user_command` / `run_workflow`，与命令面板、右键菜单、
   键表共用，侧栏这一侧不写第二份执行逻辑，也不为它多画一条 div 链。

**每帧取，但缓存过**。侧栏的渲染路径每帧都要问一次「有哪几行」，而这一句的上半截在磁盘上：
`user_commands(&[])` 一次调用就把 `config.json`、`commands/*.json`、每个扩展清单全读一遍再
解析——不缓存就是每帧十几起 IO，顺带把「重名」那几条 `tracing::warn!` 刷成每帧一条（§2 的
热路径纪律）。缓存挂在 `AppState::sidebar_entries()` 上，判据是新增的
`extensions::declarations_fingerprint(config.json)`：`config.json` 本身 + `commands/` 目录里
每一份 + 整个 `extensions/`（复用 §4.7 那个 `fingerprint`）。**三条来源都要签**，因为侧栏项
正是这三处都能声明——只签 `extensions/` 的话，用户在手改 `config.json` 加一条
`menu: ["sidebar"]` 之后侧栏不动。副作用说清楚：`config.json` 的 mtime 会随**任何**一次设置
改动而变（改列偏好、开关侧栏都落盘），所以这一份签名比 §4.7 那个更容易失效——那是安全方向
的失效（多读一次盘），而不是缓住用户的声明。反面也要说：缓存挡掉的是「每帧读盘 + 解析」，
挡不掉 `read_dir` / `metadata` 那几次系统调用，**每帧仍然 stat 一遍配置目录、`commands/` 和
每个扩展的 `manifest.json`**。扩展数上到几十、或配置目录落在网络盘上时这里就是下一处热点，
届时该问的是「侧栏为什么每帧重建」（§2 第 2 条那句同一尺度）而不是「再加一层缓存」。
与 §4.8 那条「要开一次命令面板才重取」不同，侧栏每帧都问签名，所以手改清单下一帧就出来，
不必重启。

**验收**：mo-config 8（+1：`SLOT_NAMES` 那份**手写**的可用列表四槽齐全——漏一个不会编译失败，
只会让提示骗人；`sidebar` 三种写法都认则补在了原来那条宽松解析测试里）/
mo-app lib 62（+3：只挑 `sidebar` 那几条、`when_ext` + 侧栏被拒、签名跟着三条来源变）/
mo-ui lib 141（+2：`contributed_sections` 那条单元测试 + 一条 headless 端到端）。全量
`cargo test --workspace --all-features --no-fail-fast` → `TEST_EXIT=0`，541 条 `test … ok`。
端到端那条是真管线：往共享测试目录种一个 `p25sidebar/manifest.json`，断言行出现在渲染帧里、
落在 `mo-sidebar` 的横向范围内，点它之后 `Modal::Info` 里点名是哪条命令。

⚠️ **三条踩出来的**：
* `debug_bounds` 要 `&'static str`（选择器不参与运行时拼接），所以测试里写常量
  `SELECTOR`、再用 `strip_prefix` 从常量里截出命令名——两边各拼一次的话，改了一头另一头
  还在通过。
* 测试目录 `mo-test-config-<pid>` 会跨轮复用（Windows 回收 pid），**任何按位置选行的选择器
  都会在下一轮指着另一条**。这一轮改成按名字派生，顺带把这个坑填了。
* §4.4 那条老坑（循环里的 `text!` 必须显式给 ID，否则 a11y 一挂就 debug panic
  `0xc0000409`）这一轮长出了第二个触发条件：**贡献区的标题是用户写的字符串**，理论上能与
  内置区撞名（有人把 `category` 写成「书签」）。两句一样的标题 = 两个一样的 NodeId = 原地
  复活，而这回是配置触发的、跟代码无关，最难查。heading 的 ID 现在带 `ix` 前缀。

**反向验证**（八条变异体，每条红在预判的那一句断言上；还原都是**写文件**，mtime 自变）：

| 变异 | 红在哪 |
|---|---|
| M1 `render` 那份 `Sources.contributed` 不给（换成空表） | 端到端段1：「清单声明的侧栏行没出现在渲染帧里」 |
| M2 点击臂 `Activate::Contributed(_) => {}` | 端到端段2：实际 `modal = None`（点击臂没接上） |
| M3 `contributed_sections` 里分区名写死 `"扩展"` | 单元测试 `left: ["扩展"]` ≠ `right: ["字幕工具","工作流"]` |
| M4 `validate` 里 `when_ext` + 侧栏那条守卫短路 | `rejects_sidebar_slot_on_ext_gated_command` |
| M5 行 ID 换成位置号 | 「ID 要指着声明本身」 |
| M6 `goes_to` 写成 `!self.menu.is_empty()` | `sidebar_entries_picks_only_the_sidebar_slot` 数名字：四条全进来了（`["只进面板","进侧栏和面板","写错的界面名","也要进侧栏"]`），「有没有声明这个界面」退化成「写没写过这个字段」 |
| M7 `declarations_fingerprint` 不 hash `config.json` | 「改了 config.json 还认成没改 = 侧栏缓死了」 |
| M8 `SLOT_NAMES` 去掉 `sidebar`（长度改成 3，否则先编译不过） | `every_slot_is_listed_in_the_error`：告警文案里的「可用」只剩三个 |

已知缺口（下一轮别当意外）：
* 草案里的 `icon` / `section` 两个字段都没收，贡献项共用一颗图标、分区名只能跟着 `category`。
* 贡献区永远排在内置五区**之后**，没有排序字段可把用户那一区放到快捷访问上面。
* 贡献行没有行尾按钮（停用要去扩展清单里做：三处能改同一件事，就没有一处是权威）、不接受
  拖放、`active` 恒 false（它不对应目录，所以点了也不会高亮——这一行是「做一件事」不是
  「看一个地方」）。
* **点了就跑，没有任何提示**。在 §6 的权限框落地之前，「装个扩展 = 侧栏多一颗一键执行按钮」
  这件事界面上看不出来。这是 P2 收尾那轮（安装与权限框）要还的债，别当界面美化往后排。
  （P2-6 还了一半：**手动启用**必经确认卡，见 §4.10；**手放进目录**的清单缺省即启用，
  这条仍然开着。）
* 声明为什么没出现（`when_ext` 被拒、重名被忽略）只有 `tracing::warn!`，界面上没有一处能查。
  与 §4.6/§4.8 那两条是同一个未接的口子。

### 4.10 扩展管理器摊开贡献 + 启用前确认 —— ✅ 已落地（2026-09-27，P2-6）

§6 那条「安装时一次性征求」的**权限确认**，在既没有安装流程、也没有 `capabilities` 执行点的
现在，能落的形态只有一个：**启用前把这家要往界面里放的东西逐条摊开**。所以这一轮的形状与
草案差三处，都是主动的差别：

1. **不碰 `capabilities`**（§6 那四个字符串）。它是 provider 的授权单位，而 P3 之前没有任何
   一处代码会去检查它——收一个解析了却没人读的字段，正是 §4.7 那条纪律（收一个字段就投一个
   字段）不许的事。等 P3 真按 capability 放行 `read-contents` 时再收，一次收到位。
2. **不碰安装**（选目录 / `.moext` zip / sha256 / `installed.json`）。`mo_ui::dialogs` 整个文件
   都是应用内表单，**没有原生目录选择框**，「从磁盘安装」这一步现在就是一条走不通的按钮。
3. 确认的时机从「安装时」挪到**启用时**。这不需要新语义：清单本来就是「放进 `extensions/`
   即存在」，那么「第一次让它生效」就是天然的授权点。

**五条语义**：
* **点行 = 选中并展开贡献，不翻状态**；翻状态的是行尾那颗状态胶囊（键盘上仍是 Enter）。
  这一页的第一用途从「开关列表」变成「看清楚这家放了什么」，而鼠标用户唯一能选中一行的
  动作如果顺手把扩展关了，「想了解」就成了「有代价」——代价是没人敢点。
* **启用先过卡，停用立即生效**。停用是收缩边界（把已经放进界面的东西撤掉），没什么要再问；
  启用是往界面里放一批陌生的命令行，要点头。这条不对称是刻意的，测试 M5 就钉在这里。
* **卡上带的是扩展 id**（`Modal::ConfirmEnableExt(String)`），不是行号——§4.2「载荷带身份、
  不带可漂移的下标」的第三例（前两例：菜单载荷、侧栏 `Activate::Contributed`）。
* **卡片浮在扩展页之上**，中央区仍按 `Modal::Extensions` 画。A/B 类那套分类里这是
  `ConfirmTrash` 那个例外的第二例：取消 / Esc / 点遮罩都回面板，**不能**走 `close_modal`
  （那会把整页关掉，用户从「不想启用」被踢回浏览器）。
* **措辞只有一份**：数据在 `mo_app::extensions::contributions()`（三臂
  `Command` / `Workflow` / `TypeLabel`，顺序恒定 命令→工作流→类型，**不看 `enabled`**——
  停用的更要能预览），句子在 `mo_ui::app::contribution_line()`，展开区与卡共用同一批句子。
  落点名 `slot_word`（命令面板 / 右键·条目 / 右键·空白 / 侧栏）与界面上那一处的叫法一致，
  否则「卡上写侧栏、侧栏上没东西」这种话就没法核对。`shell` 与每一步都写进句子里——用户要
  审的是**它会把什么交给 shell**，句子只剩「有条命令」等于没问过。

**验收**：mo-config 8 / mo-app lib 63（+1：`contributions_answer_what_the_manifest_will_change`
钉三臂的措辞来源、命名前缀、`enabled=false` 照样摊开）/ mo-ui lib 144（+3：句子单测 +
两条 headless 端到端）。全量 `cargo test --workspace --all-features --no-fail-fast` →
`TEST_EXIT=0`，545 条 `test … ok`。端到端第一条走的是真管线：种一份 `enabled: false` 的清单 →
点行 → 断言两条贡献各占一行渲染出来、**盘上没动** → 点胶囊 → 断言弹卡且卡上行数 = 展开区
条数 → Esc → 断言回到面板且盘上仍没动 → 再点、点「启用」→ 断言这才写盘。

⚠️ **四条踩出来的**：
* `debug_bounds` 认的是 **`debug_selector` 那个字符串**，`window.click` 认的是**元素 ID**，
  两者不是一回事。那两颗按钮当初只给了 `.id("ext-enable-ok")`，测试第一次就红在「按钮没渲染
  （选择器 mo-ext-enable-ok）」——实际渲染了，只是没选择器。以后新加的按钮如果要被端到端点，
  **ID 与 selector 成对写**。
* 只有一份清单时，「按 id 确认」与「按位置确认」是**同一件事**，任何按位置的实现都能通过。
  那条纪律只有第二种场景能钉住：第二条测试种 `p26a` / `p26b` 两份，**选中 B、点 A 的胶囊**，
  于是「启用错了家」和「点胶囊先把选中行换掉」（漏 `stop_propagation`）各自红一处。
* 卡上的**字面文本**在 headless 里读不出（gpui 测试没有文本快照，本文件多条注释同此）。
  退一步钉「行数」：`mo-ext-enable-line-{0,1}` 必须在、`-2` 必须不在。少一条 = 只让用户审一半，
  多一条 = 卡上混进了没审过的东西。行号写死的前提是 fixture 恰好两条，那句 `assert_eq!`
  就是这条测试自己的前置条件（M6 靠它才红得出来）。
* 盘上的 `enabled` 用**扫字符串**判断——mo-ui 不依赖 `serde_json`，为一条测试加依赖不值。
  写回方 `set_extension_enabled` 用的是 `to_string_pretty`，所以 `"enabled": true` 那行必然在。

**反向验证**（六条变异体，每条红在预判的那一句；还原都是写文件，mtime 自变，`diff` 验字节一致）：

| 变异 | 红在哪 |
|---|---|
| M1 点行改成 `toggle_extension`（回到「点一下就把扩展关了」） | 端到端一：「点行不该离开扩展页：`ConfirmEnableExt`」。⚠️ **不是**预判的那句「贡献行没渲染」——确认卡浮在面板上，展开区照画，所以选择器那两句仍然通过。这一格记下来：卡后面的内容挡不住，别指望「选中」类断言替「模态」类断言把关 |
| M2 `toggle_extension` 里启用直接写盘、不弹卡 | 两条端到端都红在「点停用中扩展那颗胶囊应当弹确认卡」 |
| M3 `confirm_enable_ext` 把模态带的 id 换成 `extensions[ext_index]` | 端到端二③：「卡上写的是 A，点了『启用』就该启用 A」+「B 只是被选中着」 |
| M4 胶囊那颗按钮漏 `stop_propagation` | 端到端二②：`left: Some("p26a")` ≠ `right: Some("p26b")` |
| M5 停用也弹卡（把 `if !enabled` 变成恒真） | 端到端二④：「停用是收缩边界，不该再要一次确认」 |
| M6 卡片传空列表（不摊贡献） | 端到端一：「卡上只摊了一条，展开区明明有两条」 |

已知缺口（下一轮别当意外）：
* **「装了就跑」这条债还没还完**：手放进 `extensions/` 的清单 `enabled` 缺省是 **true**，
  所以确认卡只在用户手动停过一次之后才会遇到。真正的闭合要有安装流程（复制进来时写
  `enabled: false`，第一次启用必过卡）——那是 §6 那一半，卡在原生目录选择框上。
* 扩展页仍然说不出「这条声明为什么没生效」（`when_ext` 被拒、坏界面名整条不加载、键位撞车
  被丢弃）。三处 `tracing::warn!` 无处可查，与 §4.6 / §4.8 / §4.9 是同一个未接的口子。
* 卡上工作流那一条**不带扩展名前缀**（`flatten` 只给命令加前缀，工作流从 P0 起就是裸名）。
  于是两家都叫「打包」的工作流在卡上长一样，回溯不到出自哪家。要么给工作流也上命名空间
  （会改命令面板与设置里已有的显示），要么在 `Contribution::Workflow` 里另带扩展名——
  本轮选了「跟着界面现状」，把差别记在这里。
* 贡献进来的键位在设置「快捷键」页仍看不见（那页只遍历 `BINDINGS`）。卡上能看见
  「绑键 cmd+shift+w」，但那页没有这一行，也就没法在那里改绑——P2 收尾欠的最后一块。
  （→ ✅ 2026-09-28 已做，见 §4.13：设置页列出贡献键位与被拒原因，只读不改绑。）

### 4.11 坏声明在界面上可见 —— ✅ 已落地（2026-09-27，P2-7）

§4.6 / §4.8 / §4.9 各记了一条同一个未接的口子：「认不出就整条不加载」「坏键串与撞车只有
`tracing::warn!`」，而界面上没有一处能查。这一轮把两类静默都接上了，判据**各只搬一次**：

1. **加载失败的清单**：`extensions::load` 拆成 `load_report() -> (Vec<Extension>,
   Vec<BrokenExtension>)`——`BrokenExtension { path, reason }`，`reason` 是 `validate` 的
   报错**原句**（或 IO / JSON 错误）。UI 不重判：什么算坏、错在哪，判据仍然只在
   `validate` 一处，面板只搬运。`load` 变成报告的投影（`.0`），键表 / 类型表 / 侧栏那些
   「只要能用的」调用方一行不改。
2. **贡献键位被拒**：`Keymap::push_chord` 拒掉坏键串 / 撞车时把
   `DroppedChord { spec, chord, reason }` 记进 `keymap.dropped()`——`chord` 是**载荷本体**
   不是名字（重名的两家工作流分不出，载荷能对回清单；`Chord::source()` 存的就是清单
   路径原文，命令与工作流同形）。扩展页选中一家，行下就亮出「绑键 cmd+t 没生效：
   新建标签页已经是它的键位」这一批，按 `source` 对回是哪一家，不重判撞车——两处判
   输家迟早答出两个输家。

**时机与刷新**：`open_extensions_picker` 一次重取三样——`extensions_report()`、键表
（`keymap_from`）、坏清单。与 `palette.open` 同一个「用户主动刷新」的时机、同一份 IO；
e2e 特意把 fixture 种在**窗口建好之后**，让「面板打开时重取键表」成为被测的那一环。

**界面语义**：坏清单行排在能用的之后，标「（加载失败）<目录名>」，原因直接摊在行下——
它没有可启停的状态、贡献不出任何东西，唯一有用的信息就是为什么没用上，所以**不用选中**，
也没有胶囊。

**验收**：mo-app lib 64（+1：三类坏法——JSON 坏、`validate` 拒、无 manifest.json 的普通
目录不算坏——后者的「安静跳过」语义不变）/ mo-ui lib 146（+2：键表把三条被拒的都记账
并带原因与身份；headless 端到端钉住「坏行亮出来 + 原因行在 + 没有胶囊 + 撞键那句亮在
自己那家下面」）。全量 `cargo test --workspace --all-features --no-fail-fast` →
`TEST_EXIT=0`，548 条 `test … ok`。

**反向验证**（四条变异体，每条红在预判的那一句；还原都是写文件，`diff` 验字节一致）：

| 变异 | 红在哪 |
|---|---|
| M1 `load_report` 的 validate 分支不记账（只 `continue`） | mo-app 单测 `left: 1 ≠ right: 2`；端到端「加载失败的清单没在扩展页出现」——**两层各红一头** |
| M2 `push_chord` 退回只 warn 不记录 | 键表单测 `left: 0 ≠ right: 3`；端到端「键位撞车的『没生效』没亮在这一家下面」 |
| M3 面板打开时不重取键表 | 只有端到端红（fixture 种在窗口建好之后，键表里根本没有那笔账）——键表单测对它全绿 |
| M4 面板打开时把 broken 列表扔掉（接线断） | 只有端到端红在坏行选择器；mo-app 单测照绿——数据层与接线两层各有一条红 |

已知缺口（下一轮别当意外）：
* `type_labels` 那条撞车（两个扩展抢同一个扩展名，先到先得）**仍是 warn-only**——它在
  `AppState::type_labels()` 的缓存里发生，不在键表里，要亮出来得给类型表也开一条报告
  通道；本轮没动它。（→ ✅ 2026-09-28 已做，见 §4.13：缓存里带撞车记录，输家的展开区
  亮「被抢先认领」。）
* 设置「快捷键」页仍然只列 `BINDINGS`，贡献键位（无论生效与否）在那页都看不见——
  P2 收尾欠的最后一块，与 §4.10 的缺口同一条。（→ ✅ 2026-09-28 已做，见 §4.13。）
* 用户自己 `commands/*.json` 里写坏的键串同样只 warn 不亮——`dropped()` 记了账，但
  扩展页按 `source` 过滤后它们（source 是 commands 目录下的文件）没有归属的行。要亮
  得给设置页加一块，本轮不扩面。（→ ✅ 2026-09-28 已做，见 §4.13：设置页不挑来源，
  `commands/*.json` 的坏键串在那里亮。）

### 4.12 安装流程：从磁盘装一个扩展 —— ✅ 已落地（2026-09-27，P2-8）

§4.10 的授权点停在「启用」，根因是没有安装流程——手放进 `extensions/` 的清单缺省
就是启用的。这一轮把**正门**修出来，门禁装在正门上：

1. **原生目录选择框**：`mo_platform::pick_folder(title)`。Windows 走
   `IFileOpenDialog` + `FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM`（coclass 的 CLSID 照
   `CLSID_FILE_OPERATION` 的先例自己钉）；macOS 走 `NSOpenPanel` 只选目录、
   `on_main_thread` 派发。**取消不是错误**：Windows `Show` 回
   `HRESULT_FROM_WIN32(ERROR_CANCELLED)`（`0x8007_04C7`）、macOS `runModal` 回 0，
   都折成 `Ok(None)`。来源必须由用户在对话框里当面指认——没有命令行参数、没有配置
   入口，那等于给陌生扩展开一条静默安装的门。
2. **`extensions::install_from(source, root)`**：读来源清单 → `validate` → 拒收
   （已存在同名 id；来源就在扩展目录里——它已经是装好的；符号链接——链接指到哪
   只有来源机器知道）→ `copy_tree` 复制进 `<root>/<id>/`（失败即清掉半个目录）→
   **改写清单为 `enabled: false`** → 写 `installed.json`：`{source, files:[{path,
   sha256}]}`，逐文件记**相对路径**（统一 `/` 分隔，子目录不与顶层撞名）与 sha256，
   不含账本自己。中途任何一步失败都不留东西。
3. **UI**：扩展页底部「从磁盘安装…」（`mo-ext-install`）→ 选完目录装、扩展与键表
   一起重取（与 `open_extensions_picker` 同一份账——装进来的键位撞了车，「没生效」
   当场就亮）、选中新行。

**安装即停用**是这一轮的安全落点：装出来的扩展当场以停用状态出现在面板上，点「启用」
走 §4.10 那张贡献确认卡。「装了就跑」从「唯一路径」收缩为「手工摆放一条」——正门有
门禁，剩下那条等卸载 / 迁移时收。

**缝拆两半**：`pick_folder` 在 headless 里弹不出来，所以按钮的 `on_click` 只做
「弹框 + 分派」，装完之后的后半段（`install_extension_from`）单独一个方法、e2e 直接
调它——盘上状态、行渲染、选中、停用都断言得到；「按钮真的会弹框」这半段 headless
测不了，本轮没有人工验证路径，记缺口。

**一枚测试自己教的事**：`notice()` 是**模态** Info，第一版「装完弹一句『已安装』」
直接把扩展页盖掉了——装完正该看的就是这一行，被弹窗顶走等于倒退。成功不弹，失败才
弹（错误原句经 `notice` 给出）。

**验收**：mo-app lib 66（+2：装出停用 + 账本来源与逐文件 sha256；四类拒收且不剩半个
目录、不弄坏已装好的）/ mo-ui lib 147（+1：装完当场有行、有胶囊、贡献清单展开、
模型与盘上都是停用、账本在）。全量 `cargo test --workspace --all-features
--no-fail-fast` → `TEST_EXIT=0`。

**反向验证**（四条变异体，每条红在预判的那一句；还原 `diff` 字节一致）：

| 变异 | 红在哪 |
|---|---|
| M1 `install_from` 不改写 `enabled`（原样 `true`） | mo-app 单测红在 `assert!(!m.enabled, "安装即停用：启用要走确认卡")`——安全语义那一句 |
| M2 跳过 `installed.json` 写盘 | 单测红在账本读取的 `unwrap` |
| M3 `install_from` 跳过 `validate` | `refuses_to_install` 红在「validate 不过要拒」，且坏 id 真被装了进去 |
| M4 `install_extension_from` 不重取扩展 | 只有端到端红在 `mo-ext-row-p28ui`——数据层与接线层各有一条红 |

⚠️ M2 的第一版变异写成 `if source…is_empty() { return }`——条件恒假，测试照绿。
**变异体自己先死了不算数**：它没走到要保护的代码路径，红的缺席什么都证明不了。
写变异体先看一眼它真的改变了被执行的语句。

已知缺口（下一轮别当意外）：
* **`.moext`（zip）入口 ✅ 已落（2026-09-28）**：新增 `mo_platform::pick_file`（Windows
  `IFileOpenDialog` 文件模式**不**带 `FOS_PICKFOLDERS`、macOS `NSOpenPanel` 只选文件），
  `extensions::install_from_archive` 解压到临时目录 → 定位扩展目录（清单在解压根、或根下
  恰好一个子目录里）→ 复用 `install_from`（复制 + 写回停用 + 账本）→ 装完清临时目录不留半截；
  扩展页加「从 .moext 安装…」（`mo-ext-install-moext`）。两种压包布局都认、zip-slip 与符号
  链接都拒、压包里找不到清单整包拒掉。`mo-app` +3 单测（`installs_from_archive_unzips_then_disables`
  / `installs_from_archive_with_manifest_at_root` / `refuses_archive_without_manifest`）、
  `mo-ui` +1 headless（装完当场有行、停用、账本在）。
* **手工摆放仍缺省启用**：`Manifest.enabled` 缺省 `true` 的语义不能动（手写清单的
  人不欠一次确认卡），缺口收口靠卸载 / 迁移语义，不在加载侧。
* **卸载 ✅ 已落（2026-09-28，§10 #3 的另一半）**：`extensions::uninstall_extension`
  （删 `root/<id>` 整目录，id 先过 `valid_id` 再拼路径——`..` 的点不在合法字符集里，
  路径穿越无门）+ `AppState::uninstall_extension`；扩展页选中行展开区加「卸载…」按钮
  （`ext-uninstall-<id>`），先过确认卡 `Modal::ConfirmUninstallExt`（载荷带 id 不带下标，
  红色确认键，与清空回收站同级的破坏性动作）——Esc / 取消 / 点遮罩都回扩展页，点了
  「卸载」才删盘、删完面板当场重取、选中行夹回合法区间。缓存按目录指纹自动作废，无需
  手动清。`mo-app` +2 单测（删目录 / id 穿越拒收）、`mo-ui` +1 headless（弹卡 → 不点头
  不动盘 → Esc 回页 → 确认后目录消失、行当场没了）。
* macOS 侧 `pick_folder` 是照本仓 objc 惯例写的，本机（Windows）没法跑；CI 编过
  即算过，真机行为待验。

### 4.13 P2 收口：贡献键位进设置页 + 类型标签撞车可见 —— ✅ 已落地（2026-09-28，P2 剩余 #1 / #2）

§10 台账的 #1 / #2 两条一起收掉。两件事的形状都是「判据早就在、就差一截接线」：

1. **贡献键位进设置「快捷键」页**（§4.8 / §4.10 / §4.11 三处共同记的那块）：
   * 数据层是 `Keymap::contributed()`——表里所有**非内置**条目（配置命令 / 扩展命令 /
     工作流），设置页单列一块画「动作名 + 键位」；**被拒的**（`dropped()`：坏键串 /
     撞车）挨着列出「没生效 + 原因」。⚠️ 设置页**不挑来源**：`commands/*.json` 里写坏
     的键串在扩展页按 source 过滤后没有归属行（§4.11 记的缺口），这里是它们唯一能
     被看见的地方。
   * **`open_keys_picker` 现在重取键表**（`keymap_from`，与 `open_extensions_picker`
     同一个时机与同一份 IO）。之前这页读的是启动时 / 上次开面板的旧账——P2-4 那句
     「清单改完要开一次命令面板键表才重取」的口径从此多了第二个刷新点。
   * 页面上的块标题写明「改绑请在命令面板或扩展清单里做」：贡献键位在这里**只读**，
     「恢复默认」「捕获新键位」那套交互仍只对内置条目生效（`rebind` / `unbind` /
     `reset` 只认 `Chord::Builtin`，这条纪律没动）。
2. **类型标签撞车亮在输家那一家下面**（§4.11 的另一条缺口）：
   * `extensions::type_labels_report` 返回 `(表, Vec<TypeLabelConflict>)`，撞车记录与
     表**共用同一份签名缓存**（`AppState::type_labels` / `type_label_conflicts` 同一把
     锁同一份 `Some((签名, 表, conflicts))`）——打开扩展面板时重取一次就够，渲染热路径
     仍只拿表那一半（`type_labels()` 投影）。
   * 扩展页按 `loser` 归并：句子是「类型标签「.xxx」被扩展「<赢家 id>」抢先认领，这一条
     不生效」，画在输家的展开区。**赢家不被告知**——它的标签生效着，输了才需要知道。
   * 「不重判」纪律照旧：什么算撞车、谁先到，判据只在 `type_labels_report` 一处；
     UI 只按扩展 id 对回是哪家。

**验收**：`mo-ui` lib 154（+2 条 headless 端到端：
`settings_keys_page_lists_contributed_and_dropped_chords`——fixture 在窗口建好**之后**
种「清单一条合法 key + 一条坏 key」与「`commands/*.json` 一条坏 key」，打开设置键位页，
模型侧断言贡献表进表、两条坏账都在，渲染侧按模型侧钉住的下标找行；
`type_label_conflicts_show_under_the_loser_extension`——种 `p29a` / `p29b` 两家抢
`.p29z`，断言缓存里一笔账且赢家按目录名字典序、输家展开区有那句话、赢家没有）。
全量 `./scripts/run-ci.sh` 全绿。

**反向验证**（四条变异体，每条红在预判的那一句；还原后 grep `MUTATION` 计 0）：

| 变异 | 红在哪 |
|---|---|
| M1 `open_keys_picker` 不重取键表 | 模型侧：「生效的贡献键位应当进表（open_keys_picker 应当重取键表）」 |
| M2 `keys_body` 把 `contributed()` 换成空表 | 渲染侧：「贡献键位那行没画出来（mo-keys-contrib-0）」（模型侧先绿，两层分开） |
| M3 `type_label_conflict_lines` 返回空 | 渲染侧：「输家那家的展开区没有『被抢先认领』这句」（模型侧先绿） |
| M4 `AppState::type_label_conflicts` 交空账 | 模型侧：「撞车没有被记进缓存」 |

⚠️ **第一版测试自己教的事**：fixture 一开始种在窗口创建**之前**，`RootView::new` 建键表
时盘上已经有了贡献项——于是 M1 首跑**照绿**（重取断了根本测不出来，启动路径替它兜住了）。
把 fixture 挪到窗口建好之后才让 M1 红在预判那句。**教训与 §4.11 的 M3 同一条**：
「用户主动刷新」这类接线的测试，必须先制造一个「数据在接线之后才存在」的窗口，否则
启动路径永远替断掉的接线兜底。

已知缺口（下一轮别当意外）：
* `keys-contrib-N` / `keys-drop-N` 是**位置号**选择器。渲染循环与 `contributed()` /
  `dropped()` 同序，测试先在模型侧钉住下标再按号找行，并行污染了账的长度也不会指错；
  但「按名字派生」那条纪律（§4.9 侧栏、§4.10 扩展行）在这里没贯彻——键位没有稳定的
  id 可用（命令可重名，展示名会撞），位置号 + 模型侧钉桩是当下的取舍。
* 设置页的块标题说「改绑请在命令面板或扩展清单里做」，但命令面板其实没有改键入口——
  贡献键位想改只能改清单。文案在说真话与留口子之间选了后者，真要做改键得先给
  `Chord::Command` / `Workflow` 一条可回写的路径（改谁的清单、怎么定位那一行）。
* 撞车句子报的是**扩展 id**（`p29a`），不是展示名——展示名会重名，id 不会；对作者来说
  清单里写的就是 id，能对上。要更友好得在句子里带上两边展示名，本轮没做。
* `type_labels` 那份缓存只在**重绘时**检查签名（§4.7 的老缺口原样适用）：列表停着不动
  时改了清单，撞车句也要等下一次重绘才出现。

### 4.14 P3：provider 宿主落地 —— ✅ 已落地（2026-09-28）

§5 的协议与 §4.3 的宿主照单全收，形状上补齐了三处草案没写细的地方：

* **清单字段**：`provider`（`run` argv / `methods` / 两个超时）与 `capabilities` 进了
  `Manifest`，`validate` 拒四种写坏法——capabilities 没有 provider（没有执行点 = 永远
  没人读）、methods 写了 `list`（P4 还没实现，`when_ext` + 侧栏同一条理由）、超时写 0
  / 超 60 秒、认不出的能力。贡献表新增 `Contribution::Provider`（确认卡上那句
  「进程：接管 …；申请能力：读文件名、读文件内容（头 4 KiB）」），`read-names` 缺省
  给、不用写也会出现在能力清单里。
* **协议**：JSON Lines，`{id, method, params}` / `{id, result|error}`；握手
  `initialize{protocol:1}` → `{name, version, methods[]}`，握手帧占 id 空间顶端
  （u64::MAX 往下数），调用帧从 0 递增——迟到的握手应答**按构造**不会撞成某次调用的
  回答。插件健康地拒答（error 帧）不计失败；超时 / 崩溃 kill 进程、记连续失败，
  3 次进指数退避（5s 起步翻倍、封顶 5 分钟），扩展管理器亮
  「provider 已停用 · 约 N 秒后自动恢复；日志：<host.log>」。stderr 全量落
  `<缓存>/mo/plugin-logs/<id>/host.log`——插件 → 宿主的唯一通道。
* **capability 执行点在派发侧**：`maybe_head` 授了 `read-contents` 才打开文件读头
  4 KiB；没授权时连打开都不发生。测试钉的是「读没读」（变异体去掉授权判断即红），
  不是「字段空不空」。
* **classify 只收 `label`**：协议答 `group` / `icon_key` / `columns` 收下不用——
  界面今天只有「种类」列一个消费点（`kind_label` 的 provider 优先级：逐文件答案
  压过清单静态文案，`ProviderRowProbe` 钉）。`preview` 走单一漏斗
  `AppState::preview`：认领类型先问插件，`text/markdown/json/code` → 对应
  PreviewKind，`image-file` 验「文件真的在」再交给图片渲染器，`rows` / `unsupported`
  / 任何失败**回落内置**（§9 第 3 条的「内置预览照常」就是这条兜底）。
* **缓存**：`<缓存>/mo/plugin-classify.sqlite`（mo-cache 新增 `ClassifyCache`），
  行键 `(扩展 id, 路径)`、行内带写入当时的 mtime + size（与缩略图同一套失效逻辑）；
  内存表 + sqlite 双层，卸载（`forget_ext`）把宿主、内存行、sqlite 行一起清——
  §6 的卸载语义就此闭合。
* **渲染路径纪律**：归属表快照每帧取一次（与 `type_labels` 每帧那次同价），行内
  查表零 IO；真活（起进程 / 问插件 / 落缓存）全在 blocking 池，
  `request_classify` 与缩略图同一条「每帧调也安全」的 fire-and-forget 形状。

**验收**：`src/bin/p3_provider.rs`（ok / hang / crash / garbage / refuse 五种模式）
当 §8 要求的 example 夹具；`tests/provider_host.rs` 7 条钉协议行为——
**「provider 卡死 / 崩了，UI 不受影响」落在**：hang 超时 kill（pgrep 钉「进程真死了」——
行为上两次都是 Timeout，只有进程表分得出 kill 与否）、crash 连续 3 次进退避后第 4 次
立刻短路、garbage 行跳过不炸、起不来的 argv 同一条退避账。反向验证五条变异体各红在
预判句：渲染无视 provider 答案 / 去掉 read-contents 门 / 超时不 kill（pgrep 抓到孤儿
进程）/ 去掉退避短路 / 卸载不清账。

## 5. provider 协议（stdio）—— ✅ 已落地（2026-09-28，P3，见 §4.14；`list` 方法属 P4 未收）

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

> **P2-8 落了安装的主干**（§4.12）：原生目录选择框 + `validate` + 复制 +
> `installed.json`（来源与逐文件 sha256），且**安装即停用**——装出来的扩展首启必过
> §4.6 那张确认卡。`capabilities` 已随 P3 落地（2026-09-28，§4.14：validate 收口 +
> `read-contents` 的执行点 + 确认卡逐条亮）。手工摆放的清单仍缺省启用——`enabled`
> 缺省语义不能动，收口靠卸载 / 迁移。

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
  * **P2-4 ✅（2026-09-27）**：清单的 `key` 决定键表，扩展命令能用组合键触发（§4.8）。
    §8 那句验收标准里的第三半句（一个 fixture 扩展贡献一项能**按出来**）自此成立。
  * **P2-5 ✅（2026-09-27）**：清单的 `menu: ["sidebar"]` 决定侧栏，扩展能加一个侧栏项，
    点它真的执行那条命令（§4.9）。至此 §3 那四类字段全部落地（`types` 只落 `label`、
    `menu`/`keybindings`/`sidebar` 三类都收进了命令自带的字段），§8 那句验收标准的最后一
    半句（一个侧栏项、headless 断言渲染出现）也成立了。
  * 剩下：安装与权限框、扩展管理器展示「本扩展贡献了什么」（含贡献进来的键位在设置里
    看不见那一条）。四类声明字段的缺口见各节：`types` 的 `group`/`icon`（§4.7）、侧栏的
    `icon`/`section`（§4.9）、坏声明在界面上无处可查（§4.6/§4.8/§4.9 同一条）。
  * **P2-6 ✅（2026-09-27）**：扩展管理器摊开「这家要往界面里放什么」+ 启用前确认卡（§4.10）。
    §8 那句验收标准里的「扩展管理器展示」自此成立；「安装与权限框」只剩后一半（安装流程）。
    顺带把「点了就跑、没有任何提示」那条债还了一半：**手动启用**必经确认，但**手放进目录**
    仍然装了就跑（缺省 `enabled: true`），闭合它要等安装流程。
  * P2 剩下：安装流程（原生目录选择框 / zip / sha256 / `installed.json`）、贡献键位在
    设置「快捷键」页可见、`type_labels` 撞车那条 warn-only（§4.11 的缺口）。
  * **P2-7 ✅（2026-09-27）**：坏声明可见——加载失败的清单带着 `validate` 的报错原句
    亮在扩展页上，贡献键位撞车 / 坏键串的「没生效」亮在自己那一家下面（§4.11）。
    §4.6 / §4.8 / §4.9 三节共同记的那条「同一个未接的口子」就此闭合；设置「快捷键」页
    看不见贡献键位那条**不在其中**，仍是开口子。
  * **P2-8 ✅（2026-09-27）**：安装流程主干（§4.12）——原生目录选择框、`validate`、
    复制进 `extensions/<id>/`、`installed.json` 记来源与逐文件 sha256，且**安装即停用**
    （首启必过确认卡）。P2-6 那条「手放进目录仍然装了就跑」的债就此从「唯一路径」
    收缩为「手工摆放一条」。`.moext`（zip）入口与卸载仍是缺口。
  * P2 剩下：~~贡献键位在设置「快捷键」页可见、`type_labels` 撞车那条 warn-only~~
    （✅ 2026-09-28，见 §4.13）、~~安装的收尾（zip 入口 / 卸载）~~（✅ 见 §4.12）。
    **P2 至此全部落地。**
* **P3 provider 协议 ✅（2026-09-28，见 §4.14）**：进程监管 + `classify`/`preview` +
  缓存 + 超时 kill 全部落地（capabilities 一并收口）。验收达标：`p3_provider`
  五模式夹具 + `tests/provider_host.rs`，「provider 卡死 / 崩了，UI 不受影响」
  由 hang/crash 两条确定性测试钉死。
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

## 10. 未完成事项台账（2026-09-28 收口时点，各处缺口的总账）

> 各节的「已知缺口」散在 §4.x 里，§8 只记「剩下」两个字。这里把**还没做的**集中列一遍，
> 每条带出处；做完一条划一条，别让「缺口」变成只写不读的装饰。

**P2 剩余（下一轮候选，按建议优先序）：**

1. **贡献键位在设置「快捷键」页可见** —— ✅ 已落（2026-09-28，§4.13）：设置页单列一块
   列出生效的贡献键位与被拒原因（含 `commands/*.json` 写坏的那批），`open_keys_picker`
   打开时重取键表；只读展示，改绑仍去清单。
2. **`type_labels` 撞车 warn-only** —— ✅ 已落（2026-09-28，§4.13）：撞车记录与类型表
   共用一份签名缓存，输家的展开区亮「被抢先认领」，赢家不被告知。
3. **安装收尾**（§4.12 缺口）：`.moext`（zip）入口 —— ✅ 已落（2026-09-28，见 §4.12 已知缺口）；
   **卸载** —— ✅ 已落（2026-09-28：删目录 + 确认卡 + 面板重取，见 §4.12 已知缺口；
   §6 说的「清 classify / preview 缓存行」—— ✅ 随 P3 落（2026-09-28，§4.14）：
   卸载经 `Manager::forget_ext` 清宿主、内存表与 `plugin-classify.sqlite` 的行）。
4. **`capabilities` 不收不查**（§4.10 的既定取舍）—— ✅ 已落（2026-09-28，P3，§4.14）：
   四把钥匙进了 `validate`（认不出 / 重复 / 无 provider 的组合都拒），`read-contents`
   的执行点在 `maybe_head`（授了才读头 4 KiB），确认卡逐条亮能力。

**验证欠账（headless 够不着的那半条缝）：**

5. **macOS `pick_folder` 真机行为**（§4.12）：本机是 Windows，只保证 CI 编过；NSOpenPanel
   那段 objc 是照本仓惯例写的，没在真机点过一次。
6. **「从磁盘安装」按钮的前半段**（§4.12）：按钮 → `pick_folder` → 装这条链在 headless 里
   只能直接调后半段，按钮真的会弹框这件事没验过（headless 弹不出原生对话框）。

**P3 / P4（排期主体，§5 是实现依据）：**

7. **P3 provider 协议** —— ✅ 已落（2026-09-28，§4.14）：进程监管（握手 / 超时 kill /
   退避停用 / 空闲回收 / shutdown）+ `classify`（sqlite 缓存，mtime+size 失效）+
   `preview`（单一漏斗回落内置）+ capabilities 收口。验收：`p3_provider` 夹具 +
   `tests/provider_host.rs`，「provider 卡死 / 崩了，UI 不受影响」由
   hang/crash 两条确定性测试钉死；反向验证五条变异体各红预判句。
8. **P4 `list` 列表源**：单独一轮，§9 的三点风险就是为它留档的。

**外围欠账（不属于插件系统，别在这一页修）：**

9. **任务 #24（P1-1 补强）**：
   * `mo_core::types` 一问一答的守卫断言 ✅（2026-09-28）：三问的形状 / 跨轴矛盾（`.svg`、
     `.avif`）/ 分组四族互斥 / 小写判据都在 `crates/mo-core/src/types.rs` 的 `tests` 里；
     `mo-ui/file_item::kind_by_ext`（种类文案那张内置表，本轮**不**搬进 `label_of`，见下）与
     `mo_core::types::group_of`（分组那问）的自相矛盾也钉住了
     （`kind_label_agrees_with_group_of_and_is_stable`）——正是收口类型知识要消灭的「分组叫图片、
     种类列却叫别的」那一类显示。
   * tiff 缩略图假阳性 —— **已修（2026-09-28）**：根因是「派不派取图任务」
     （`entry::supports_thumbnail`）与「解码器解不解」（`mo_thumbnails::generate_to_with`）两处各说各话，
     `preview_of` 把 `tiff` 当图片认、解码闸却只放六种格式。修法是把唯一判据收进
     `mo_core::types::THUMBNAIL_DECODABLE_EXTS` + `supports_thumbnail_ext`，
     两处都引用它（不是把 tiff 从类型表删掉——那会连分组/预览一起改判）。
     `mo-core` +2 测试、mo-thumbnails 13 全过。见 `devlog/engine-testing.md` §7。
   * 「种类」文案收口 `mo_core::types::label_of` —— **按用户决策不做**：`kind_by_ext` 那张独立表是
     刻意的（P2-3 起），搬进去是大重构，且与 `group_of` 的关系已由上面的守卫钉住，先不迁。
10. **手工摆放的清单仍缺省启用**：正门（安装）与退路（卸载）都已闭合；`enabled` 缺省值
    仍不动（手写清单的人不欠一次确认卡）——这是定下的语义，不是待办。
11. **右键空白处吃选区的 `when_ext`**（§4.6 记了）：语义等有真实插件再定。
12. macOS 拖出 / 文件剪贴板写、地址栏不认 `/`、TEMP 里没人删的测试目录、§22 macOS US 键表、
    Linux 整体未验——都记在 `devlog/windows-port.md` 文末那份待办里，这里不重复。
