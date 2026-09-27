# 插件系统（扩展包 / 能力分层 / provider 协议）

> **状态：设计稿，未实现。** 本篇不遵守 devlog「只记已验证结论」的约定（见
> [README.md](README.md)），记的是**将要做的**东西与其依据。实现进度一律落在
> [windows-port.md](windows-port.md) 那样的逐节编年里，别把这里的 P1~P4 当成已完成。
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
| 右键菜单 | `enum MenuAction`（`pub(crate)`）约 30 变体，**无** `User(_)` 逃生口；条目 `push` 出来 | `mo-ui/src/context_menu.rs:71`、`items()` :176 |
| 侧边栏一项 | 无数据模型，六段手写 div | `mo-ui/src/sidebar.rs:16`（回收站那段 :137-179） |
| 新面板 | `enum Modal` 约 25 态 + render 手写三档 match | `app.rs:174`、`6739` |
| 类型知识 | 三份各自独立的扩展名字符串匹配 | `mo-preview/src/lib.rs:196-217`（`is_image`/`is_pdf`/`kind_by_ext`）、`mo-core/src/view.rs:119`（`kind_group_of`）、`mo-app/src/icon.rs:133-165`（`icon_key`/`is_package_ext`） |
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

### 4.1 `TypeRegistry`（mo-core）

今天「`.png` 是什么」这件事在三个 crate 各写一份字符串匹配，会互相矛盾（列表分组说是文档、
预览说是纯文本）。收成一张表：

```rust
pub struct TypeRule { pub ext: &'static [&'static str], pub group: GroupKey,
                      pub icon_key: IconKey, pub preview: PreviewClass,
                      pub label: Option<&'static str>, pub is_package: bool }
pub fn rule_for(ext: &str) -> Option<&TypeRule>;   // 内置打底 + 插件贡献，插件**在前**
```

插件规则排在内置**之前**（作者更懂自己的格式，允许覆盖）。三处调用点改为查表：
`mo-preview/src/lib.rs:196-217`、`mo-core/src/view.rs:119`、`mo-app/src/icon.rs:133-165`。

⚠️ 这一步必须**逐条保持现有答案不变**，且要靠既有测试钉住（icon / preview / view 三组
都已有断言）；改完加一条一致性测试：同一扩展名在三处得到的答案出自同一条 rule。

### 4.2 `ActionRegistry`

```rust
pub struct ActionSpec { pub id: String, pub label: String, pub category: String,
                        pub where_: Vec<Slot>,           // context:file / context:blank / palette / toolbar
                        pub when_ext: Vec<String>,
                        pub kind: ActionKind }           // Builtin(&'static str) | Command(UserCommand) | ProviderCall{method}
```

落地点三处：

* `commands_in()`（app.rs:418，命令面板数据源）改成读 registry，而不是硬编码函数；
* `MenuAction` 加逃生口 `Plugin(usize)`（对齐 `CommandId::User(usize)` 已有的做法），
  `items()` 尾部追加 registry 给的项，`run_menu_action`（app.rs:6285）加一臂；
* `sidebar.rs` 从手写 div 改成遍历 `Vec<SidebarItem>`——「新增一个侧栏项」从抄 40 行 div
  变成加一行数据。

### 4.3 `ProviderHost`

起进程、握手、按调用超时、退避停用、结果缓存。UI 侧只在后台线程调用（沿用 mo-app 现有
的泵模式），**任何 `block_on` 都不许出现在渲染路径上**。

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

* **P1 纯宿主重构，零插件概念**：`TypeRegistry` 收三处扩展名表；`ActionRegistry` 收命令面板 +
  右键菜单（加 `MenuAction::Plugin`）；侧栏改数据驱动。
  验收：全量测试绿 + 新增「三处类型答案出自同一条 rule」的一致性测试。
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
