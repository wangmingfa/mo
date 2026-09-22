# GPUI 布局与交互的坑

环境：gpui-kit 0.6.1（gpui 0.3.5 + gpui-base + gpui-component + gpui-platform）。日期：2026-09-17。

---

## 1. `flex_row()` / `flex_col()` 不会设置 `display: flex`（空白界面）

* **现象**：界面一片空白，工具栏竖着堆叠、中央区高度为 0。
* **根因**：gpui 0.3.5 中 `flex_row()` / `flex_col()` **只设置 flex-direction**，`Style::default().display` 仍是 `Block`。26 处布局全部因此失效。
* **修法**：每个用了 flex 方向的容器都要显式加 `.flex()`。没有 `h_flex()` / `v_flex()` 辅助函数。
* **守卫**：`crates/mo-ui/tests/layout.rs` 用 headless 布局探针锁定关键区域的几何关系。

## 2. `uniform_list` 高度塌成 0

* **现象**：文件列表不渲染。
* **根因**：列表项只在 prepaint 阶段渲染，布局阶段 taffy 看到的是「没有子节点」，高度算出 0。
* **修法**：`uniform_list(...)` 之后必须显式 `.flex_1()` / `.size_full()` / `.h(...)`。

## 3. 可点击元素必须 `.id(...)`——否则 on_click 永远不触发（最大坑）

* **现象**：双击文件夹、工具栏按钮、面包屑、侧边栏书签……点击全部无反应，但编译零告警。
* **根因**：gpui 的 click 事件注册整段包在 `if let Some(element_state)` 里（div.rs）。无元素 ID 的裸 `div()` 拿不到 `InteractiveElementState`，鼠标事件监听器**从未注册**。
* **修法**：可点击元素一律 `.id(...)`：
  * 列表行：`.id(("file-row", i))`——用**全列表绝对索引**，滚动后 ID 稳定；
  * 静态按钮：`.id("nav-back")` 等唯一名字。
  * `uniform_list` 本身只有整体 ID，**不会**给每行推 per-item 命名空间。
* **排查口诀**：`grep -rn "on_click" src/`，每个调用点向上必须能找到 `.id(`。
* **连带影响**：加了 ID 的元素类型变为 `Stateful<Div>`，与占位行 `Div` 混装同一个 `Vec` 时统一 `into_any_element()`。

## 4. `hover()` 悬停样式同样依赖 element state

* **现象**：hover 高亮不生效。
* **根因**：hover 监听器状态也存在 `InteractiveElementState` 里，无 ID 的元素同样拿不到。
* **修法**：同上，补 `.id(...)` 即一并修复。

## 5. 双击判定用 `click_count()`

* **要点**：`on_click` 回调第一参数是 `&ClickEvent`，macOS 双击会发**两次 click**（count=1、count=2），在回调里判断 `ev.click_count() >= 2` 即双击。
* **注意**：第一次 click 已经 toggle 了选中，双击打开时选中会 toggle 两次（净效果不变），属预期。

## 6. macOS 快捷键的 key 名

* **要点**：macOS 上方向键的 `key` 是 `"up"` / `"down"`，**不是** `"arrowup"` / `"arrowdown"`。命令面板 / 模态里两种都要接受。

## 7. `#[gpui_kit::test]` 在 lib crate 内爆栈

* **现象**：`recursion limit reached while expanding #[test]` / SIGBUS。
* **根因**：该宏在 crate 内部展开会遮蔽内置 `#[test]`（尤其 `use gpui_kit::*` 把它的 `test` 宏一起带进来时）。
* **修法**：crate 内部用普通 `#[test]` + `TestAppContext::single()` + `VisualTestContext::from_window`；测试模块**不要** `use super::*`。

## 8. flex item 的自动最小尺寸会顶飞右侧列

* **现象**：长文件名把右侧大小列顶出行外（实测 712 > 行宽 596）。
* **根因**：flex item 的自动最小尺寸是 min-content（完整文件名宽度）。
* **修法**：内容列加 `.overflow_hidden()`（按 CSS 规则把自动最小尺寸降为 0），名称列再 `.truncate()`。
* **关联坑**：`truncate()` 是 Div（Styled）上的方法，不要挂在 Text 上；`gpui_kit::white()` 返回 `Hsla` 不是 `Rgba`。

## 9. 图标槽位不定宽 → 文件名左右抖动

* **现象**：打开含图片的文件夹，缩略图加载瞬间文件名水平跳动。
* **根因**：emoji 图标（约 18px）与缩略图（20px）自然宽度不同，行内元素按内容宽度收缩。
* **修法**：图标槽位定宽（`w(20).h(20).flex_shrink_0()` + 居中 + `overflow_hidden`），三种状态（emoji / 加载中 / 缩略图）都在同一槽位渲染。

## 10. 长生命周期订阅必须用 `WeakEntity`

* **现象**：窗口关闭后 `Exited with leaked handles`。
* **根因**：事件订阅闭包持有 `Entity<RootView>` 强引用，形成自持环。
* **修法**：订阅闭包捕获 `WeakEntity<RootView>`，回调内 `weak.update(cx, ...)`；`Context::spawn` 的闭包签名是双参数 `|weak, cx|`。

## 11. 窗口默认尺寸与居中

* **要点**：`WindowOptions.window_bounds` 不设的话 gpui 缺省 1000×600、且不居中。居中用现成的 `WindowBounds::centered(size(px, px), cx)`（platform.rs）。

## 12. 沉浸式标题栏（appears_transparent）

* **要点**：`TitlebarOptions { appears_transparent: true, traffic_light_position: Some(...) }` 隐藏系统标题栏；内容延伸到窗口顶。工具栏高度必须**钉死常量**（`TOOLBAR_HEIGHT = 48`），红绿灯位置由同一常量推导——否则内容高度浮动，红绿灯永远对不准。

## 13. `uniform_list` 的 items 闭包每帧被调用多次（含单行测量）——副作用会死循环

* **现象**：大目录滚动时列表在「整屏 `…` 占位」与「正常内容」之间疯狂闪烁，永不停止。日志（tracing）显示每帧两个 fetch 交替 spawn：`need=0..101 visible=0..1` 与 `need=102..333 visible=202..233`，各自 done 后互相覆盖 window，永不收敛。
* **根因**：gpui 的 `uniform_list` **每帧用单行 range 调用 items 闭包多次**来测量行高（`measure_item` 在 request_layout 与 prepaint 各调一次，渲染 `item_to_measure_index..+1` 即默认 `0..1`），之后再以真实可见区调用一次。若 items 闭包里有「判断窗口不覆盖 → spawn 补窗」这类副作用，测量调用发出的请求与真实请求范围不相交，两次 fetch 落地时**先后覆盖同一份 window**，下一帧谁都覆盖不了对方的需求 → ping-pong 死循环。
* **修法**：`range.len() <= 1` 时跳过所有补窗副作用（不设 pending、不 spawn），只渲染行。真实可见区至少两行，单行 range 只可能是测量调用。核心原则：**items 闭包必须是幂等渲染 + 无跨调用干扰的副作用**；确需副作用时先识别测量调用并跳过。
* **排查方法**：`RUST_LOG=mo_ui=debug cargo run 2> /tmp/mo.log` 跑一次复现，看 fetch spawn/done 的 need 范围是否每帧重复交替——是，即此坑。

## 14. gpui 0.3.5 没有原生拖放 API —— 拖拽只能自己用鼠标事件拼

* **现象**：想实现「跨窗格拖拽文件」，在 gpui 里找不到 `on_drop` / `on_file_drop` / `DropEvent`，`grep` 整个 `gpui-pre-0.3.5` 源码也没有。
* **根因**：0.3.5 的交互层只有鼠标 / 键盘事件，没有 drag-and-drop 抽象；系统级文件拖入（从 Finder 拖进来）同样无法接入。
* **修法**：用 `on_mouse_down` + `on_mouse_up` 自己拼一套**应用内拖拽**：
  * 行上按下 → `begin_drag()` 记录 `DragState { pane, tab, paths }`（行已选中且多选时拖整个选中集合，否则只拖这一行）；
  * 行上抬起 → `drop_on_entry()`：目标是目录就复制进去，原地按下抬起视为普通点击直接丢弃，跨窗格但落在非目录行上则**把 DragState 放回去**；
  * 窗格容器上抬起 → `drop_on_pane()`：跨窗格时落到该窗格的当前目录。
* **关键点**：事件是「内层 → 外层」冒泡，所以**行先于窗格**执行；行处理不了时必须把状态放回去，否则窗格级永远收不到。另外按住 ⌥ 抬起来切换「复制 / 移动」语义（`ev.modifiers.alt`）。
* **副作用提醒**：外部文件拖入（OS → 应用）在本代框架下做不了，README 里不要写「支持从 Finder 拖入」。

## 15. 鼠标单击选择语义修错了：普通点击应是「单选替换」而不是「追加」

* **现象**：不按修饰键单击多个文件，前面选中的不会被取消——全是累加。
* **根因**：`file_list.rs` / `grid.rs` 的 `on_click` 对**每一次**单击都调 `selection.toggle(id)`，从不判断修饰键。`toggle` 是「在现有选区上切换」，所以越点越多。
* **修法**：单击按修饰键分流，对齐 Finder / 资源管理器：
  * 无修饰 → `select(id)`（清空再单选）；
  * cmd/ctrl → `toggle(id)`（在选区上增删）；`ClickEvent` 跨平台主键是 `ev.modifiers().platform`（macOS=Cmd），再 OR 上 `ev.modifiers().control` 让 Windows/Linux 的 Ctrl 也生效；
  * shift → 从 `anchor` 连选到点击项：`clear()` + `select_range()` + `set_anchor()`（锚点要保留为起点，否则下一次 Shift 会以终点为基准）。
* **两个 gpui 0.3.5 的坑**：
  1. `ClickEvent` 没有 `modifiers` **字段**，只有 `modifiers()` **方法**（和 `MouseUpEvent.modifiers` 字段不是一回事，拖拽读的是后者）；
  2. `app.select_range(from,to)` **只 insert 不清空**，且 `from/to` 是 `dir.view.visible_indices()` 的全局可见下标——所以连选同步到 app 侧前要先 `clear_selection()`，否则 app 选区会累积、导致后续复制 / 移动选错文件。
* **架构提醒**：`sync_panel` 每 ~120ms 用 `app.selection_ids()` 回灌 `panel.selection`（`set_from`），app 才是最终事实来源。本地 `panel.selection` 的改动只是 ≤120ms 的即时反馈，真正的语义修正落在对 `app` 的 `select/toggle/select_range` 调用上。
* `SelectionModel` 增加 `set_anchor()`；新增 4 个回归测试（含 `select` 替换、`set_anchor` 不动选区、Shift 连选保留锚点）。

## 16. Windows 顶栏自绘窗口控制按钮（最小化 / 最大化 / 关闭）

* **现象**：Windows 下顶栏没有最小化 / 最大化 / 关闭三键。
* **根因**：`lib.rs` 里 `TitlebarOptions.appears_transparent: true` 是**无条件**设的，而 Windows 侧 `hide_title_bar = appears_transparent`（见 `gpui-pre-windows/window.rs`）——系统标题栏被隐藏了，却没有自绘的 CSD（客户区装饰），于是三键缺失。
* **修法**：三键各 `.id(..)` 后打 `.window_control_area(WindowControlArea::{Min,Max,Close})`。Windows 会把它映射成 `HTMINBUTTON/HTMAXBUTTON/HTCLOSE`，**点击、双击、Win11 贴边分屏吸附全交系统**，所以 Windows 上**不要**再挂 `on_click`（非客户区点击根本不进 GPUI 回调）。Linux 无对应命中区，用 `on_click` 调 `window.minimize_window()/zoom_window()/remove_window()` 兜底；macOS 走原生红绿灯，不渲染这组按钮。
* **顺带**：Windows 顶栏左内边距从为红绿灯预留的 80px 降到 12px；`is_maximized` 由 `RootView::render` 传入以在「最大化 / 还原」图标间切换（系统改状态会触发 resize → 重绘）。
* **踩过的弯路**：一开始还想给顶栏加 `.window_control_area(Drag)` 做拖拽。教训——命中测试回调 `for (area,hitbox) in window_control_hitboxes { if mouse_hit_test.ids.contains(hitbox.id) { return Some(area) } }`，而 `mouse_hit_test.ids` 含**祖先**命中框，所以 Drag 矩形一旦与任何可点击控件几何重叠，重叠处就一律判成标题栏、子控件收不到点击。Drag 区必须是与交互控件严格不重叠的纯空白条带；本例顶栏被地址栏 flex_1 铺满、没有安全空白，遂放弃拖拽，只保留三键。


## 16. 标签页条上移到窗口最顶端，与交通灯 / 窗口控制按钮同行（Win11 风格）

* **需求**：标签页要从地址栏下方移到最顶行，且 macOS 红绿灯 / Win·Linux 的最小化最大化关闭按钮要和标签页落在**同一行**。
* **关键约束**：macOS 红绿灯是 AppKit 画的，位置由 `toolbar::traffic_light_position()` 按 `TOOLBAR_HEIGHT = 48` 推导（`lib.rs` 里 `appears_transparent + traffic_light_position`）。要让红绿灯与标签页垂直居中，顶部那一行**必须正好 48px 且在窗口最顶端**——所以不能直接把标签条塞进原来的 48px 工具栏，而是新建一行 `render_top_row`（高度 `TOOLBAR_HEIGHT`）作为 root 的第一个 child。
* **结构变化**（`app.rs`）：
  * 新增 `render_top_row()`：macOS 左留 80px（给红绿灯）→ 每窗格一条 `render_tab_bar`（分栏时并排、各 `flex_1`）→ Win/Linux 追加 `drag_strip()` + `window_controls()`。
  * `toolbar::render()` 删掉 `is_maximized` 参数和红绿灯 80px 边距、窗口控制按钮，只留导航 + 地址栏 + 刷新 + 视图模式（第二行）。
  * `render_pane()` 去掉标签条参数（标签条不再嵌在窗格里），`render_tab_bar()` 高度改 `h_full()` 填满 48px 行，分栏时第二个窗格左侧加分隔线。
* **toolbar.rs**：`drag_strip()` / `window_controls()` 提到 `pub` 供顶部行复用；`control_button` 仍私有。
* **测试**：`toolbar_height_is_pinned_for_traffic_lights` 改断言 `mo-toprow`（y=0、高 48）；其余 layout 测试（地址栏在工具栏内、状态栏贴底、侧栏左置）不受影响，地址栏仍在 `mo-toolbar`（第二行）内。
* ⚠️ 真实窗口下 macOS 红绿灯对齐、Win/Linux 拖拽条命中区只过了 headless 布局测试，没在沙箱里点过——需本机 `cargo run` 验收。

## 17. 列表视图改版：Finder 式表头 + 斑马纹 + 四列布局

* **参考**：macOS Finder 列表视图截图——列头（名称/修改日期/大小/种类，右对齐固定宽列）、全宽蓝色选中、交替行底色。
* **结构**（`file_list.rs`）：
  * 新增 `header()`：26px 表头（`mo-file-list-header`），`container()` 底 + 底部分隔线、11px 灰字；列宽与数据行共用常量、左右内边距 16（=列表容器 12 + 行 4）对齐数据行内容起点；表头固定在滚动区上方，不随内容滚动。排序尚未接入（纯展示）。
  * `render()` 返回值改为「表头 + （relative 容器：uniform_list + Scrollbar overlay）」两层；滚动区高度因此比中央区矮一个表头。
  * 斑马纹：未选中行按全局奇偶交替 `zebra()`（新增主题色 0xf7f7f8），选中行仍 `selected_bg()` 蓝底白字。
* **数据行**（`file_item.rs`）改为四列：名称（flex_1 + truncate）| 修改日期 | 大小 | 种类（三列固定宽 `DATE_W=150`/`SIZE_W=80`/`KIND_W=100`、右对齐、12px 灰字/选中白字，共用 `meta_cell` 闭包；debug selector `mo-date-cell`/`mo-size-cell`/`mo-kind-cell`）。
  * 修改日期：`FileMetadata.modified: Option<SystemTime>` → chrono Local `%Y年%m月%d日 %H:%M`（mo-ui 新增 `chrono` 依赖，workspace 统一声明，`default-features=false` + `clock`）。未就绪留空、失败显示 —。
  * 种类：`kind_label()` 按扩展名归类（PNG/JPEG 图像、视频、音频、PDF 文稿、Word/Excel/PPT、Markdown/文本、归档、源代码、App/DMG 等，兜底「EXT 文件」/「文档」），与 icons.rs 的图标分类一致。
  * 加载占位从 ⏳ emoji 改为空白槽（延续去 emoji 方向）。
* **测试**：`size_column_is_pinned_to_the_right_edge` 重写为 `meta_columns_are_pinned_right_and_aligned`（种类列贴右缘、大小/日期列向左各隔 8px gap、列宽不被压缩）；layout 集成测试 `sidebar_sits_left_of_the_file_list` 改断言「表头+列表 = 中央区高度」。
* 坑：并行发的两个 Edit 第二个常不落盘（layout.rs 两处编辑只生效一处），需单独重发并回读验证。

## 18. 列表表头三件套：拖宽 / 拖序 / 点击排序

* **需求**：参考访达列表视图，表头支持 (1) 拖分隔条调列宽、(2) 拖列头调列序、(3) 点表头切排序。
* **列模型**（新模块 `list_columns.rs`，不依赖 GPUI，可直接单测）：
  * `ColId{Name,Date,Size,Kind}` → 表头文案 / 排序键 / 默认宽 / 是否弹性 / 对齐方式；
  * `ColumnLayout{order: Vec<ColId>, widths: [f32;4]}`：`set_width` 把宽度钳在 60..=420，`move_col(from,to)` 越界或原地都是 no-op；
  * `drop_index(centers, x)`：纯函数，按各列**中点**算拖放落点（把判定从渲染代码里抽出来就是为了可测）。
* **排序方向**（`mo-core`）：`SortKey` 之外新增 `SortDir{Asc,Desc}`，`SortDir::natural_for(key)` 给出各列的自然方向（名称/种类升序，大小/修改时间降序）。比较器改成「**目录恒在前** → 主键按升序比较 → 方向翻转 → 名称恒升序兜底」，避免降序时同值条目连次序键一起倒过来。`Directory::set_sort(key, dir)`、`AppState::set_sort(key, dir)` / 新增 `AppState::sort()` 供 UI 画箭头；命令面板的 4 个排序命令改为 `sort_via_command`（同列翻转、换列取自然方向）。
* **表头实现**（`file_list.rs::header`）：
  * ⚠️ 两层结构：`on_children_prepainted` 只在 `Div` 上有，而带 `.id()` 的元素会变成 `Stateful<Div>` —— 于是外层 Stateful 行挂鼠标事件、内层裸 Div 负责测量并回写 `RootView.header_cells`（列真实 bounds，名称列弹性所以拿不到宽度，只能靠回写）。
  * **点击 vs 拖列**：不依赖 `on_click`（它与 `on_mouse_up` 的派发先后不可靠），统一在表头行的 `on_mouse_up` 里结算——位移 ≤ 4px 当点击（切排序），否则按 `drop_index` 换列序；被拖动的那列常亮底色。
  * **调列宽**：分隔条是列头单元格的子节点（`absolute`、6px 宽、`ResizeLeftRight` 光标），内层先派发写入 `Resizing`，外层列头的 `on_mouse_down` 见已有拖拽态就不覆盖。
  * 弹性列（名称）不挂分隔条、不设 `min_w`，与数据行的收缩规则保持一致，窄窗格下才不会错位。
* **数据行**（`file_item.rs::view` 新增 `layout` 参数）：按 `layout.order` 拼列；图标 + 颜色标签 + 文件名打包进「名称列」，列怎么拖图标都跟着文件名走。`file_list::render` 用 `ListChrome{cols, sort, dragging}` 打包传参（8 个参数会撞 clippy `too_many_arguments`）。
* **测试**：`list_columns` 5 个单测（默认布局 / 钳制 / move_col no-op / 排序键映射 / 落点）；`mo-core` 新增 2 个（方向翻转且目录恒在前、自然方向）；`file_item` 新增 `columns_follow_layout_order`（把种类列拖到最前，行内顺序跟着变）。
* ⚠️ GPUI 0.3.5 没有鼠标事件模拟 API，三种拖动交互只有纯逻辑单测 + headless 布局测试，真实拖拽手感需本机 `cargo run` 验收。

## 19. 列表表头：可见的列分隔线（可拖调宽）

* **需求**：表头把每列的分隔线画出来，并让这条线本身成为调列宽的把手。
* **原来为什么看不见**：分隔条是 6px 宽的**透明**命中区（挂在列头**右缘**），既没有颜色，
  也没有「名称 | 修改日期」那一条（名称列是弹性列，右缘把手被跳过了）。
* **分隔线挂在「列的左缘」**（第一条不需要）：
  * 4 列 → 3 条线，`名称|修改日期` 这条也在；
  * 线本身常显（1px、`theme::divider()`=0xc8c8cc），外圈 7px 是命中区
    （`left = -(HEADER_GAP/2 + 7/2)`，即命中区以两列间隙的中线为对称轴）；
  * 拖动中：线加粗到 2px + 转 `muted()`，松开复原（`RootView::resizing_divider()`）。
* **调宽的方向语义**（`list_columns::divider_resize`，纯函数 + 单测）：
  * 规则是「**左列变宽 dx、右列变窄 dx**」——两侧同时动，被拖的那条线才严格跟着鼠标。
  * 只改一侧不行：布局里恒有一个弹性列吃剩余空间，固定宽列的**远侧**边被容器边缘钉死，
    单边改宽度只会让它不动的那条边挪位，线反而不动。
  * 两侧共用**同一个钳制后的位移**：各自独立钳制的话，一侧触上下限时另一侧还在动，线会偏。
  * 弹性列（名称）不能设固定宽 → 它那一侧交给邻居独自承担，线依然跟着鼠标
    （弹性列吃/吐剩余空间，边界正好由邻居宽度决定）。所以第一列是弹性列时，
    拖 `名称|修改日期` 的实际效果就是「名称变宽」。
  * `DividerAnchor`（按下瞬间的两侧宽度快照）+ 按**总位移**重算（不是逐帧增量）：
    增量叠加钳制会在触限后把线拖偏。
* **坑**：表头内层行原来没有 `h_full()`，`items_center` 让它的高度变成内容高（18px），
  于是分隔线只有 18px、撑不满表头 → 补 `h_full()` 后是 25px（表头 26px 减去 1px 底边框）。
* **测试**：`list_columns` 新增 4 个（锚点跳过首列/弹性列、对称改宽、单侧兜底、共用钳制）；
  `layout.rs` 新增 `header_dividers_sit_between_columns`——断言 3 条线都渲染出来、
  中心落在两列间隙的中线上、且贯穿表头高度（第一列左侧不许有）。
* ⚠️ 仍是 headless 布局测试 + 纯逻辑单测；真实拖拽手感（7px 命中区宽度是否好抓）需本机验收。

## 20. 表头分隔线的视觉微调：浅一档 + 不顶边

* **反馈**：19 条的分隔线「稍微灰一点」、上下不要顶边要留间距。
* **线的颜色**：`theme::divider()` 0xc8c8cc → **0xd6d6da**（浅一档）。表头底是 `container()`
  (0xf6f6f7)，分隔线同时是拖动把手——太深抢眼，只留克制的浅浅一条。
* **垂直内缩**：新增 `DIVIDER_H = 14.0`（表头 26px，上下各留 6px）。**只有看得见的线体**
  改成固定高 14px；**命中区（`mo-header-divider-*`，7px 宽）仍然 `h_full()`**——
  视觉变轻但「抓取」的命中面积不变，手感不受影响。
* **可测性**：线体单独挂 debug selector `mo-header-divider-line-{col}`（原来只能测命中区）。
* **测试**：`header_dividers_sit_between_columns` 扩展——除原有的「落在两列间隙中线、
  命中区贯穿表头高度」外，新增「线体上下 inset ≥ 1px（不顶边）」+「上下 inset 对称（垂直居中）」。
  坑：`TestWindowExt::debug_bounds` 只吃 `&'static str`，运行时拼的 String 会 `E0597`
  报生命周期不够 —— 选择器要写成字面量。

## 21. 地址栏换成框架的真实输入框（修「编辑时无法选中文本」）

* **根因**：地址栏的「编辑态」从来不是输入框，而是一段自绘文本——
  `toolbar.rs::address_bar` 在 editing 时渲染 `text!(format!("{input}▏"))`，
  光标是**拼在字符串末尾的字面量 `▏`**；按键由全局 `on_key_down` 里的
  `handle_address_key` 处理，只会「追加 1 个字符 / `pop()` 退格 / Enter / Esc」。
  没有 caret 下标、没有选区模型、没有鼠标事件 → 点选 / 拖选 / ⌘A 一概不存在，
  连「把光标点到中间」都做不到。
* **改为**接入 gpui-component 的 `Input` + `InputState`（已随 gpui-kit 默认
  feature 进依赖，`gpui_kit::init` 里已调过 `gpui_component::init`）：
  选区 / 光标 / 双击选词 / ⌘A / 剪切复制粘贴 / 撤销 / 中文输入法全部自带。
* **状态**（`panel.rs`）：`address_input: String` → `address: Option<Entity<InputState>>`
  + `address_sub: Option<Subscription>`。**一个面板一个输入状态**（分栏时两个窗格
  互不串味），首次进入编辑时懒创建——`Panel::new` 拿不到 `window`，
  而 `InputState::new(window, cx)` 需要它。⚠️ 订阅句柄**必须持有**：
  `Subscription` 一 drop 就退订，回车 / 失焦事件就再也收不到。
* **进入编辑**（`RootView::begin_address_edit(window, cx)`）：预填当前完整路径 →
  `focus` → `select_all`。全选是 Finder / Win11 的行为，直接敲字即可替换。
* **事件**（`cx.subscribe_in`）：`PressEnter` → `submit_address`（跳转 + 把焦点
  还给根视图，否则上下键 / 输入即过滤一直收不到）；`Blur` → `end_address_edit`。
* **按键链路**（关键）：gpui 的 `dispatch_key_event` 是**先派发 key binding 的
  action、再跑 bubble 阶段的 `on_key_down`**（`window.rs` 里 `match_result.bindings`
  循环先于 `finish_dispatch_key_event`）。而输入组件把退格 / 方向键 / ⌘A / ⌘C⌘X⌘V /
  ⌘Z / Enter 全注册成了 action，**在绑定阶段就被消费**，根本到不了全局监听器——
  所以不会「双份生效」。全局监听器里那段地址分支因此只剩两件事：Esc（输入组件
  的 `escape` 在 `clean_on_escape=false` 时会 `cx.propagate()` 放行）→ 退出编辑；
  其余一律 return 放行给输入组件（**必须整段 return**，否则「输入即过滤」
  和上下键导航会跟着一起触发）。删掉了 `handle_address_key`。
* **配色桥接**（`lib.rs::sync_component_theme`）：`Input` 的选区 / 光标 / 前景色
  全取自 gpui-component 的 `Theme` 全局，不覆盖就会在自绘的极简配色里冒出一套
  shadcn 默认色。用 `Theme::global_mut(cx)` 覆盖 selection（那抹蓝 + 30% 透明）、
  caret、foreground、muted_foreground、border。
* **样式**：`Input::new(state).appearance(false).bordered(false).small()
  .text_size(px(13.0)).p(px(0.0))`——外框 / 底色 / 圆角仍由工具栏的胶囊画，
  只把输入框摆进去；`small()` 是 24px 高（与面包屑段一致）。
* **测试**（`app.rs` 新增 `mod tests`）：进入编辑 → 路径预填 + 整条全选；
  Esc / 失焦退出编辑；再次进入复用同一个实体。测试里**真的 `render_frame`** 一帧
  ——输入框的绘制路径（Theme 全局、点击/选区 overlay）只有画出来才暴露问题。
  * ⚠️ 坑 1：**crate 内测试不能 `use super::*`**——app.rs 顶层有 `use gpui_kit::*`，
    会把 gpui 的 `test` 属性宏引进来顶掉内置 `#[test]`，展开时撞递归上限。
  * ⚠️ 坑 2：测试里要手动 `cx.update(gpui_kit::init)`，否则
    `no state of type gpui_component::theme::Theme exists`。
  * ⚠️ 坑 3：`panel.path` 由异步回灌填充，headless 下不稳定 → 测试里直接摆 path，
    只测「预填 + 全选」这段自己的接线（集成测试版本已放弃）。
* **既有抖动（非本次引入）**：单独跑某个 layout 用例时会随机撞
  「Detected activity on thread `tokio-rt-worker` ... Your test is not deterministic」
  ——`AppState::new()` 起的 tokio worker 撞上 gpui 的 test scheduler 线程断言。
  HEAD 上同样 4/5 命中，全套跑则稳定通过。要根治得让 mo-app 在测试构建下不起
  运行时（或把 worker 收进可控线程），暂未处理。

## 22. 右键上下文菜单（文件 / 文件夹 / 空白处）

* **需求**：给文件与文件夹补上右键菜单。
* **新模块 `crates/mo-ui/src/context_menu.rs`**（不碰文件系统，纯「模型 + 渲染」）：
  * `ContextMenu{x, y, target: Option<PathBuf>, is_dir, selected, paths}`——
    菜单**不持有业务状态**，只记「在哪儿弹的、对着谁、当时选中了哪些」；
  * `items(&menu) -> Vec<MenuItem>`（纯函数，单测覆盖）按上下文推导条目：
    空白处 → 目录级（新建文件夹 / 粘贴 / 全选 / 刷新 / 在终端中打开 / 显示简介）；
    条目 → 打开（目录「打开」、文件「快速查看」）+ 目录专属的「在新标签页 / 分栏中打开」
    + 重命名 / 创建副本 / 复制 / 剪切 / 拷贝路径 / 移到废纸篓 / 压缩…(+归档才有「解压」)
    + 计算哈希 / 比较（恰好 2 项）/ 标签 / 显示简介 /（目录）磁盘用量 + 在终端中打开。
  * `MenuAction` 枚举把 22 个动作与 UI 解耦；`render()` 只负责画；
    真正干活的是 `RootView::run_menu_action`——把动作翻译成**既有**的 `AppState`
    调用或既有模态，菜单层不新增业务逻辑。
* **定位**：菜单是**根容器的绝对定位子节点**，根容器从 `(0,0)` 铺满窗口，所以鼠标
  事件的坐标直接当偏移用（与表头拖拽落点判定同一套坐标）。`render()` 里按
  `viewport_size` 钳一次：先按「鼠标右下」放，超出右 / 下边就贴边（`MENU_W=232`，
  `panel_height()` 把分隔线一起算进去，算错会被切一截）。
  * ⚠️ 根容器必须 `relative()`，否则绝对定位子节点会掉到别处甚至挤动 flex 布局
    （`context_menu_renders_at_the_pointer_without_disturbing_layout` 守这条）。
  * 面板挂 `.occlude()` 截断命中链，防止点菜单穿到下面的文件行；
    `.on_mouse_down_out()` 点外面即关（不用额外全屏遮罩层）。
* **右键的三个落点**：
  * 文件列表行（`file_list.rs`）、网格单元（`grid.rs`）、分栏行（`columns.rs`）：
    `on_mouse_down(MouseButton::Right)` → `open_context_menu(Some((path, is_dir)), …)`
    + `cx.stop_propagation()`；
  * 窗格空白（`app.rs::render_pane`）→ `open_context_menu(None, …)`（目录级菜单）。
    条目上的右键已经 `stop_propagation` 消化掉，不会冒泡到这里。
* **右键要顺带「选中这一项」**（Finder 行为）：右击未选中的条目时，先同步写进
  `panel.selection` 并发一个异步 `app.select(id)`，否则菜单里的「移到废纸篓」
  会去删**旧选区**。同时按 `pane` 把 `active_pane` 切过去（分栏时重命名 / 属性
  读的都是 `active_pane`）。
  * ⚠️ **选中路径快照**：`ContextMenu.paths` 在打开菜单那一刻就按可见顺序固化。
    开模态的动作（重命名 / 压缩 / 创建副本 / 拷贝路径）若去读 app 的选择，
    就要赌上面那个异步任务已经跑完——带快照就没有竞态。
* **既有函数加 target 参数**（都在 `None` 时保持旧行为：选中项的第一个 / 当前目录）：
  `open_properties(cx, Option<PathBuf>)`、`analyze_disk_usage(cx, Option<PathBuf>)`、
  `open_archive(cx, Option<Vec<PathBuf>>)`、`open_batch_rename(cx, Option<Vec<PathBuf>>)`、
  `toggle_split(cx, Option<PathBuf>)`。空白处右键点「显示简介」必须看**当前目录**，
  不能捡旧选区。
* **`mo-app` 新增两个能力**：
  * `duplicate_paths(paths)`：同目录就地复制。**不走 `transfer`**——那是「目标目录
    + 沿用原名」，源与目标同目录时沿用原名会覆盖源文件，目标名必须先
    `unique_path()` 去重；
  * `create_folder(dir, name)`：先判存在再决定是否去重。`unique_path()` 总是从 ` 2`
    起编号，直接拿它会把本来不冲突的名字变成「新建文件夹 2」。
* **Esc**：右键菜单开着时 Esc 先关菜单（`had_menu` 判断后再 `return`），其余按键
  继续走正常路由——菜单不该像模态那样吃掉方向键 / 输入即过滤。
* **测试**：`context_menu` 5 个（空白只有目录级动作 / 目录有开变体无解压 /
  归档才有解压 / 条目随选中数变化 / 高度含分隔线）；`app.rs` 3 个（落点=鼠标位置
  且不挤动列表、右下角钳回视口、条目菜单比空白长 + Esc 关闭语义）。
* ⚠️ 仍是 headless 布局测试 + 纯逻辑单测；真实右键手感与「新建文件夹后是否进
  内联改名」这类交互细节需本机 `cargo run` 验收（当前新建后只 refresh，不自动进改名）。

## 23. 右键菜单补「新建文本文件」

* **需求**：右键菜单里增加新建文本的功能（原菜单只有「新建文件夹」）。
* **缺的那一层是文件系统**：`mo-fs::FileSystem` trait 一直只有 `create_dir`，
  没有任何「写」的能力（UI 不允许直接碰 `std::fs`），所以先在 trait 上补
  `write_file(path, contents)`，由 `LocalFileSystem` 实现。
  * ⚠️ 用 `OpenOptions::create_new(true)` 而**不是** `std::fs::write`：后者在目标
    已存在时静默覆盖——新建文件是数据丢失入口，宁可报错。去重是调用方的责任。
* **`mo-app` 新增 `create_file(dir, name)`**（`name` 为空时用「新建文本.txt」），
  返回真实路径。顺手把 `create_folder` 里那段「**先判存在再决定是否去重**」的逻辑
  抽成 `AppState::free_path(dir, name, fallback)` 给两者共用——`unique_path` 总是
  从 ` 2` 起编号，无条件调用会把本来不冲突的名字变成「新建文件夹 2」，这条坑只有
  一处实现才不会再踩。
* **菜单**（`context_menu.rs`）：`MenuAction` 加 `NewFile`；空白处菜单在「新建文件夹」
  正下方插「新建文本文件」——**同一组、中间不加分隔线**（分隔线留给下面的「粘贴」）。
  条目级菜单（对着文件 / 文件夹右键）**不加**这两项新建，与 Windows 资源管理器一致。
* **UI 落点**：`app.rs` 把原来内联在 `A::NewFolder` 分支里的 spawn 块抽成
  `create_entry(NewEntry, cx)`（`NewEntry{Folder, File}`），两种新建共用一条路径
  （取当前目录 → 异步创建 → `refresh()` → 失败弹 `Modal::Info`），错误文案也统一成
  「新建{文件夹|文本文件}失败：…」。建完只 refresh，**不自动进内联改名**（与文件夹一致）。
* **测试**：
  * `context_menu` 新增 `new_items_share_one_group`（两项相邻、均可用、新建项无分隔线
    而「粘贴」有），`blank_area_shows_directory_level_actions_only` 与
    `panel_height_accounts_for_separators`（7 项 / 3 条线）同步更新；
  * `mo-app/tests/productivity.rs` 新增 `create_file_is_empty_and_never_overwrites`——
    新文件为空、重名序号插在**扩展名之前**（`新建文本 2.txt`）、已有文件内容原封不动、
    空白名字回落到默认名、以及「名字不冲突时不平白加序号」。

## 24. 「已记住的服务器」列表补外框、斑马纹与协议图标

* **需求**：连接对话框里记住的服务器列表按截图改样式——加一圈边框，行底色交替
  （斑马纹沿用文件列表那组色值：奇数行 `theme::zebra()`、偶数行 `theme::surface()`），
  行首再按协议放一枚图标。
* **框角会被行底色切方**：gpui 不把子元素裁进父级圆角（同 §22 与
  `dialog_header_carries_the_card_corner_radius`），所以贴边的首 / 末行得自己收角：
  `context_menu::item_hover_bg` 那套 `rounded_t` / `rounded_b` 照搬过来，行半径 5pt
  比外框 6pt 小一圈（外框还含 1px 描边），同心才不穿帮。中间行四角全直角。
* ⚠️ **`painted_quads()` 里描边不并进底色那张 quad**：一个 `border_1()` 的 div 会画
  五张同位同尺寸的 quad——底色那张 `border_widths` 全 0，四条边各一张、只在对应的
  那条边上带宽度。首版断言直接拿底色 quad 查 `border_widths.top > 0`，必然失败。
  查描边要 `filter(同矩形)` 再看边宽，别用「找到第一张」的辅助函数。
* ⚠️ **绘制测试别拿绝对色值比色**：调色板是进程级全局槽位（`theme::set`），
  `theme::tests::set_switches_active_palette` 和主题选择器用例在并行跑时会临时翻成
  深色，`assert_eq!(quad.background, Background::from(theme::zebra()))` 就是随机闪断
  （首版正是这么挂的）。改成只断**相对**关系：相邻两行不同色、隔一行同色、描边
  alpha > 0——浅色深色两套调色板下都成立。
* **协议图标**（`icons.rs`）：`SMB`（三节点连成网）/ `FTP`（托盘 + 上下双箭头，
  `ftp`/`ftps`/`sftp`/`ssh` 共用）/ `WEBDAV`（一朵云，`webdav`/`dav`/`davs` 共用），
  认不出的一律回落到 `GLOBE` 而不是空着。`protocol_icon(endpoint)` 自己切 `://` 前缀
  并归一小写——scheme 正常由 `RemoteUrl::parse` 归一，但 `config.json` 是明文、可能被
  手改。svg 的颜色必须显式传给 `icons::icon(data, size, color)`，不继承父级文字色。
* **测试**：
  * `app::tests::remembered_servers_list_has_a_frame_and_zebra_rows`——外框四角同半径
    + 四条边都有不透明描边；三行底色奇偶交替；首行只收上面两角、末行只收下面两角、
    中间行全直角；每行行首一枚 14×14 图标且落在地址文字之前；
  * `icons::tests::protocol_icon_maps_scheme_aliases_and_falls_back`——别名归一、
    大小写、无前缀与未知协议回落。

## 25. 网格视图四周留白 + 名称居中

* **需求**：网格视图首行顶着工具栏、滚到底最后一行贴着状态栏（原来 list 只给了
  `px` 左右留白），名称还贴着格的左缘、跟居中的图标对不齐；下一轮用户又指出画廊
  名字也没居中（同一份 `cell` 代码，见下）。
* **`uniform_list` 的 padding 四个方向都吃**，上下不只是装饰：`padding.top` 加到条目
  起点，上下都算进滚动内容高度与 `scroll_max`（`elements/list.rs` 的
  `layout_items` / `prepaint_items`），所以底部那截在滚到底时真留得出来。改成 `.p(12)`
  一条就够，**不需要**再往每行塞 `px`。
* ⚠️ 反面教训：读 `prepaint_items` 里那句 `item_origin = bounds.origin + (0, padding.top)`
  就断定「条目原点不加 `padding.left`，水平 padding 不被消费」，于是给每行加了 `px`
  ——真跑断言时左侧量出来是 24。原点那一行确实只加 `padding.top`，但水平那两侧是在
  `layout_items` 的可用宽度里扣掉的，两处分开处理。**先用一条 headless 断言量一下
  再下结论**，别按单行代码推断。
* **名称居中：靠 flex，别靠 `text_center()`**。第一版给名字那层加 `.text_center()`，
  用户验收回来说画廊（其实网格一样）名字仍贴左。gpui 的 `TextLayout::paint` 是按
  `window.text_style().text_align` 在盒子内对齐的，这条路在 `text!` 上实测不生效。
  改成让这一层**收缩到内容宽**（`max_w_full` + `truncate`，去掉 `w_full`），由 cell 的
  `items_center` 居中——与图标同一套机制，图标能居中早就证明了它有效。
* ⚠️ **居中的断言会假绿**：名字那层是 `w_full` 时，盒子铺满整格，拿它和 cell 比中点
  永远相等，文字贴左也测不出来。所以除了「中点对齐」，必须再加一条**盒子确实窄于单元**
  （短名字）的前置断言。实测：`w_full` 版名字层 843 vs 单元 851（铺满，断言无意义）；
  `max_w_full` 版收缩到内容宽，居中断言才真正在守东西。凡是断言「某元素居中 / 对齐」，
  先确认那个元素本身不是铺满的。
* **测试**：`app::tests::grid_and_gallery_inset_content_and_center_names`——往 panel 里塞
  两个假条目（`covered` 命中就不发取窗任务，快照不会被异步结果冲掉），一条长名字一条
  单字符，网格与画廊各跑一遍（两者共用 `grid::cell`，用户就是先看到网格再看到画廊的），
  量出「单元相对 list 左上各留 12pt」「长名字不溢出单元」「短名字收缩到内容宽且相对
  单元居中」。
* **同一处留白也补给了列表视图**（`file_list.rs` 的 `.px(12)` → `.p(12)`）：行 hover
  与选中底色原本上下贴边。断言 `app::tests::file_list_rows_are_inset_from_the_edges`
  与网格那条共用 `seed_window`（放 in-crate 而不是 `tests/layout.rs` 的原因见
  [engine-testing §6](engine-testing.md)）。

## 26. headless 测试的「真 IO + 确定性调度器」冲突：`not deterministic` 与 SIGABRT 同源（2026-09-22）

* 症状：`cargo test` 偶发红，panic 落在 `gpui-pre-scheduler-*/src/test_scheduler.rs`：

  ```
  Detected activity on thread Some("tokio-rt-worker") ThreadId(18), but test scheduler
  is running on Some("file_list_height_tracks_the_window") ThreadId(4). Your test is not
  deterministic.
  ```

  同一轮日志里往往还能看到第二句 `assertion left == right failed: local task dropped by a
  thread that didn't spawn it. Task spawned at crates/mo-ui/src/app.rs:803`，紧跟
  `panic in a destructor during cleanup` → 整个测试二进制 SIGABRT。
* **两者是同一条链，不是一个 bug 的两个症状**：`assert_correct_thread` 并不当场抛，
  它把 `non_determinism_error` 记下来，等 `end_test()` 才 panic；这个 panic 走 unwind，
  期间 `RootView::new` 里 `.detach()` 的 `tab_loop`（`app.rs:803`）被 tokio 线程 drop
  → `executor.rs` 的 `Checked::drop` 断言「谁生谁销」不成立 → destructor 里二次 panic
  → abort。**修掉第一句，第二句和 SIGABRT 一起消失。**
* 根因：`AppState::new()` / `RootView::new` 会 `spawn_blocking` 去读**真实目录**，读完由
  tokio 的 worker 线程唤醒 GPUI 任务；而 TestScheduler 规定「唤醒必须来自跑测试的那个
  线程」。真机没这问题——GPUI 事件循环本身就在 OS 主线程。
* **修法**（官方豁免开关，一行，放在建 `AppState` / `RootView` **之前**）：

  ```rust
  cx.dispatcher.allow_parking();
  ```

  `TestAppContext.dispatcher` 是 `#[doc(hidden)] pub`；`allow_parking()` 把
  `parking_allowed_once` 置位，此后 `assert_correct_thread` 直接 `return`（该标志
  **不复位**，所以一次调用覆盖整条用例）。
* **它不削弱任何布局断言**：`run_until_parked()` 就是 `while tick() {}`，从不经过
  `park()`，执行时序完全不变；被关掉的只有那道线程检查。
* 边界（别用错）：`allow_parking` 只消除**误报**，并不能让你「等」到外部 IO 完成——
  「点了按钮真的走了哪个后端」这类**断言异步副作用**的用例仍不能放 headless UI 层，
  要继续下沉到 `mo-app` 的语义测试（见 [engine-testing](engine-testing.md)）。
* 实测（同一台机器，`cargo test -p mo-ui --test layout`，4 并发 × 3 波）：
  **加之前 5/12 命中 `not deterministic`（其中 1 次直接 FAILED）→ 加之后 12/12 全绿**；
  单进程 `cargo test --all-features`（含全部 crate）连跑 8 次全绿。
* ⚠️ 排查纪律：先用 `git stash` 二分确认不是你引入的**真**回归（真回归的断言会打印真实
  尺寸数字），再决定要不要 `allow_parking`。另外 `mo-ui/src/app.rs` 里还有 ~25 处单测
  直接 `TestAppContext::single()` + `AppState::new()`，暴露面理论上相同；单独加压
  60 次只翻过 1 次且 48 次连跑复现不出（疑似 4 进程并发时踩共享夹具），**暂未改**。

## 27. 列表行图标的尺寸收口到常量：槽位 16、Lucide 描边 12

**现象**：列表里位图图标（系统图标 / 缩略图）画在 20×20 的槽位里、内置 Lucide
单色 SVG 画 16×16，三个尺寸散落在 `file_item::view` 的三处调用点上，改一次要
顺着找三遍；且 20 的槽位在 24px 行高里上下只剩 2px，视觉偏挤。

**修法**：抽两个常量收口（`crates/mo-ui/src/file_item.rs`）：

```rust
const ICON_PX: f32 = 16.0;   // 槽位边长，同时也是光栅图标（系统图标 / 缩略图）的绘制尺寸
const GLYPH_PX: f32 = 12.0;  // 内置 Lucide 描边 SVG 的绘制尺寸
```

* **位图铺满槽位，描边图形要再小一圈**：`icon()` 的 viewBox 自带留白，SVG 的
  视觉边界比它的绘制框小，若跟位图同尺寸摆在一起会显得偏大。两者差 4px 是长期
  目视校准的结果，别为了「统一」把它们改成同一个数。
* 槽位必须**定宽**（`flex_shrink_0` + `overflow_hidden`）：SVG 与位图的自然宽度
  不同，不定宽的话缩略图一加载文件名就会左右抖动。
* 与平台层的关系：`mo_platform::file_icon` 里那张系统图标是按 40px 重绘再编码 PNG
  （见 [macos-platform](macos-platform.md) §14），下采样到 16px 显示仍有 2x 以上
  余量，**不要在 UI 改尺寸时顺手去动它**——它的大小只为够用，与显示尺寸解耦。
