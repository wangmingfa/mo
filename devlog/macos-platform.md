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

## 7. `Class::get("NSThread")` 返回 None —— 框架没链接

* **现象**：`mo-platform` 的裸二进制（example）一跑就 panic 在 `.unwrap()`，Mac 上明明有 `NSThread`。
* **根因**：`objc` 只链接了 `libobjc`；Foundation / AppKit 的类是**按需加载**的，没有链接指令就没人把它们加载进进程，`Class::get` 自然查不到。App 里（Mo 本体）碰巧因为 gpui 依赖 cocoa 已经拉进来了，所以同一份代码只在**裸二进制**里炸。
* **修法**：显式声明
  ```rust
  #[link(name = "Foundation", kind = "framework")]
  extern "C" {}
  #[link(name = "AppKit", kind = "framework")]
  extern "C" {}
  ```
* **顺带**：`Class::get(...).unwrap()` 这种写法本身就不该留在业务路径上——类拿不到时应当 `Err(PlatformError::Failed)`（`class()` 辅助），否则一次「系统缺了某个类」就把整个进程带走。

## 8. 回收站：`NSWorkspace.recycleURLs:` 换来一片沉默，换成 `NSFileManager`

* **现象**：`recycleURLs:completionHandler:` 返回 NO（文件没动），拿不到任何原因。
* **根因**：它是**异步** API（结果走 completionHandler、还要主线程 run loop 配合），在后台任务 / 裸二进制里传 nil handler 就只剩一个 NO。
* **修法**：改用同步的 `NSFileManager.trashItemAtURL:resultingItemURL:error:`——不挑线程、直接给 `NSError`，能把系统的原话（「宗卷不支持废纸篓」这类只有系统知道的原因）带回给用户。
* **注意**：错误信息要走 `NSError.localizedDescription`（`UTF8String` 取），别自己猜原因。

## 9. AppKit 调用必须回主线程，于是有了死锁的前提

* **约束**：`NSWorkspace`（在访达中显示 / 推出卷宗）是 AppKit 的，要求主线程。Mo 的后台任务都在 tokio worker 上，所以 `on_main_thread` 会判 `NSThread.isMainThread`，不是主线程就 `dispatch_sync` 到主队列。
* **坑**：`dispatch_sync(主队列)` 要**主线程正在 drain 主队列**才回得来。Mo 本体没问题（主线程在跑 GPUI 事件循环），但 **`cargo test` 里主线程被 `block_on` 占着**，测试一调就挂死（表现为 SIGTERM / 超时，没有任何 panic）。
* **对策**：自动化测试**不调** AppKit——守卫落在判据本身（`AppState::goes_through_remote` 已是为这个暴露成 pub 的），真机行为用一次性 example 探针验证（跑完即删）。

## 10. 卷宗 / 位置区（2026-09-22）

* 侧边栏加「位置」区：外接磁盘 / DMG / Time Machine 盘。`mo_platform::volumes()` 列 `/Volumes` 下条目，用 `statfs` 的 `f_fstypename` 把网络型（smbfs/nfs/afp/webdav/cifs/ftp…）过滤掉——那些归「网络」区，别两边列同一个盘。
* `AppState::volumes()` 带 `VOLUME_TTL=5s` 缓存（侧边栏每帧问，裸 `statfs` 逐个查会抖）；`eject_volume()` 先问平台 `eject` 再退回 `mo_remote::mount::unmount`（Mo 自挂目录 AppKit 不认）。行尾「推出」按钮照样 `stop_propagation()`。

## 11. 系统文件图标（2026-09-22）

* `mo_platform::file_icon(path) -> Option<Vec<u8>>`：拿到访达同款真实图标（`.app` 是真 App 图标、文档是所属 App 图标），比内置 Lucide 单色 SVG 准。转 PNG 链路：`NSWorkspace.iconForFile:` → `TIFFRepresentation` → `NSBitmapImageRep` → `representationUsingType:properties:`（`NSPNGFileType = 4`，`properties` 传 `nil`）。
* 缓存：`AppState::file_icon` 把 PNG 写进 `temp_dir()/mo-icons/<hash>.png`，按路径键、封顶 4000 清空，列表行用 `img(path)` 加载（光栅图没法用文字色描边）。远程页（`browsing_remote()`）直接返 None 退回内置 SVG。
* ⚠️ **`?` 运算符陷阱（这一轮真踩了）**：`on_main_thread(move || …)` 的闭包若返回 `Option<_>`，里面**不能**写 `workspace()?` / `class(..)?`（那是 `Result`，`?` 要求返回类型是 `Result`）——要 `workspace().ok()?` / `class(..).ok()?`。返回 `Result` 的闭包（`reveal`/`eject`）才直接 `?`。这处编译期才发现，但本轮回合一开始写错、靠通读抓回。
* 只接了主列表（`file_item::view` 加 `system_icon: Option<PathBuf>` 参数 + `file_list` 调用）；grid / columns 当初仍是内置 SVG，见下面的补记。

### 11.1 补记：四个视图统一，位图按槽位分两档（2026-09-22 晚）

**诉求**（用户）：「现在好像只有列表视图才有系统图标吧，其它 3 个模式还是用的自绘图标，没有统一」。

* **四个视图走同一条链路**：`file_item::{system_icon, entry_system_icon}`。
  * `entry_system_icon(app, entry, slot_pt)` 给有 `Entry` 的视图（列表 / 网格 / 画廊），内含「有缩略图的行不问」那道判据；
  * `system_icon(app, path, is_dir, slot_pt)` 是底层那层纯查表，**列视图用**它——列视图的条目是 `LightEntry`（`name/kind/path`，压根没有缩略图状态，也不画缩略图），不必过那道判据。
  * 落点：`file_list`（改调 `entry_system_icon`）、`grid::cell`（网格 + 画廊）、`columns::column_box`（`columns::render` 多收一个 `&AppState`，因为列视图的数据不走主目录模型、拿不到 `panel.app` 以外的来源）。
* **位图铺满、描边缩一圈**：网格 / 画廊里那块方框（`listing::visual_box` = 36 / 96pt）由缩略图**或**系统图标铺满，内置描边 SVG 按 0.6 缩着画——与列表行（位图 16 / 描边 12）同一条约定。为此把 `grid::cell` 里「上半部分那块方框」抽成 `grid::visual()`，好处是它能被单独摆进测试（`cell()` 要 `Entity<RootView>`，不好测）。
* **槽位 → 位图档位**（这一轮的主要内容）：`mo_platform::file_icon_raster(path, px)` 多了 `px`（原来是模块内写死的 `ICON_PX = 40`）。档位在 `mo_app::icon`：
  * `ICON_PX_SMALL = 40` / `ICON_PX_LARGE = 128` + `icon_px_for_slot(slot_pt)`，阈值 24pt；
  * **档位是缓存键的一部分**（`IconKey::Path(PathBuf, u32)` / `Type(String, u32)`，两张表的主键也跟着带上），两档各存各的、互不串用——代价是切视图模式时新槽位要重新问一轮（一屏几十张、每种类型 / 路径只问一次）。
  * 为什么不一刀切：**全按 40px** 取，画廊那个 96pt 的方框里就是近 5 倍上采样（糊）；**全按 128px** 取，主线程那段（`iconForFile:` + 重绘 + 拷像素）按**面积**涨约 10 倍，而列表里 16pt 的小图标根本用不上。大档取 128 是与缩略图同数（`mo_thumbnails::DEFAULT_SIZE`）：画廊方框按 @2x 要 192px，128px 是 1.5 倍上采样，**与缩略图同等**，所以「有缩略图的行」和「只有图标的行」看不出差别。
* **视图侧只有一张槽位表**：`listing::icon_slot(mode)`（列表 / 列视图 = `file_item::ICON_PX` 16；网格 / 画廊 = 自己那个方框）+ `listing::visual_box(mode)`。视图别写死数字，也别自己算像素（`AppState::file_icon` 只收槽位大小）。

**守卫**：

| 用例 | 位置 | 钉什么 |
|---|---|---|
| `small_slots_take_small_rasters_and_big_slots_take_large` | `mo-app/icon.rs` | 阈值：≤24pt 取 40px，36 / 96pt 取 128px；顺带编译期断言「大档 > 小档」 |
| `the_two_buckets_never_cross_serve` | `mo-app/icon.rs` | 档位是缓存键的一部分：按类型（`.txt`）与按路径（目录）两种键都验「小档的图填不了大槽位」 |
| `every_view_mode_maps_to_the_expected_icon_bucket` | `mo-ui/listing.rs` | **穷举 `ViewMode::ALL`**，把视图槽位表与 `icon_px_for_slot` **组合**起来断言。这是「四个视图统一」唯一能自动验的一环 |
| `the_bitmap_slot_fills_the_box_in_grid_and_gallery` | `mo-ui/listing.rs` | 槽位与方框的关系：网格 / 画廊铺满方框，列表 / 列视图没有方框 |
| `the_visual_box_holds_a_full_bleed_bitmap_or_a_smaller_glyph` | `mo-ui/grid.rs` | 四格（缩略图 / 系统图标 / 内置 SVG / 加载占位）× 两种模式：方框一样大、位图铺满、描边 0.6、没给系统图标绝不画位图。用 `mo-grid-raster` / `mo-grid-glyph` 两个 selector 区分走了哪条分支 |

反向验证（逐条改坏确认变红，6/6）：阈值 → 1000（红 3 条）、查表丢档位（红）、`icon_slot(Gallery)` 退回 16（红）、位图按 0.5 画（红）、系统图标分支改走描边（红）、外层方框不定宽（红）。

⚠️ **有一处测不到**：「某个视图是否真把 `listing::icon_slot(mode)` 传下去了」——那是最外层一行的接线。headless 里缓存永远是空的（`AppState::file_icon` 恒返 `None`，真活在不存在的图标泵里），「接了」和「没接」渲染出来一模一样。靠注释 + 真机看一眼（网格 / 画廊里该是彩色真实图标，而不是单色描边图）。同 `gpui-layout-and-interaction.md` §31 里「z 序测不到」那条一个道理。

⚠️ **`px` 必须由调用方给，且要跟缓存键一致**：泵取图时用的是 `key.px()`（键里存的档位），不是某个全局默认值——不然列表问来的 40px 会写进画廊要的那一格。`CGBitmapContextCreate` 拿到 0 长度缓冲会失败，所以 `px.max(1)`。

## 12. 隐藏文件判据（2026-09-22）

* `mo-fs/src/local.rs` 的 `metadata().permissions.hidden` 之前**写死 `false`**——`.DS_Store`/`.git` 一律显示「不隐藏」，是真 bug。改 `is_hidden(path, m)`：unix 上「文件名以 `.` 开头」**或** `st_flags & 0x8000`（`UF_HIDDEN`，macOS `chflags hidden`）；非 unix 仍 `false`。
* ⚠️ **`st_flags` 是 macOS-only**：它来自 `std::os::macos::fs::MetadataExt`，**不是** `std::os::unix::fs::MetadataExt`（Linux 的 `stat` 没有 `st_flags` 字段，unix 版 trait 不提供该方法）。所以 `is_hidden` 里 `st_flags()` 必须 `#[cfg(target_os = "macos")]` 限定、import 也只在该分支引入，否则 Linux 编译不过 + macOS 报 unused import。dotfile 判据走 `std::os::unix::ffi::OsStrExt`（unix 全平台），与 `st_flags` 分开。
* **单测不要 `.expect()` 异步 `metadata()`**：`LocalFileSystem::metadata` 返回 `Pin<Box<dyn Future>>`，同步 `#[test]` 里直接 `.expect()` 是编译错。直接测 `is_hidden(path, &std::fs::metadata(p).unwrap())` 这个纯函数即可（子模块能访问 private fn）。
* 安全：这个 `hidden` **不参与列表过滤**（列表只看 `ReadDirEntry`，`show_hidden` 配置项也没接过滤），所以改它只修正属性面板、不会突然把用户的 dotfile 藏起来。过滤是另一件事。

## 13. file_icon 在渲染里被调 → 测试死锁（2026-09-22 抓到并修）

* **现象**：给 `file_list` 渲染加 `app.file_icon(path)` 后，整条 `mo-ui` 测试套件**挂死**（之前 23 分钟跑不完，单跑 `file_list_rows_are_inset_from_the_edges` 60s 不过）。根因：`file_icon` 底层 `mo_platform::file_icon` 走 `on_main_thread` → 非主线程时 `dispatch_sync` 回主队列；而 `TestAppContext::single()` 把测试体跑在**子线程**（非 OS 主线程），主队列不 drain → `dispatch_sync` 死锁（和 §9 的 `reveal`/`eject` 同款陷阱，**但 file_icon 在 render 里被调、测试必然触达**，所以比 reveal/eject 更容易踩）。
* **修法**：`mo-platform` 暴露 `is_main_thread()`（macOS 走 `NSThread.isMainThread`，非 macOS 恒 `true`——非 macOS 那边 `file_icon` 直接返 `None`、无 `dispatch_sync` 死锁）。`AppState::file_icon` 在缓存未命中分支、**调平台之前**加守卫：`if !mo_platform::is_main_thread() { return None; }`——非主线程（测试 / 后台任务）跳过平台调用、回退内置 SVG（系统图标只是视觉加成，不致命）。
* **为什么安全**：GPUI 事件循环（含渲染）跑在 OS 主线程，真机 `is_main_thread()` 为 `true` → 照常出系统图标；测试把渲染跑在子线程 → `false` → 跳过、不死锁。已验证 `file_list_rows_are_inset_from_the_edges` 从 60s 死锁变 0.06s 通过。
* **反向验证**：把 `file_list` 里的 `file_icon` 调用临时改成 `None` 重跑，确认布局/图标相关测试行为不变 → 守卫不影响真机路径。

## 14. 系统图标别把原始尺寸转 PNG（「启动后特别卡、鼠标一直转圈」的根因，2026-09-22）

* 现象：`cargo run` 起来后整个应用卡住、沙滩球不断。真机探针量出**40 个条目 10.77 秒**
  （单张 250–540ms）——而 `file_icon` 是在 `file_list` 的 `uniform_list` 渲染回调里**逐行**调的
  （`file_list.rs` 里 `app.file_icon(&entry.path)`），一屏几十行就是 10 秒级的主线程阻塞。
* 根因：`NSWorkspace.iconForFile:` 给的是**原始尺寸** NSImage（512×512 起），
  `TIFFRepresentation` → `NSBitmapImageRep` → PNG 整条链都在处理几百 KB 的位图。
  实测对普通文件出 103KB / 对目录出 787KB 的 PNG。
* ⚠️ `setSize:` **没用**：它只改「逻辑尺寸」，`TIFFRepresentation` 照样按原始像素出图
  （实测 PNG 仍是 787KB、耗时几乎没降）。必须**真画一张小的**：
  ``[[NSImage alloc] initWithSize:40×40]`` → `lockFocus` → 原图 `drawInRect:` → `unlockFocus`
  → 再 `TIFFRepresentation` → PNG。`objc` 里要自己补 `NSSize` / `NSPoint` / `NSRect`
  （`{CGSize=dd}` / `{CGRect={CGPoint=dd}{CGSize=dd}}` + `Encoding::from_str`），
  且 `alloc` / `initWithSize:` 的对象没开 ARC、画完要 `release`。
* 效果：40 张 **10.77s → 0.123s**（稳态 1.2ms/张，首张 65ms 是一次性初始化），
  PNG **787KB → 6.7KB / 21KB**。列表行只要 20pt（@2x 取 40px），再大纯属浪费。
* 纪律：**渲染路径上不许出现 AppKit 调用 + 位图编码 + 写盘**。⚠️ 这条调用还必须在主线程
  （`on_main_thread`），所以放后台也只是把它 `dispatch_sync` 回主线程——省不掉，只能让它
  足够小、并且靠缓存命中（`AppState::file_icon` 的路径键缓存）来避开重复生成。

## 15. 卷宗能不能「推出」要问系统，别一律画按钮（2026-09-22）

* 现象：侧栏「位置」区里，**内置硬盘（Macintosh HD）** 行尾也有一个推出按钮。内置盘没有
  「推出」这个概念——访达不给它画，点了只会得到一句「推出失败」。
* 根因：`Volume` 只带 `name` / `path`，侧栏无条件给每行画按钮。
* 判据：macOS 上问 Foundation 的卷宗属性（`getResourceValue:forKey:error:`）——
  * `NSURLVolumeIsInternalKey == true` → **不可推出**（内置盘，先否掉）；
  * `NSURLVolumeIsEjectableKey == true` → 可推出（USB / SD / 光盘）；
  * `NSURLVolumeIsRemovableKey == true` → 可推出（磁盘映像 `.dmg`）；
  * `NSURLVolumeIsLocalKey == false` → 可推出（网络盘）。
  ⚠️ `None`（该 key 读不到）**不算证据**：不能因为 `IsLocal` 问不到就当网络盘、把按钮
  画出来，所以除明确的 `false` 外一律不成立。判据本体抽成纯函数 `decide_ejectable`
  才好进单测——真机问 AppKit 那步在测试子线程里会被下一段的守卫拦掉。
* ⚠️ 读属性同样只能在主线程（`on_main_thread`）：`volume_is_ejectable` 开头必须有
  `is_main_thread()` 守卫（非主线程保守 `false`），否则 headless 测试渲染侧栏时
  `dispatch_sync` 回主队列会**整套挂死且没有 panic**。与 §14 的 `file_icon` 是同一条坑，
  只不过这次的调用点在**同步的** `AppState::volumes()`（侧栏每帧问一次，靠 5s TTL 兜底）。
* 真机探针（一次性 example，跑完即删）确认：`/Volumes` 下 `Macintosh HD`（apfs）→
  `ejectable=false`；另一块 `172.25.48.48`（**nfs**）被 `is_network_fs` 正确滤到「网络」区，
  不会在「位置」区重复出现。顺带验证了 `[NSThread isMainThread]` 在我们这种非 Cocoa
  命令行程序里也返回 `true`。
* 单测两条：`only_removable_volumes_can_be_ejected`（四类卷宗 + 读不到时的保守）、
  `volumes_stay_conservative_off_the_main_thread`（守「守卫别忘了加」）。

## 16. 图标偶发拿不到 → 永久挂内置 SVG：失败要冷却重试（2026-09-23）

* 现象：Downloads 里个别文件夹（`js-sdk`、`daemon-NJ-…-21-49-58`）一直是描边文件夹
  SVG，重启前永远不补。截图裁开放大确认挂的是**内置 Lucide 兜底**，不是系统给的错图。
* 根因两条叠加：
  * **系统侧**：macOS 26 图标服务抖动——同一目录同一进程连问 `iconForFile:` 两次，
    实测可能一次给蓝色文件夹、下一次 **4µs 瞬间返回 nil**；另一轮两次都给「空白文档」
    占位图；再一轮又正常。与路径内容无关（名字纯 ASCII、xattr 有没有 quarantine 都抖）。
  * **Mo 侧**：图标泵把「问一次失败」用 `asked` 集合**永久记账**（旧注释明说「不再
    重试」），偶发失败 → 整个会话那一行停在内置 SVG。渲染层 `request()` 每帧想再要，
    也被 `asked` 拦住。
* 修法（`mo-app/src/icon.rs` + `extract_icons`）：失败进**重试队列**（带 1s 冷却、
  排队尾），`MAX_ICON_ATTEMPTS = 6` 次用尽才认命（认命时清掉该键在途重试，保证
  「最多问 6 次」语义干净）；成功落库清零失败账。`pop_next(now)` 优先吐新请求、
  重试只吐**到期**的——反复失败的老键堵不住用户正在看的新行。
* 探针：`cargo run -p mo-platform --example icon_probe -- <路径>…`（保留在仓库，
  复现图标问题用；报告 Some/None + 不透明像素数 + 平均 RGB，能区分蓝文件夹/空白文档）。
* 残余风险：抖动的另一种形态是**给了「空白文档」位图**（Some 而非 None），照常落库
  会话内显示白页图标——未在真机 Mo 上复现过，先不做像素级启发式拦截，出现了再说。

## 17. 图标空窗期：目录行先给系统蓝文件夹占位 + 退避提速（2026-09-23，接 §16）

* 用户复测：重试生效了（会自愈），但要**滚动几下**才陆续跳成系统图标——固定 1s
  冷却太慢，且空窗期目录行露内置描边 SVG，观感就是「还没修好」。
* 两条修法（`07b9e98`）：
  * **目录占位图**：`folder_icon_raster(px)` 走 AppKit 资产目录
    （`NSImage imageNamed: NSFolder`），不碰文件路径、不吃 iconservices 抖动，稳定
    ~0.2ms。泵启动一拍内备好 40/128 两档；`file_icon` 查表未命中且是目录 → 立刻返回
    占位（仍照常排队真图标），真图标到了无缝替换。普通目录真图标与占位**一模一样**，
    特殊目录（桌面/下载）晚一点换自己的——这正是 Finder 的观感。
  * **指数退避**：冷却 150ms 起翻倍（150→300→600→1.2s→2.4s→4.8s，封顶 5s）。抖动
    多半几百毫秒恢复，早期密集重试直接把空窗压到一两拍。
* ⚠️ 顺手抓到一个**潜藏已久的越界读**：`nsstring()` 把 `&str::as_ptr()` 直接传给
  `stringWithUTF8String:`，而 `&str` **不保证 NUL 结尾**。路径来自堆上 `String`，
  后面凑巧是 0 才一直侥幸能用；`"NSFolder"` 字面量在 rodata 里后面是别的数据，读出
  乱码名，`imageNamed:` 恒 nil（表现为：分步探针每步都对、封装函数恒 None——差异
  就在字面量 vs 堆字符串）。改 `CString` 根治。教训：**凡是把 Rust 字符串喂给 C 的
  「…UTF8String:」族 API，一律 CString**。
* 探针扩展：`icon_probe` 无参数时连取三次 NSFolder 占位图，验稳定性（应恒 Some、
  同 opaque 像素数）。

## 18. 启动即卡死：if-let 临时 MutexGuard 跨体再入锁（2026-09-23，接 §17）

* 现象：`07b9e98` 之后 `cargo run` 启动直接转彩球。headless 测试全绿、真机必挂。
* 根因（一行）：`extract_icons` 里
  `if let Some(px) = self.icon_cache.lock().unwrap().pending_folder_fallback() { … }`
  ——**2021 edition 下 `if let` 条件里的临时值活到整个 if-let 语句结束**，MutexGuard
  全程持锁；体内下一行再 `self.icon_cache.lock()` 取缓存 = 同一把非重入锁再入，
  泵线程当场死锁且**持锁不放**。主线程首帧渲染走 `file_icon` 查表要同一把锁 →
  全应用卡死。泵一启动（占位图未备齐必然非 idle）就撞上，所以是「启动即卡死」。
* 修法：先 `let pending = ….lock().…;` 再 `if let Some(px) = pending`，守卫在语句尾
  立即释放（`dae4ec5`）。
* 判据：**2021 edition 下，凡 `if let` 条件里出现 `.lock()`（或任何需要尽早 Drop 的
  临时），体内绝不能再碰同一资源**——把值先绑出来。2024 edition 已收紧临时作用域，
  但 workspace 是 2021，这条永远适用。头less 测试抓不到：泵的 AppKit 路径不进测试。

## 19. 滚动时图标跳变 → 整目录低优先级预取（2026-09-23，接 §17/§18）

* 用户复测：图标自愈和占位图都生效了，但**滚动列表时图标会变**——新行进视口才
  `request`，泵异步取回后跳变（目录行是 NSFolder 占位 → 蓝文件夹，文件是描边 SVG
  → 系统图标），观感就是「图标不稳定」。
* 方案：**整目录预取**，不是视口+缓冲。图标（40px 档）极便宜（主线程 ~0.2ms/条、
  且同扩展名共享键去重后真实问系统次数远小于条目数），预取整个目录的成本可忽略；
  缩略图才需要 `want_thumbs` 那套「只画哪行派哪行」（1.5–12ms/张）。
* 实现（`mo-app/src/icon.rs` + `load_path`）：
  * `IconCache` 加 `prefetch: VecDeque` 低优先级队列，与实时请求共用 `asked` 去重。
  * `pop_next` 优先级：**新请求（用户正看的行）→ 到期重试 → 预取**，每条重新判，
    重试不被预取饿住、新行不被几千条预取堵住。
  * `load_path` 目录就位后（视图序）整目录入队 40 档（16pt 槽位）；入队前
    `clear_prefetch()` 作废上一目录残留。远程页跳过（本机没有那些文件）。
  * 泵逻辑零改动：预算 `ICON_BUDGET_MS=3`/拍照旧消化，预取只会把空闲的拍填满，
    不增加任何单拍主线程成本 → 不会卡顿。
* 边界：滚得比泵消化快时，最前面几百行已就位；后面行仍是「先 SVG 后真图」的短暂
  空窗，但预取按视图序入队 = 按滚动方向备货，正常滚动速度无感。切到网格/画廊的
  128 档仍走渲染路径 request（视口行才画，量小）。
