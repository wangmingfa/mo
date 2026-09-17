# macOS 平台层的坑

日期：2026-09-17。涉及：gpui-pre-macos 0.3.5、objc 0.2.7、AppKit。

---

## 1. 裸二进制（cargo run）⌘Q 退不出去

* **根因**：macOS 的 ⌘Q 语义来自应用菜单栏的「退出」菜单项 → `NSApplication terminate:`。`cargo run` 是裸二进制、没有菜单栏，⌘Q 没人接。
* **修法**：在全局按键路由（`on_key_down`）**最前面**接 `⌘ + q` → gpui 的 `cx.quit()`，保证任何模态状态下都能退出。

## 2. 红绿灯（traffic lights）垂直定位

* **根因**：红绿灯由 AppKit 绘制，不参与 GPUI 布局（`debug_bounds` 量不到），位置全靠 `traffic_light_position` 手动指定。gpui 公式：**按钮中心 = pos.y + 按钮 frame 高 / 2**，而 AppKit 标准按钮 frame 高是 **16pt**（可见圆点 12pt 居中其中）——直接按 12 算必然偏。
* **修法**：`pos.y = (TOOLBAR_HEIGHT - 16) / 2`，即工具栏 48px 时 y=16，中心恰好 24。
* **教训**：曾按估算给 y=14（偏上）、y=18（偏下）各错 2px；两个观测点反推出 frame=16 后一次校准。截图实测校准在沙箱环境不可靠（后台起 GUI 进程静默死、多显示器、色彩配置文件偏移），优先读源码公式。

## 3. Dock 图标比其它应用大一圈

* **根因**：Apple Big Sur 图标网格里 squircle 本体只占 1024 画布的 **824px（82.4%）**，我们画到了 90%。
* **修法**：`scripts/make_icon.py` 的 `CONTENT_RATIO = 0.824`；圆角比例 0.2237。
* **注意**：改完图标 `cargo run` 前先退出旧进程，macOS 会缓存 Dock 图标。

## 4. 图标源图的「透明背景」不可信

* **现象**：Dock 图标圆角外侧有一圈白色残边。
* **根因**：AI 生成的源图实际是**白底**（承诺了 transparent 但 alpha 全 255）。按 bbox 裁切 + 圆角蒙版后，蒙版内、图案 squircle 外漏进白底。
* **修法**：`strip_border_white`——从画布边界 BFS 泛洪，只清除**与边缘连通**的近白区域（内部白色图形与边界不连通，不受影响）；alpha 用 MinFilter + GaussianBlur 反走样；圆角蒙版 4x 超采样。
* **子坑**：`Image.frombytes("L", ..., bytes(255 - v for v in bg))` 把背景写成 254 而非 0，首版完全没抠掉——掩码字节必须 0/255 二值。
* **验证方法**：对角线像素采样应为「透明 → 半透明主题色 → 实色」，全图扫「贴透明区的不透明白像素」= 0。

## 5. 运行时设置 Dock 图标（裸二进制没有 .app 包）

* **要点**：`include_bytes!` 嵌入 PNG，启动时经 objc 运行时调 `NSApplication.setApplicationIconImage`（`crates/mo-ui/src/icon.rs`）；非 macOS no-op。
* **objc 0.2 的两个坑**：
  * `msg_send!` 多参数选择器**不用逗号**分隔：`msg_send![cls, dataWithBytes: p, length: n]`；
  * 其宏内部 `cfg(feature = "cargo-clippy")` 会展开到**使用方 crate**，新版 rustc 触发 `unexpected_cfgs` 告警——在使用方 `[lints.rust] unexpected_cfgs` 里声明 `check-cfg` 消除（见 crates/mo-ui/Cargo.toml）。

## 6. unsafe 的收敛

* **约定**：`icon.rs` 是全仓唯一 FFI 层，模块级 `#![allow(unsafe_code)]` + 每处 `unsafe` 带 SAFETY 注释；workspace `rust.unsafe_code = "warn"` 保证新 unsafe 出现在别处会告警。
