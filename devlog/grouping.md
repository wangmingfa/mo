# 列表分组（按类型 / 按日期）

任务⑤后半。列表视图插**分组头**：无 → 按类型 → 按日期循环切换；组顺序固定、
组内保持现有排序（分组与排序是叠加关系，不是替换）。网格 / 画廊 / 列视图**不参与**。

## 1. 分层

* **mo-core `view.rs`**：`Grouping`（None/Kind/Date，带 `key`/`from_key`/`next`）+
  `GroupKey`（Folder/Image/Document/Media/Archive/Other + Today/Week/Earlier）+
  `Row`（`Header(GroupKey)` / `Entry(visible 位)`）。`DirectoryView` 加
  `grouping` + `rows`，`rebuild` 末尾 `rebuild_rows`：单趟按组分桶（桶序 = 固定组序，
  空组不出头）。行 API：`row_count` / `row_entry(i)` / `row_header(i)` / `pos_to_row(pos)`
  ——**无分组时全部与 visible 恒等**，既有路径零感知。
* **mo-app**：`set_grouping` / `grouping` / `list_row_count` / `list_window(range, grouped)`
  （返回 `WindowRow::{Header(GroupKey), Entry(Entry)}`，UI 需要条目数据所以带克隆）/
  `select_rows_range(from, to)`（行区间 → 条目位区间，跳组头）/ `select_between(FileId, FileId)`
  （shift 连选两端按 id 解析）/ `focus_by_prefix` 与 `move_cursor` 改返**行下标**。
* **mo-config**：`ui.group: String`（`none`/`kind`/`date`），新标签页默认值；
  `panel_with_prefs` 用 `AppState::spawn` 投递种子任务（分组是异步状态，构造处不能 await）。

## 2. 最深的一个坑：行空间 vs 条目空间

分组开启后，列表的 `uniform_list` 行流混着组头，**行号 ≠ 条目位**。窗口快照
（`panel.window`）、框选 y→行换算、shift 连选端点、type-ahead/键盘导航的滚动跟随——
这些原本共用「可见下标」一个空间的东西，现在分属两套。决定：

* 窗口快照按**请求时的空间**取（`ensure_window` 里按 `view_mode == List && grouping != None`
  判），`panel.window_is_grouped` 标记当前空间；`sync_panel` 比对该标记，**切空间必作废
  重建**（只比 count 查不出来：切分组不改条目数，但行数与下标全变）。
* 跨空间传递端点一律用 **FileId**（`SelSync::Range` 从下标对改成 id 对）——id 在任何
  空间下指同一个文件；下标在错误的空间下会指到别的文件，而且编译器查不出来。
* 组头行高与数据行**同为 24px**：虚拟化行高必须恒定，框选的「行号 → 鼠标 y」换算
  也不用分叉；组头落点被视为「已存在行」，不触发框选。

## 3. UI

* 组头行：固定斑马底 + 弱化 11pt 小字，`debug_selector: mo-file-hd-{i}`；
  文案在 mo-ui（`group_title`），mo-core 只定键。
* 切换入口：命令面板（`GroupingCycle`）+ 布局设置器第 7 行（值列显示当前档）；
  两条路都把新档落成 `ui.group` 默认值；「恢复默认布局」连当前面板一起回「不分组」。

## 4. 测试

* mo-core：组序固定 / 空组不出头 / 行流逐行断言 / 日期三桶（无元数据落「更早」）/
  无分组恒等 / 过滤+分组叠加。
* mo-app：`grouping_rows_headers_and_row_range_selection`——行数、窗口行构成、
  行区间选择跳组头、type-ahead 行号、切回不分组还原。
