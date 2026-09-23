# 传输指示块 / 删除键位 / 回收站入口

2026-09-23。三个用户反馈一起落地：传输进度从「窗口底部整条横幅」改成「左下角小块 + 点击浮层」（仿 GNOME Files）；删除键位按平台归位；侧栏加回收站入口。

## 1. 传输指示：小块 + 浮层

**原状**：`progress_panel::render` 整块铺在 `body` 与状态栏之间，一两条「完成 0%」就把状态栏顶上去两行，任务结束后还长期占着地方。

**现状**：`render_overlay(ops, app, open, entity)` 绝对定位在左下角（`left 10 / bottom 32`，状态栏 26px 上方），不占布局；常驻小块 260px 宽只显示**一个**任务（优先进行中/排队的第一条，全结束显示最后一条）+ 底部 3px 通栏进度条；点小块开浮层（440px，全部任务 + 取消/✕ 移除），点外面（`on_mouse_down_out`）或 Esc 收起。

三个坑：

1. **`on_mouse_down_out` 按 wrapper bounds 判定**——小块和浮层必须包在同一个 wrapper 里（context_menu 的二级菜单同一坑），否则点小块 = 点外面，先关再开。
2. **`impl IntoElement` 两分支必须同型**——空 ops 早退要 `div().id("mo-ops-empty")`（Stateful）而不是裸 `div()`。
3. **headless `click(id)` 只认「被观察」的元素**——必须 `.test_support()` 包一层（gpui-kit 的 observation 注册表；非 test 构建是恒等包装，且要求元素已有 `.id()`）。⚠️ click 用的是 **ElementId**（`.id("sidebar-trash")`），不是 `debug_selector` 的名字——两者不同名时测试点不中。

生命周期补口：`OperationManager::remove(id)` 一直有但没接 UI；给 `AppState` 加 `dismiss_operation(id)`，浮层 ✕ 用来摘已结束的句柄（进行中的走 `cancel_operation`）。

Esc 收浮层挂在根 `on_key_down` 的 escape 分支里（与右键菜单同一个 consumed 合并判断）。

## 2. 删除键位

**历史坑三连**：`file.trash` 默认写 `"delete"` → `KeyCombo::parse` 又把 `"delete"` 折成 `"backspace"`（旧别名折叠）→ macOS 上裸按 ⌫ 触发「移到废纸篓」，而根键盘路由里裸 ⌫ 本来有「无过滤词时返回上级」的结构性分支——被键表拦截了，永远走不到。

**现状**：

- macOS：`cmd+backspace`（⌘⌫，Finder 同款）；Windows/Linux：`delete`（资源管理器同款），在 `default_spec` 的 **else 分支**按平台覆盖（原来只有 macOS 分支会覆盖）。
- `parse` 里 `"delete"` 不再折成 `"backspace"`：前向删除键（fn+⌫ / Delete）是独立物理键。
- `key_label("backspace")`：macOS 显示 `⌫`，其它平台 `Backspace`——不再冒充「Delete」。
- 裸 ⌫ 回落到底部的结构性分支：删过滤词 / 返回上级。

守卫：`trash_uses_platform_delete_key_and_backspace_stays_free`（keys.rs）——断言 `parse("delete").key == "delete"`、裸 ⌫ 在键表里查不到任何动作、默认键位按平台正确。

## 3. 回收站入口

回收站面板（`Modal::Trash`）早就有了，但只能从命令面板进。现在：

- `RootView::open_trash_panel(cx)` 收口（拉快照 + 开模态 + 光标归零），命令面板 `CommandId::OpenTrash` 与侧栏共用。
- 侧栏「快捷访问」区末尾加一行「回收站」（icons::TRASH，Lucide trash-2）。**不进 `quick_locations`**：那套按路径匹配高亮、点击走 `open_local`，回收站是模态面板，语义不同。
- `central_view` 补 `mo-central-view` debug_selector——「次级视图是否占住了中央区」此前没有断言抓手。

## 测试（headless，tests/layout.rs）

- `transfers_render_as_a_corner_badge_above_the_status_bar`：注入假 op（`mo_ui::inject_ops_for_tests` 直塞 `Panel.ops` 快照），断言小块 ≤300px 宽、悬在状态栏上方、状态栏 baseline 不动、浮层默认收起。
- `badge_click_toggles_the_transfer_popover`：真点小块 → 浮层出现且在小块上方；再点 → 收起。
- `sidebar_trash_entry_opens_the_trash_panel`：入口在侧栏内，点开出现 `mo-central-view`。

## 3. 二轮反馈：统一任务面板（2026-09-23 下午）

用户两张截图：①折叠小块的位置OK，但要求这块区域成为**统一任务区**，后续所有耗时操作都进里面；②展开样式怪——浮层悬空、和底下的小块内容重复、进度条灰得像条短线、「完成」和「0%」同框。

**重构**：小块 + 悬空浮层 → **同一张卡片原地展开**（`progress_panel::render_overlay` 重写）：

- 折叠态：300px 一行（状态点 + 描述截断 + 百分比）+ 3px 通栏进度条，整行点击展开。
- 展开态：380px 卡片，标题行「任务（n）· x 个进行中 · 收起 ▾」+ 分隔线 + 两行式任务列表（上行：状态点 + 描述 + 取消/✕；下行：进度条占满 + 尾标）。`on_mouse_down_out` 现在只包一张卡片，「两个元素同包 wrapper」的坑消失。
- 视觉修正三处：进度条 `theme::accent()`（浅色主题是 0xcdcdcd 灰）→ `theme::selected_bg()`（选中蓝）；`ratio_of` 对 Completed 恒返 1.0（不再「完成 0%」）；尾标进行中给百分比、结束态给中文状态（绿色完成 / 红色失败，`rgba(0x248a3d)` / `rgba(0xd70015)`——调色板没有 success/danger 角色，用固定系统语义色）。
- 「统一入口」现状核实：删除/复制/移动/建链全走 `AppState::submit_operation` → `OperationManager` 一份快照，面板只读它。后续远程传输、索引构建只要同样走 `submit_operation` 就自动进任务区。

**新坑**：`theme.rs` 里有私有 `const fn rgba(r,g,b)` 三参帮手，而 `use gpui_kit::*` glob 进来的是 gpui 的 `rgba(u32 hex)` 单参版——同名字不同签名，三参写法直接 E0061。另有：headless `bounds()` 按 `debug_selector` 查，折叠/展开两个分支的 badge 都得挂 `.debug_selector(...)`（`.id()` 不够）。

测试同步改：`badge_click_toggles_the_transfer_popover` 的几何断言从「浮层悬在小块上方」反转为「列表紧贴标题行下方、左缘对齐（同一张卡片）」；宽上限 400。

## 4. 回收站面板二轮：侧栏保留 + 行样式对齐文件列表（2026-09-23 下午）

用户截图：进回收站后左侧整个没了；行又高字又大，像另一套 UI。

**根因**：`Modal::Trash` 是 A 类次级视图，render() 里直接 `self.render_trash()` 顶掉整个中央区——侧栏只在浏览分支渲染，次级视图分支从来没画过它（全局搜索同病）。行样式则是 `render_trash` 自己拼的：emoji 图标、`p(6)` 内边距、默认字号，从没走过 file_list 的行语言。

**修复**：
- A 类里分出「浏览型」子集（全局搜索 / 回收站）：渲染侧栏 + 次级视图并排。回收站开着时「当前位置」高亮置空（进面板前的目录不该继续亮），改高亮侧栏「回收站」入口本身——`sidebar::render` 加 `trash_active` 参数，与快捷位置同款 accent 底。
- `render_trash` 行渲染对齐 file_list：24px 行高、13px 字号、px(4) 内边距、奇偶斑马纹、选中蓝底白字、悬停；emoji 换内置 Lucide 字形（目录 FOLDER、文件按扩展名 `icons::file_ext_icon`——回收站原路径多半已不存在，系统图标查不到）；列改为「名称 + 原目录（截断）+ 删除时间（右对齐）」；行可点击选中（与键盘 ↑↓ 共用 `palette_index`）。
- 测试注入口 `inject_trash_for_tests`（`trash_entries` 升 `pub(crate)`）；headless 回归两条：进回收站后 `mo-sidebar` bounds 不变、`mo-trash-row-0` 高度恰为 24。

## 5. 从回收站导航不退出面板（2026-09-23 傍晚）

用户截图：在回收站里点侧栏「主目录」，地址栏变了（下层面板确实导航了），但人还留在回收站、侧栏高亮也在回收站上。

**根因**：模态关闭只长在两条路上——Esc 键路由和 `dispatch_action`（命令面板）。侧栏五处导航（快捷访问 / 远程连接 / 网络盘 / 卷宗 / 书签）直接调 `AppState::open_local` 等方法，工具栏 ←→↑ 走 `spawn_nav`、地址栏走 `submit_address`，全都绕过了它：下层面板换了页，`self.modal` 还是 `Trash`。

**修复**：`RootView::leave_secondary_view(cx)` 统一出口（modal→None + 重置 palette_index / cmd_query），在 7 个导航入口调用：侧栏 5 处 + 工具栏 `spawn_nav`（签名加 entity）+ `submit_address`。B 类对话框有遮罩挡着点不到侧栏，无条件置 None 安全。

测试坑再录一次：`click()` 认 **ElementId**——复合 id 要传 `("sidebar-loc", 0usize)`（usize！i32 不满足 `From`），而不是 debug_selector 的 `mo-sidebar-loc-0`；且目标必须 `.test_support()` 包过。快捷位置行补了包装。

## 6. 速度 / 剩余时间 / 暂停恢复（2026-09-23 晚）

* **估速在 UI 层差分**，不给操作层埋计时器：`RootView::op_speeds(&ops)` 拿相邻两次
  快照的 `done` 差分出瞬时速度，EMA 平滑（0.6 旧 + 0.4 新），样本记在
  `op_stats: HashMap<op_id, OpSample>`。采样间隔 <50ms 不可信（抖动被放大），直接沿用旧值。
  首次快照无差分 → 速度 0 → UI 只显示百分比。非 Running（暂停/结束）**清样本**——
  暂停期间不搬字节，把暂停时长摊进速度会把「恢复后的速度」算虚低；恢复后从零重新观测。
* **暂停是真的能停**：`Operation::pause()` 原本只置标记，`copy_tree` 从不检查——死代码。
  新增 `fs_util::wait_if_paused(state)`：协作式检查点（进等待把状态标成 `Paused`，
  恢复标回 `Running`，等待中收到取消返回 true），在 `copy_tree` 每个条目 / 每个文件前调用。
  轮询 50ms（恢复延迟肉眼无感，不空转 CPU）。只有复制 / 移动是传输循环，单文件快操作
  （删除 / 回收站）没有可停的检查点。
* **按钮不能骗人**：trait 加 `pausable()`（默认 false，Copy/Move 覆写 true），
  `OperationHandle` 快照带 `pausable` 字段，UI 行尾据此给「暂停 / 继续」或「取消」——
  否则对删除操作按暂停毫无反应。`OperationManager` 加 `pause/resume`，
  `AppState` 加 `pause_operation/resume_operation`（与 cancel 同形的 async 门面）。
* 文案：速度 `B/s → TB/s` 换档（≥100 取整、否则一位小数）；剩余 `8s / 1m20s / 2h05m`；
  速度或剩余估不出就不显示，不给「剩余 0s」这种话。行尾标签语义集中在 `status_tail` +
  `render_op_row` 的 label 分流（暂停/继续/取消/✕ 四态）。
* 守卫：mo-operations `wait_if_paused_blocks_until_resume_or_cancel`（真线程阻塞语义）；
  mo-ui `op_speeds_averages_progress_deltas`（建样本 → 差分出正速度 → 暂停清样本 → 恢复归零）。
