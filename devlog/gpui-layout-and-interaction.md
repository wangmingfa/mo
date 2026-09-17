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
