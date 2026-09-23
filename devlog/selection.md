# 选择交互（反选 / 框选 / type-ahead）

日期：2026-09-23。工具链：rustc 1.98.1，gpui-kit 0.6.6 / gpui-pre 0.3.6。

选择模型：app 侧 `AppState::selection`（`mo-core::SelectionModel`）是唯一事实来源；
UI 本地 `panel.selection` 只做即时反馈，异步 `pull_selection` 回灌。三件套都落在这条
边界上——本地先动、抬起/结束时回灌 app。

---

## 1. 反选（⌘⇧A）：只在可见集内翻转

* `AppState::select_invert_visible`：`visible_indices()` 里「没被选中」的替换选择集。
* ⚠️ **只在可见集内翻**：藏起来的（过滤掉的 / 隐藏文件）不参与。否则「反选」会选中用户
  根本看不见的东西，下一操作就动到意料之外的文件。与「全选」同一条边界。
* 落点：`keys.rs` 的 `select.invert`（`cmd+shift+a`，进 `BROWSER_SCOPED` 让模态打开时吞键）
  → `dispatch_action` / `run_menu_action` → `app.select_invert_visible().await` + `pull_selection`。
* 右键空白处菜单 `MenuAction::InvertSelection`（紧接 `SelectAll`）。

## 2. 框选（列表视图橡皮筋）：本地实时、抬起回灌

* 几何只在列表视图成立（网格/画廊/列视图行高/槽位不同，未覆盖）。
* 起点：`file_list` 里列表的 `on_mouse_down`，**只有点在空白区**才启动——`start_box_selection_if_empty`
  按 `list_origin + PAD(12) + i*ROW_H(24) - scroll_y` 反解行下标，落在已存在行带内就当点到条目、
  交给那行的单选/拖拽。
* 拖动：`RootView` 根容器的 `on_mouse_move` → `update_box_selection` 实时更新橡皮筋 + 本地选择；
  `on_mouse_up` → `finish_box_selection` 回灌 app（`clear_selection` + `select_range` + `pull_selection`）。
* 列表内容区左上角（`list_origin`）由 `on_prepaint` 回写（见 §4）。

## 3. type-ahead：复用 `panel.query`，打字即跳选

* 不另建缓冲：敲字符走既有「输入即过滤」路径（`panel.query.push(ch)` + `apply_filter`），
  同时 `app.focus_by_prefix(prefix)` 在可见条目里找**文件名**以输入串开头的第一条、单选它、
  返回可见下标；UI 用 `scroll.scroll_to_item(idx, ScrollStrategy::Center)` 跟随。
* 比较大小写不敏感；空串 / 无匹配返回 `None` 且不改选择。与 Finder / 资源管理器一致。

## 4. ⚠️ 本次踩的 gpui-kit 0.6.6 API 坑（可复用）

* **`scroll_to_item` 在 `UniformListScrollHandle` 上**，签名 `(ix: usize, strategy: ScrollStrategy)`
  （`gpui-pre` `elements/uniform_list.rs`），**不是** `List` 元素那个 `(IndexPath, ScrollStrategy, &mut Window, &mut Context)`。
  别对着 `panel.scroll` 调错重载。
* **`ScrollStrategy`** 定义在 `gpui` crate，经 `gpui_kit` 的 `pub use ::gpui::*` 再导出 →
  写 `gpui_kit::ScrollStrategy::Center` 即可。
* **`Pixels.0` 是 `pub(crate)`**（外部 crate 不可访问）→ 取 f32 用 `f32::from(pixels)`
  （`gpui-pre` `geometry.rs` 有 `impl From<Pixels> for f32`）。`offset().y.0` 这种写法编不过。
* **`on_prepaint` 只在 `Div` 上**（`gpui_base::ElementExt`，不是 `UniformList`）。`ElementExt`
  在 gpui-base 根级 `pub use element_ext::ElementExt`，公开路径是 `gpui_kit::base::ElementExt`
  （**不是**私有子模块 `gpui_kit::base::element_ext::ElementExt`）。
* **`uniform_list(..., move |...| { ... entity ... })` 整体 move 捕获 `entity`**：后面若还要
  `entity.clone()`（如 `on_mouse_down` / `on_prepaint` 闭包）会报「borrow of moved value」。
  修法：进闭包前先 `let entity = entity.clone();` 用块 shadow，保留外层 `entity`。

## 5. 测试

* `mo-app/tests/productivity.rs`：`focus_by_prefix_jumps_to_first_matching_name`（大小写不敏感、
  无匹配/空串不动选择）、`invert_selection_flips_visible_set_only`（翻转变为另两项、全选后反选为空）。
* `mo-ui/src/context_menu.rs`：`blank_area_shows_directory_level_actions_only` 期望 8 项含
  `InvertSelection`；`panel_height_accounts_for_separators` 同步 `blank.len() == 8`。
