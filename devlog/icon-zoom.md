# 图标缩放（网格 / 画廊）与 ui.icon_scale

任务⑤前半：⌘+/⌘-/⌘0 三键 + 命令面板 + 布局设置器三入口，倍率持久化到 `ui.icon_scale`。
列表 / 列视图**不参与**（行高固定 24pt，放大图标牵动整行布局，见下）。

## 1. 几何：Zoom 值对象，三处几何只许一个入口

缩放要同时落在**三样**东西上：

* 方框 `visual_box`（图标画多大）；
* 单元宽 `cell_width`（一行放几列）；
* 行高 `row_height`（`uniform_list` 行高必须与行元素 `.h()` 一致）。

三处各乘一次、各写各的，迟早有一条漏乘（漏方框→图标溢出格子；漏单元宽→相邻单元互相压住；
漏行高→行行重叠）。所以 `listing::Zoom(f32)` 把三个几何函数包成方法，视图只拿一个 `Zoom`：

* `factor(mode)`：网格 / 画廊返回倍率，列表 / 列视图**恒 1.0**——边界约定收在唯一一处；
* `icon_slot(mode)` 跟着方框一起缩：放大后还去要原档位图 = 大倍上采样糊成一片，
  缩小后白取大档 = 白花主线程时间（档位判定仍是 `mo_app::icon_px_for_slot`）；
* 文字**阻尼**缩放 `zoom_text`：收在 `[0.9, 1.4]`——2× 时名字 24pt 比图标还抢眼，
  缩到 0.75 时 11pt 的「大小」行只剩 8pt 就糊了。

自由函数 `columns_for(cell_w, available)` 改成接收**缩放后**单元宽的共享内核，
`Zoom::columns_for` 委托它——列数公式只存在一份。

## 2. 档位：mo-config 收口，mo-ui 只消费

`ICON_SCALE_MIN=0.75 / MAX=2.0 / STEP=0.25`（`mo-config`）。下限 0.75 的依据：
文字不缩放，方框缩到 27pt 以下格子被两行文字撑破；上限 2×：画廊 96pt 方框放到 192pt
还看得见缩略图，再大只是把可视条数压到个位数。

`clamp_icon_scale`：NaN 回落默认；**±∞ 交给 clamp 顶到边界**（∞ 语义明确，不是脏值；
第一版用 `!is_finite()` 把 +∞ 也回落成 1.0，被测试抓住）；量化到 0.01 吃掉浮点步进脏值
（`0.75+0.25+0.25 = 1.2499…`）。配置里的坏值读进来不收口的话，`0` 会让方框边长变 0、
NaN 污染整条几何链。

`RootView::set_icon_scale` 是唯一写入口：夹档位 → 值没变不落盘（按住 ⌘+ 不连写 config.json）
→ `persist_ui` + `notify`。

## 3. 键位：印刷变体是这组键的全部难点

`view.zoom_in = cmd+=`（键帽字符，macOS「⌘+」物理上就是 ⌘⇧=）、`zoom_out = cmd+-`、
`zoom_reset = cmd+0`。三个坑：

1. **`-` 不能当分隔符**：键串解析原来同时按 `+`/`-` 切，`cmd+-` 被切成 `["cmd",""]` →
   主键空串 → 解析失败 → 动作静默失联。现在有 `+` 只按 `+` 切，没有才退回 `-`（老写法）。
2. **印刷变体要连 Shift 一起折**：默认键位写 `cmd+=`（无 Shift），用户按「⌘+」gpui 报的却是
   Shift+=（key == `+`）——`fold_typographic_shift` 只折 key 不折 shift 的话，两个组合永远差
   一位、永远打不中。`+`/`=` 与 `_`/`-` 两族折回基本键名时 Shift 位一并归掉（这四个字符
   没有别的绑定用，无副作用）。守卫 `keys::tests::zoom_keys_survive_typographic_variants`。
3. `cmd++` 这种写法经 `+` 切分后主键为空、解析不出，配置里统一写 `cmd+=`。

## 4. 派发接线（四条路都到）

键表 `Keymap::lookup` → `dispatch_action("view.zoom_*")`（⌘±/⌘0）；
命令面板 `CommandId::{ZoomIn,ZoomOut,ZoomReset}`（run_command 6635 / on_key_down 6855）；
布局设置器 8 行（4/5/6 = 放大 / 缩小 / 还原，7 = 恢复默认，`layout_activate` 直接 return
免得底部再 `persist_ui` 写第二次）；右键菜单不加（空白菜单已 8 项，缩放不是高频右键动作）。

## 5. 测试

* `mo-config`：`clamp_icon_scale` 档位 / 量化 / NaN / ±∞；坏配置不拖垮整份（`icon_scale:0` + 同级正常字段照常生效）。
* `mo-ui::listing`：缩放只碰网格 / 画廊；1.0× 与旧几何逐项相等（默认配置渲染不能变）；放大列数变少、缩小变多；坏倍率仍算出可用几何（有限、>0、至少 1 列）；文字阻尼端点。
* `mo-ui::grid`：`zooming_scales_the_painted_box_and_bitmap`（真实 headless 窗口量 bounds）。
* `mo-ui::keys`：`zoom_keys_survive_typographic_variants`（§3 的三条）。
