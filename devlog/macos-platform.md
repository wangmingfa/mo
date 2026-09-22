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
* 只接了主列表（`file_item::view` 加 `system_icon: Option<PathBuf>` 参数 + `file_list` 调用）；grid / columns 仍是内置 SVG，要一致再补。

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
