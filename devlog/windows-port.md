# Windows 移植的坑

日期：2026-09-25 ~ 09-26。涉及：gpui-pre 0.3.6、`windows` 0.58 / 0.62（别名）、Win32 Shell / SetupAPI / CfgMgr / GDI / 剪贴板 / 键盘布局、`IFileOperation`、WinRT `Windows.Data.Pdf`。

Mo 主打 macOS，Windows 这一路的规矩是：**契约不变，实现换**。平台层的对外形状（`PlatformError`、`IconRaster`、`Volume`）两端共用，上层因此一行没为 Windows 改过判断——除了一处例外，见 §6。

---

## 1. macOS 专用代码没有 cfg 门控：Windows 根本编不过

* **现象**：`cargo check -p mo-platform --target x86_64-pc-windows-msvc` 一次报 15 个 E0432/E0433/E0455。
* **根因**：`mo-platform/src/lib.rs` 里 `mod macos;` 是**无条件**声明的，而 objc / dispatch / libc 三个依赖只在 `[target.'cfg(target_os = "macos")'.dependencies]` 下引入——Windows 上模块被编译，依赖不在。
* **修法**：模块声明与函数分发都套 `#[cfg(target_os = "macos")]`。**只服务 macOS 的辅助代码要一并门控**，否则 `mod` 过了却留一堆 `dead_code` 告警（`mo-remote` 的 `spawn_with_stdin` / `shares_from_macos_mount` / `Stdio` import、`mo-ui` 测试辅助 `remote_menu` 就是这类）。
* **顺带的纪律**：跨平台的测试断言按 `any(macos, windows)` 放宽时，「这条平台确实不支持」的那半边要收成 `not(any(...))`，别留成 `not(macos)`——那样 Windows 上就永远测不到「不该支持的东西确实没支持」。

## 2. `explorer /select` ：路径形状与退出码都不可信

* **修法**：`crates/mo-platform/src/windows.rs:73` —— 把 `/` 统一换成 `\` 再传（`explorer` 对正斜杠的接受度随版本变，反斜杠是唯一稳的）。
* **⚠️ 别看 `explorer` 的退出码**：它成功时也经常回非 0，拿退出码判成败会误报失败（这条按已知行为写的，不是踩到才发现）。所以只判「进程起没起来」（`spawn()` 的 `Result`），起不来才是真失败。
* **测试红线**：单测里**绝不在 Windows 真调 `reveal`**（会弹资源管理器窗口），这条路径改由 `supports_reveal()` 覆盖。同一条红线后来扩展到 `eject`——真调一次就把用户的 U 盘下线了，所以测试只喂**不是盘符**的路径（Windows 实现对此也给 `Unsupported`，正好是要的语义）。见 `crates/mo-platform/src/lib.rs:389` 的 `unsupported_platforms_say_so` 文档注释。

## 3. COM 初始化：tokio 的 blocking 线程是复用的

* **坑的性质**：这条同样是**看调用位置挡住的**（没等到它变成崩溃报告）。风险不在 `CoInitializeEx` 本身，在它的**配对释放**。
* **根因**：shell 调用跑在 tokio 的 blocking 池里，而**同一批线程会被反复使用**。第二次 `CoInitializeEx` 回 `S_FALSE`——线程早就是 COM 线程了，这次调用**不拥有**初始化计数；照「init 成功就 uninit」写，就会把上一次的计数打穿，线程上的 COM 状态提前失效。
* **修法**：`ComGuard`（`crates/mo-platform/src/windows.rs:93`）——`owned = hr.0 == 0`，只有 `S_OK` 才在 `Drop` 里 `CoUninitialize`。注意 `hr.is_ok()` 把 `S_OK` 和 `S_FALSE` 都算成功，**不能**拿它当「我拥有的」判据。
* **WinRT 同理**：`ensure_winrt()`（同文件 `:931`）只 `RoInitialize(MTA)`、**从不 `RoUninitialize`**——同一个线程上重复 init/uninit 会把还活着的 WinRT 对象悬空，宁可让初始化计数留到线程结束。

## 4. `IFileOperation` 进回收站：`FOFX_RECYCLEONDELETE` 不许省

* **现象**：`PerformOperations` 返回 `S_OK`，文件却**直接没了**，资源管理器回收站里查不到。
* **根因**：文档说 `DeleteItem` 默认走回收站，实测默认就是真删。
* **修法**：`crates/mo-platform/src/windows.rs:133` 显式给 `FOFX_RECYCLEONDELETE`，连同 `FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOFX_EARLYFAILURE`（最后那个：需要提权的删除**直接失败**，而不是在 GUI 应用里凭空弹一个 UAC 框）。这是一条数据安全项，改动时不许省。
* **两个 GUID 别混**：`CoCreateInstance` 要的是 coclass 的 CLSID `{3AD05575-…}`，不是 `IFileOperation` 接口的 IID `{947AAB5F-…}`；crate 没导就自己 `GUID::from_u128` 钉一份（`:70`）。

## 5. 删除后「落点是哪」只能扫 `$I` 反查，而 `$Recycle.Bin` 有几千条

* **现象**：Windows 的 `IFileOperation` 不像 macOS 的 `trashItemAtURL` 直接给新路径，Mo 的账本要记落点就查不到。
* **根因**：回收站每卷一份（`<卷根>\$Recycle.Bin\<SID>\`），本体是 `$R<名>`，原路径存在配对的 `$I<名>` 元数据里；接口不回话。
* **修法**：`recycle_one` → `find_recycled`（`:114` / `:165`）：`$I` 在 `PerformOperations` 返回前就写好了，所以删完可以立刻扫。但废纸篓里可能有几千条旧条目，**逐条读 `$I` 正文一遍就是十几秒**——于是分 8 次尝试：前 4 次**只碰 mtime 不早于「现在 − 10 分钟」的** `$I`（`$I` 的 mtime 就是删除时刻；富余是给时钟偏差和批量删除的几秒差；`DirEntry` 的元数据在 NT 上随目录枚举一起回来，不额外花 I/O），仍不中再放宽成全量扫 4 次，每次间隔 50ms。比对读 UTF-16 原始路径而不是文件名——同名条目很多。
* **账本侧要配对处理**：条目显示名剥掉 `$R` 前缀（`mo-operations/src/trash.rs:268`），永久删除 / 还原 / 清空 / F2 改名都必须把配对的 `$I` 一起搬走或删掉（`:286`、`:341`、`:349`）。漏了的话资源管理器回收站里留幽灵条目，界面上一致性也对不上。

## 6. `Unsupported` 与 `Failed` 的分工，在推出这里有了实际后果

这是唯一一处**上层契约**跟着 Windows 变严的地方，值得单独记。

* **现象**：映射盘推出失败时，用户看到的理由是「系统错误 67」——系统原话（那个盘正被别的地方用着 / 已经断了）被我们补跑的那一次 `net use /delete` 盖掉了，侧栏还叠了一层「推出失败」前缀。
* **根因**：两件事混成一个了——「这条平台没实现」和「平台试过了、被系统否决」。上层原来一律按前者兜底。
* **修法**（`crates/mo-platform/src/windows.rs:364` + `crates/mo-app/src/lib.rs:1485`）：
  * `Unsupported` = 这条路平台没实现 → 上层**该**跑自己的办法（网络映射盘 → `net use /delete`，对那类盘那才是正解）；
  * `Failed` = 系统否决了 → 上层**不许**再兜底，直接把理由交给用户（补跑一次 `net use` 不但没用，还把真实理由换成一句系统错误码）；
  * 内置盘（`DRIVE_FIXED`）故意回 `Failed`：回 `Unsupported` 会让上层对内置盘去跑 `net use /delete`，那是错的动作。
* **系统把「为什么不行」说得比我们清楚**：`CM_Request_Device_Eject` 会带回否决类型与否决者名字（正被占用的进程 / 设备名），原样递出去——「还有程序开着它的文件：xxx.exe」比四个字「推出失败」有用得多。名字常和理由重复，重复时只留一个（`request_eject`，`:601`）；`PNP_VETO_TYPE` 只翻译常见几种，其余给编号——**猜错理由比不给理由更糟**（`veto_text`，`:629`）。
* **不需要管理员**：两处 `CreateFileW` 都以 **0 访问权限**打开句柄——用到的 `IOCTL_STORAGE_GET_DEVICE_NUMBER` / `IOCTL_STORAGE_EJECT_MEDIA` 都是 `FILE_ANY_ACCESS`，只为查询不为读写。

## 7. 卷宗枚举跑在渲染线程上：`GetVolumeInformationW` 能卡几秒

* **坑的性质**：这条是**读 API 文档 + 看调用位置挡住的**，开发机上没接光驱，没等到它变成用户报告。写在这里是因为它长得太像「正常写法」。
* **风险来源**：`AppState::volumes` 带 5s TTL 缓存，缓存过期时是**渲染线程**同步重算。列盘本身便宜（`GetLogicalDrives` 一次拿全位图、`GetDriveTypeW` 纯查表），但 `GetVolumeInformationW` 在**空光驱 / 空卡槽**上会阻塞等设备就绪，一秒到几秒——每 5 秒来一次就是界面周期卡顿。
* **修法**（`:279`）：光盘驱动器**不查卷标**（`kind == DRIVE_CDROM` 直接 `None`）——宁可少个名字，也不能每 5 秒卡界面一次。macOS 那份不成问题（`NSURL` 属性是系统缓存好的），这是「按 API 名字照搬、不看它在哪个线程上跑」才会踩的。
* **两个次要坑**：位图说它在、问的时候 `DRIVE_NO_ROOT_DIR`（枚举与查询之间被人拔了 U 盘）→ 跳过，别画一行点不开的条目；映射盘（`DRIVE_REMOTE`）**不在本机卷宗里**，归侧边栏「网络」区，与 macOS 那份分工一致（两处都列同一个盘只会让用户困惑）。
* **不给推不动的盘摆按钮**：`is_ejectable` 只放过 `DRIVE_REMOVABLE | DRIVE_CDROM`（`:346`）。

## 8. 系统图标：别拿到糊的，也别拿到黑边的

* **两段式问法**（`:698`）：先 `SHGFI_ICONLOCATION` 问出「图标躺在哪个文件的第几个资源」，再用 `SHDefExtractIconW(.., px)` **按目标尺寸**抽一张真图——shell 手上有 256px 真彩，只有这条路拿得到。这条没成才退回 `SHGFI_ICON`，那是 shell 直接给的 32px 小图，放大到 96px 画廊槽位会糊，但糊的胜过没有。
* **⚠️ GDI 的 DC 没有 alpha 通道**（画进去第 4 字节恒 0），直接读回来透明信息全丢、半透明边缘变黑边。修法：**画两遍**——黑底一遍、白底一遍——从两张的差里解出 alpha：`out = a·C + (1−a)·B`，黑底那份本身就是预乘值，`a = (black + 255 − white) / 255`（`premultiplied_from_backdrops`，`:877`）。好处是**不要求 `DrawIconEx` 真按 alpha 混合**：它若只认 1 位掩码，透明处两张分别还是 0 与 255，解出来照样是 0，只是边缘少个抗锯齿。交出的仍是预乘 RGBA，与 macOS 同一契约。
* **`HICON` 是调用者负责销毁**：两条路拿到的都要 `DestroyIcon`，漏一次就是每屏几十张的泄漏。
* **按扩展名问**（`SHGFI_USEFILEATTRIBUTES`，`:673`）：**不碰磁盘**，所以回收站里「本体已不在」的条目也拿得到真图标；反过来拿不存在的路径去问只会得一张通用白纸，还会把整个类型的共享缓存污染掉。文件夹占位图同理走类型解析（`:685`），不会因为「那个目录不存在 / 没权限」而拿不到。
* **图标泵的门禁换个问法**：`appkit_usable()` 那条在 macOS 上是「主队列有没有人 drain」的防挂死闩（测试进程必假），Windows 上 `SHGetFileInfoW` 压根不挑线程——照它问等于**永远不开泵**。于是新增 `icon_source_usable()`（`crates/mo-platform/src/lib.rs:339`）：macOS 沿用主队列判据，其它平台看 `supports_file_icons()`。

## 9. WinRT 渲染 PDF：交进流里的**不是裸像素**，是 PNG

* **现象**：首版按「流里是裸 BGRA8」去读，单测直接渲染不出首页。逐步打点的诊断测试（用完即删）显示：每一步 WinRT 调用都成功，可 `out` 流里只有 **605 字节**——而 100×200 **点**的一页按像素铺满是 133×267×4 ≈ 14 万字节。
* **根因**：`PdfPageRenderOptions` 默认带 `BitmapEncoderId`，也就是说流里是一张**按默认编码器编好的 PNG**（裸 BGRA8 是早年版 Windows 10 的行为）。
* **修法**（`decode_bgra`，`:1007`）：`stream.Seek(0)` → `BitmapDecoder::CreateAsync` → `GetSoftwareBitmapConvertedAsync(Bgra8, Premultiplied)`（顺手把格式钉死，免得碰上不带 alpha 的）→ `LockBuffer` → `GetPlaneDescription(0)` 拿 **stride** → `CreateReference().cast::<IMemoryBufferByteAccess>()` → 逐行按 stride 拷出紧凑行。两个细节：`GetBuffer` 的指针归那块内存缓冲引用管，**拷贝必须在 `reference` 出作用域之前完成**；行距通常大于 `w*4`（按字对齐），不等就当作渲染失败，别越界读。
* **`PdfPage::Size()` 给的是 96 DPI 像素，不是点**：100×200pt 的页回 133.33×266.67。按点算缩放会整体小一圈，测试也得按像素断言。
* **页面本身是透明的**（只画文字笔画），如实编码就是一张黑纸。`PdfPageRenderOptions::SetBackgroundColor` 在 0.62 里挂在 `UI` feature 下（没引），也不必引——照 macOS 那份的做法铺白：预乘值合成到白是 `c + (255 − a)`，合成完处处不透明，alpha 拉满（`opaque_rgba_over_white`，`:1070`）。通道同时换成 `IconRaster` 约定的 RGBA。
* **依赖版本**：`windows_pdf` = `windows` 0.62 的**别名**，只服务这条路（`crates/mo-platform/Cargo.toml`）。0.58 没有能**同步等完**的 `IAsyncOperation::join()`（`windows-future` 0.3.2 才有，且不需要执行器），而刚上线的 Shell / SetupAPI / GDI 全按 0.58 签名验过并上线了，为一个功能把整模块重签不划算。两版同时链进来，类型互不相干，各自只待在自己的小模块里。
* **WinRT 异步在 Rust 里的形状**：`*Async` 是**同步函数返回 `Result<IAsyncOperation<T>>`**，`.join()` 才等结果——没有 `IntoFuture`，`await` 不了（0.61/0.62 都是这个形状）。整条路只回答「有没有拿到一张图」，所以错误一律 `.ok()?` 收成 `Option`，不值得为它维护错误链。

## 10. 两段式预览的第二拍对 PDF 从来没启动过（两个平台都中）

* **现象**：按空格预览 PDF，永远停在「正在渲染首页…」。macOS 上也一样。
* **根因**：拆第二拍时读的是 `pv.image`，而 `mo-preview` 对 PDF **恒给 `None`**（`crates/mo-preview/src/lib.rs:104`——它只认类型，渲染归平台），于是 `take()` 出来永远是空，首页渲染根本不会被叫起。图片恰好有 `image`，所以这个 bug 只对 PDF 显形。
* **修法**（`crates/mo-ui/src/app.rs:8502`）：`split_preview_for_two_pass` 多收一个源路径参数，PDF 分支交 `PreviewSecond::Pdf(path)`；四处调用点各自传自己手上那份路径（列表预览 / 回收站 / 搜索结果回车 / 快速预览）。
* **教训**：跨模块传「谁负责填这块」时，判据不能是「这块现在有没有值」，得是**类型**。钉住它的单测：`two_pass_split_hands_over_image_and_pdf_jobs`（喂一个 `kind = Pdf`、`image = None` 的 `Preview`，断言第二拍拿到 `Pdf(path)` 且占位文案留在第一拍里）。

## 11. 黑框闪两下：一次是主进程，一次是子进程

* **现象一**：双击 `mo.exe` 启动时先弹一个控制台窗口。
* **修法**：`crates/mo-ui/src/main.rs:3` —— `#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]`。gpui 的 Windows 构建默认按控制台子系统链接（`cargo run` 时要看 print），发布产物是 GUI 应用，该声明成 GUI 子系统。
* **现象二**：主界面改成 GUI 子系统后，**在应用内操作**仍每隔几秒闪一下黑框——侧边栏每次刷新网络盘就一次。
* **根因**：GUI 子系统进程拉起 `net.exe` / `cmd.exe` 这类**控制台**子程序时，系统会为它们新建一个控制台窗口。
* **修法**：给所有「输出本来就走管道」的命令统一加 `CREATE_NO_WINDOW`——网络盘刷新走 `hide_console` 这个小 helper（`crates/mo-remote/src/mount.rs:446`，唯一调用点就是刷新用的 `net.exe`），自定义命令执行另加一处（`crates/mo-app/src/usercmds.rs:188`）。`open_terminal`（`crates/mo-app/src/lib.rs:4359`）**有意不加**：那条就是要给用户开一个终端窗口。

## 12. 只在 Windows 上崩：UIA 一挂载就 `Duplicate a11y node id`

* **现象**：Windows debug 构建里「操作时有概率崩」，退出码 `0xc0000409`（STATUS_STACK_BUFFER_OVERRUN），panic 指向 gpui 的 `window/a11y.rs`：`Duplicate a11y node id`。macOS 上同样操作不崩。
* **根因**：`text!` 的默认元素 id 是**宏调用点**的 FNV 哈希。同一处宏在循环里画多个兄弟节点（表头四列、回收站行元数据、去重路径、高亮命中段、连接认证标签），祖先又没有各自的 id，整串就折叠出相同的 a11y NodeId。macOS 不崩只是因为**没开无障碍就没有 a11y 树可挂**；Windows 上 UIA 客户端一挂载（辅助功能激活）断言立刻炸。
* **修法**：逐迭代给显式 id —— `text!(id = format!("…-{i}"), ..)`，与 `file_item::meta_cell` 的先例一致。规则本身记在 [gpui-layout-and-interaction.md §28 末尾的 ⚠️](gpui-layout-and-interaction.md)（占位行那个坑是同一条），这里只补一句：**判据是「这条宏会不会在循环里被展开多次」，不是「这屏看不看得到」**。
* **附带的测试坑**：同一个修复轮里 `dialog_header` 布局测试要跟着改——卡片加了 1px 边框后标题栏 quad 缩进边框内侧，断言改成「同缩 1px、宽少 2px」，别再要求与卡片逐像素同界。

## 13. 验证：CI 只跑 macOS，锁屏也截不了图

* **CI 现状**（`.github/workflows/ci.yml`）：job 只在 macOS 上，Windows 代码由**本地 `cargo check/test` + release 构建**把关。所以上面这些坑没有一个能被 CI 兜住，全靠像素断言。
* **锁屏时无法截图验证**：`CopyFromScreen` 在屏幕锁定下只会拍到锁屏画面。这轮的验收全部改成**程序级**：手写一份 xref 偏移正确的最小 1 页 PDF 喂真 WinRT，逐像素断言（96 DPI 尺寸、蓝方块的 bbox 与大小、通道换序 → 红通道必须为 0、白纸占比、`max_edge` 缩放档）。见 `crates/mo-platform/src/windows.rs` 的 `renders_the_first_page_of_a_real_pdf`，与 `crates/mo-app/tests/pdf_preview.rs`（渲染产物落 `<cache>/pdf-preview`、缓存命中、mtime 失效）。
* **屏幕没锁的时候有比像素更硬的手段**：UIA 直接把窗口里每个节点的 `Name` + `BoundingRectangle` 全打出来（`FindAll(Descendants, TrueCondition)`），文案对不对、某一行在不在列表里、要点哪个坐标，一次拿全；图标糊不糊再配 `CopyFromScreen` 裁格子逐块 MD5。这台机器 1920x1080 @100%，截图坐标与 `SetCursorPos` 一比一。⚠️ 锁屏状态**每轮现测**，别沿用上一轮的结论——把「你去解锁一下」当成事实，等于让用户替 agent 验证。
* **测试隔离**：`MO_CACHE_DIR` 是进程级变量，同二进制内多线程并行必须**串行化**（`ENV_LOCK`）。`pdf_preview_root` 这轮也补上了认它（`crates/mo-app/src/lib.rs:3046`）——和 `index_path` 同一条理由：不认的话新增测试会往开发者机器的真实缓存里写。

## 14. 隐藏文件：点开头是 Mac 的习惯，Windows 得看属性位

* **现象**：`desktop.ini`、`ntuser.dat`、`AppData` 在 Mo 里全是可见的——它们不以 `.` 开头，而 `is_hidden_name` 只认这一条。macOS 上同一件事由 `st_flags & UF_HIDDEN` 兜住（`chflags hidden` 的文件不靠改名）。
* **修法**：`is_hidden_with_metadata`（`crates/mo-fs/src/lib.rs:192`）按平台补一条属性判据，Windows 侧 `hidden_by_attributes`（`:222`）取 `FILE_ATTRIBUTE_HIDDEN (0x2) | FILE_ATTRIBUTE_SYSTEM (0x4)`——用 `std::os::windows::fs::MetadataExt::file_attributes()`，不为这个再引一个 `windows` crate 依赖。
* **为什么 SYSTEM 也算**：`desktop.ini`（0x26）、`ntuser.dat`（0x2022）都是 Hidden|System，资源管理器也不显示；只看 HIDDEN 位就会漏掉这一批。反过来 **Archive（0x20）不算隐藏**——桌面上那些 `.lnk` 全是 0x20，把它们当隐藏等于把用户的快捷方式藏了。
* **⚠️ 这一折让 Mo 与资源管理器不再逐条一致**：点开头但不是隐藏属性的文件（`.ssh`、`.cargo`）Windows 上仍按隐藏处理，这是刻意的跨平台约定，不是漏判；写进 `is_hidden_name` 的文档注释里，免得下一个人「顺手修好」。见 `crates/mo-fs/src/local.rs` 的 `windows_tests`（用 `attrib +h/+s` 造真属性，逐条钉死 0x26/0x12/0x4 判隐、0x20/0x10 不判隐）。

## 15. Shift + 符号键在 Windows 上永远打不中

* **现象**：`Ctrl+Shift+.`（显示/隐藏隐藏文件）按了没反应，同一个窗口里 `Ctrl+Shift+P` 好使。同一段代码，一个字母键、一个符号键——差的就是符号。
* **根因**：gpui 的 Windows 后端在 `keyboard.rs::get_keystroke_key` 里遇到 Shift + OEM 符号键时，把 `key` 换成 Shift 后的字符（`.` → `>`）**并且把 Shift 位清零**（`need_to_convert_to_shifted_key` 那张表覆盖 `VK_OEM_*` 和 `VK_0..9`）。配置里写的 `cmd+shift+.` 于是**差两位**：主键多了、Shift 没了，严格比较永远不命中。macOS 后端不改写字符也不清 Shift，所以同一份键表在 Mac 上一直是对的。
* **修法**：`fold_typographic_shift`（`crates/mo-ui/src/keys.rs:59`）扩成完整一对表（`{[`、`}<`、`>.`、`:;`、`"'`、`?/`、`|\`、`~\`` 加上原有的 `+=`、`_-`），**表两头都算**、折回基本键并把 Shift 位归掉。折只发生在比对用的 `canonical()`（`:108`）里，`matches()`（`:123`）比它——`parse` 不再就地折，否则用户写的 `cmd+shift+.` 显示成「Ctrl+.」，提示文案就成了按不出来的指令。
* **为什么数字不进表**：`cmd+1`~`cmd+4` 是视图模式，把 `!` 折成 `1` 等于让 `Ctrl+Shift+1` 顺手切视图——白送的宽容，代价是误触。字母键同理不进表（`cmd+z` 撤销 / `cmd+shift+z` 重做必须分得开）。
* **回归测试**：`windows_shifted_symbol_events_hit_symbol_bindings` 直接造 Windows 形状的按键事件（key 是 `>` / `{` / `}`、Shift 位已被后端吃掉）查表，不靠真键盘。
* **尾巴**：这一节的表只描述 **US 布局**，非 US 键盘上会串键——修法见 §22。

## 16. 测试把用户机器上的真实配置写了

* **现象**：跑完 `cargo test --workspace`，开发者自己的「显示隐藏文件」被悄悄关掉，重启 Mo 才发现。
* **根因**：`crates/mo-app/tests/hidden_files.rs` 里两个测试都按「读真实配置 → 切开关 → 测完恢复」写，而它们跑在**同一个进程、默认多线程并行**。B 构造 `AppState` 时盘上正被 A 写成 `false`，B 记下的「原始值」就是 `false`，收尾把 `false` 留给了用户——恢复动作本身是竞态读出来的，等于没恢复。
* **修法**：与 §13 同一条纪律：`MO_CONFIG_DIR` 钉到临时目录 + 按测试名分目录 + `ENV_LOCK` 全程互斥（该文件 `use_temp_config`）。钉住之后「恢复原值」这段直接删掉——临时目录里没有需要保护的用户状态。
* **纪律**：任何**落盘**的状态（配置、缓存、索引）在测试里都必须先钉目录。判据是「这个 setter 会不会写 `~` 下的东西」，不是「这个测试看起来无副作用」。

## 17. 文案里写死的 ⌘：绑定是对的，提示是假的

* **现象**：Windows 上状态栏写着「⌘⇧P 命令」、命令面板写着「复制选中（⌘C）」——这台机器上根本没有 ⌘ 键。
* **根因不是键位**：`keys.rs` 早就把 `cmd` 与 `ctrl` 折成同一个位（非 macOS 看 `modifiers.control`，因为 gpui 的 Windows 后端把 `platform` 映射成真的 Win 键），所以快捷键按得出来；漂掉的是**第二份抄件**——31 处手写 `⌘` 字符串散在 `app.rs` / `status_bar.rs` / `staging.rs` 的文案里。抄的那份一定会漂。
* **修法**：文案一律现推。有 id 的走 `hint(id)`（`crates/mo-ui/src/keys.rs:633`，从键表拿当前生效的键组再 `format()`，用户改过键也跟着变）；没有 id 的裸键串走 `key_hint(spec)`（`:647`，解析不了就原样返回，绝不让一处文案变成空串）。`KeyCombo::format()` 在非 macOS 写 `Ctrl+Shift+P`，在 macOS 写 `⌘⇧P`。
* **测试**：`ids_used_in_prose_have_hints` 钉住文案里用到的每个 id 都能查出非空提示（拼错 id 以前是静默少一段文案）；`key_hint_renders_bare_specs_per_platform` 钉住 `⌥A` → `Alt+A`、`⌘⇧P` → `Ctrl+Shift+P`。

## 18. 已知文件夹：Windows 上该叫「桌面」，不是 `Desktop`

* **现象**：侧栏写「桌面」、标签页与面包屑写 `Desktop`，同一个文件夹两个名字；而且盘符在别处时（这台机器桌面在 `D:\Users\…\Desktop`）按 `C:\Users\…` 拼出来的路径根本对不上。
* **修法**：一张表两处用——`known_folder_labels()`（`crates/mo-app/src/lib.rs:710`，`OnceLock` 包住 `dirs::*_dir()`）既喂侧栏快捷访问，也喂 `folder_label()`（`crates/mo-ui/src/path_label.rs:32`）；标签页、面包屑、列视图、侧栏都改走它，比较时 Windows 忽略大小写（`same_dir`，`:52`，顺带吃掉尾部 `\`）。
* **为什么不用 `SHGetDisplayNameOfW`**：那是**逐段**问 Shell 要名字，面包屑每帧都要重算，等于每帧一次跨模块调用；而且它给的是本地化 + 用户改过的名字，跟侧栏那张表又会分叉。显示名与真实名分离这条（`mo_core::display_name`）本来就是同一个哲学：只改画法，路径一个字符都不动。

## 19. 系统剪贴板「进来」：gpui 早就报了，Mo 一直没读

* **现象**：资源管理器里 Ctrl+C 一批文件，回 Mo 粘贴毫无反应。Mo 的文件剪贴板是**纯进程内部**的那份（`Clipboard` 只在应用里传路径），而 gpui 的 `read_from_clipboard` 在 macOS / Windows 上都会把系统剪贴板里的文件报成 `ClipboardEntry::ExternalPaths`——这一支 Mo 从来没看。
* **剪切还是复制，gpui 报不出来**：那一位 Shell 记在 `Preferred DropEffect` 这个剪贴板格式里，gpui 没透出。Windows 侧平台层补一手读（`clipboard_files_are_cut`）；**macOS 一律答复制**——Finder 的剪切是一条私有 pasteboard 标记，公开可读的类型里没有这一位。判不准时为什么必须偏向复制：猜成剪切会把用户的文件**搬走**，猜反了只是多留一份。
* **顺带挖出两个老错**（`crates/mo-app` 的粘贴路径）：
    * 复制分支调的是 `copy_selection`（当前**选区**），于是「复制 → 换目录 → 粘贴」粘的是新目录里选中的东西，选区一空就粘出 0 项。两支现在都按剪贴板那批走 `transfer_between`。
    * 源端点取的是「当前浏览的那一端」：从资源管理器复制的本机路径、粘进正在浏览的 FTP 会话，会去服务器上找一个不存在的文件。源端点改记在剪贴板里（`Clipboard::src`）。
* **验证**：真机跑通「资源管理器复制 → Mo 粘贴」「资源管理器剪切 → Mo 粘贴（原处文件搬走）」，以及反向的 Mo→Mo。

## 20. 系统剪贴板「出去」：gpui 的后端对 `ExternalPaths` 是 `=> {}`

* **现象**：§19 让 Mo 能粘别人复制的，反向仍是断的——Mo 里 Ctrl+C 之后去别的程序粘贴，什么都出不来。原因写在 gpui 里：`write_to_clipboard` 遇到 `ClipboardEntry::ExternalPaths` 直接丢弃，两个平台的后端都写着 `=> {}`。
* **修法**：Windows 侧自己写原生那套。`GlobalAlloc(GMEM_MOVEABLE)` 一块，按 `DROPFILES` 摆 20 字节头（`pFiles=20`、`pt=(0,0)`、`fNC=0`、`fWide=1`），后接 UTF-16 路径表、每条各自 NUL 结尾、整表再空一项收尾；`SetClipboardData(CF_HDROP=15)`。同时写 `Preferred DropEffect`（复制 `0x1` / 剪切 `0x2`），否则对方程序把剪切当复制，粘完原文件还留着。
* **字节必须钉死**：头部偏一格、或结尾少一个 NUL，Mo 自己一切正常，只有别的应用读不出（资源管理器直接灰掉「粘贴」）。`dropfiles_bytes` 的测试按字节断言，不按「能读回来」断言。
* **两处内存所有权**（都写在注释里，这类错编译不了也测不出）：`SetClipboardData` 成功之后 HGLOBAL 归系统，再 `GlobalFree` 是双重释放；反过来它**失败**时必须释放，不然每次复制漏一块。`OpenClipboard` / `CloseClipboard` 用 Drop guard 配对，中间任何 `?` 早退都不会漏下锁——剪贴板被进程独占住，别的程序会连粘都粘不了。
* **上层不许报错**：`supports_file_clipboard()` 为假（macOS 没做）或写入失败时，应用内粘贴照走自己那份剪贴板，UI 不能因此说「复制失败」。

## 21. 缓存隔离补漏：`MO_CACHE_DIR` 只认了一半

* **现象**：`isolate_config_for_tests()`（§16 那个 helper）只挡了 `MO_CONFIG_DIR`，缓存那半是漏的。`AppState::new()` 一开就接上 `<缓存>/mo/search.sqlite`，测试爬过的每个临时目录都被写进**开发者机器上的真索引**——回头按搜索键找文件，会搜出一批早已被删掉的 `mo-layout-trash-*` 之类的脏根。缩略图 / 预览图 / `metadata.sqlite` 同理，每个会渲染图片的用例都在往真实缓存落文件。
* **修法**：`mo_cache::cache_dir()`、`mo_thumbnails` 的 thumb / preview 两个根都认 `MO_CACHE_DIR`（和 `AppState::index_path` 同一条理由，之前只有它认）；helper 改名 `isolate_user_dirs_for_tests()`，一次钉两个目录——**名字只说 config 会让后来人以为隔离已经做全了**。
* **验证方式**：不看日志看 mtime。跑 `cargo test -p mo-ui`（33/33）前后 `%LOCALAPPDATA%\mo\search.sqlite` 与 `metadata.sqlite` 都没动，临时目录里多出 `mo-test-cache-<pid>`。
* **没修完的部分**：那一轮只改了 `mo-ui` / `mo-platform` 用的 `isolate_user_dirs_for_tests()`，`mo-app` 的十余个集成测试各自 `set_var` 之外不钉缓存，仍在写真实 `search.sqlite`。这一条在 §26 收口。

## 22. §15 那张折表只认 US 布局：改成问当前键盘布局

* **为什么要改**：「哪个符号是哪个键的 Shift 变体」是**键盘布局**的事，不是常理。2026-09-26 用 `VkKeyScanExW` 实测两种布局的配对（`.tmp/vk.ps1`，同一套 API）：US 上 `+` 是 Shift+`=`、`?` 是 Shift+`/`；德语上 `+` 自己就是一个键、`:` 才是 `.` 的 Shift 变体、`"` 是 Shift+`2`、`{` 要 AltGr+`7`、而 `?` 那个键未加 Shift 打出的是 `ß`。照 US 表在德语上折，等于把用户按下的键绑到另一个动作上，默认键位反倒按不出来。
* **修法**：`mo_platform::unshifted_key(ch)`——`VkKeyScanExW` 查出这个字符要按哪个虚拟键、什么 Shift 态（只认「无 Shift」与「Shift」两种，AltGr / Ctrl / Alt 出来的一律不答），再用 `MapVirtualKeyExW(vk, MAPVK_VK_TO_CHAR)` 取该键**未加 Shift** 的字符；死键（bit15）与非 ASCII 结果都不答。`fold_typographic_shift` 先问布局、问不到才退 US 表。
* **两条不明显的规则**：
    * 布局**答了就不再退表**，哪怕答的是数字或字母（德语上 `"`→`2`）：表里那条 `"`→`'` 在这一列是错的，拿它兜底就把两个不相干的键又并到一起。
    * 「基本键也得是符号键」这条规则从「表里不记数字」改成了真判据（`is_symbol_char`），因为布局会给回数字（`!`→`1`）——视图模式占着 `cmd+1`~`cmd+4`，不判就白送一次误触。
* **macOS 已补（2026-09-30）：macOS 上 gpui 的 `Keystroke` 也只给字符、不给虚拟键码，
  不能直接 `VkKeyScanExW` 反查键码，于是反过来——`TISCopyCurrentKeyboardLayoutInputSource`
  拿当前布局的 `uchr` 表，枚举 128 个虚拟键码（不带 Shift / 带 Shift 各一次），用
  `UCKeyTranslate` 解「基本键 → Shift 变体」映射，再把 `ch` 折回去（`macos::unshifted_key`）。
  ⚠️ **坑**：`UCKeyTranslate` 的 `modifierKeyState` 用的是 `uchr` 自己的 `UCKeyModifiers`
  修饰位表，**Shift 位是 `0x0002`（bit 1），不是** EventRecord 的 `shiftKey`(0x0200)——
  后者在本 API 下完全不生效（实测所有键 `base == shift`，正是这个坑；`0x0004` 是 caps-lock，
  只移字母不移数字，用来交叉确认）。语义与 Windows 版对齐，非 US 布局的 mac 用户不再串键。
  单测 `unshifted_key_resolves_shift_direction_on_current_layout`：只钉「Shift 方向没接反」——
  基本键映射到自己、大写→小写（这一层在几乎所有 Latin 布局都成立，足以证明 `uchr` 的 Shift 位
  `0x0002` 没接反）。非 US 布局的符号真值（德语 `:`→`.` 等）不在主要支持范围，不单独验；要自测
  让用户在真机跑 `cargo test -p mo-platform unshifted_key` 看 `ok` 即可。
  ⚠️ **并发坑**（保留给后来人）：两条 `unshifted_key` 测试并行跑时各自进
  `TISCopyCurrentKeyboardLayoutInputSource` + `UCKeyTranslate`，这套 HIToolbox 布局查询在无 GUI
  会话的并发下会 **SIGABRT**；所以这条必须保持**单测**，别为了「分两条更清楚」而拆开。
* **Linux 已补（2026-10-08）：** 用 **libxkbcommon** 读当前 XKB 布局的 Shift 变体映射
  （`linux::unshifted_key`，`#[link(name = "xkbcommon")]` + 手写 `extern "C"`，进程级
  `OnceLock` 缓存映射表）。对每个键码分别取「无 Shift / 按住 Shift」打出的字符
  （`xkb_state_update_modifiers` 置 Shift 修饰位 + `xkb_state_key_get_utf8`，比直接查 level
  更稳——自动处理各布局的 level/Shifts 数），建 `Shift 变体 → 基本键` 表。`XKB_DEFAULT_LAYOUT`
  等环境变量可切布局（测试里靠它切德语）。拿不到 libxkbcommon / XKB 数据时 `build_map` 返 `None`、
  `unshifted_key` 退化成 `None`（上层用 US 表），**不因此让二进制起不来**。
  单测 `unshifted_key_follows_german_layout`：`XKB_DEFAULT_LAYOUT=de` 下 `?`→`ß`（US 布局则来自 `/`，
  足以证布局感知）、`A`→`a`、`!`→`1`、基本键映射到自己；无 XKB 数据环境跳过重断言。
  ⚠️ **交叉编译怪象**：从 macOS host 跑 `cargo clippy --target x86_64-unknown-linux-gnu`
  `-D warnings` 偶发不套用 workspace 的 `unsafe_code = "warn"`，把 FFI 升成 deny；真机 Linux
  native 构建与 macOS native 同行为（正常放行）。为保险 `linux.rs` 顶部加 `#![allow(unsafe_code)]`
  兜底，纯 FFI 模块属合理范围。
* **单测不碰真布局**：`fold_typographic_shift` 在 `#[cfg(test)]` 下把查询退化成 `None`（开发机插什么键盘不该决定断言红绿，与 §16/§21 同一类卫生）；非 US 的行为由 `fold_with` 那组测试喂**假布局**覆盖。`mo-platform` 侧按 KLID 显式 `ActivateKeyboardLayout` 后实测 US 与德语两列答案，装不上布局的机器跳过并 `eprintln!`。
* **实机验证撞到的两件事**（记下来，免得下一个人重踩）：
    * gpui 算字符走 `ToUnicode(vk, lParam 高字节的扫描码, state)`，它只看**当前线程**的布局。而 `SetForegroundWindow` 一激活窗口，Windows 就把该线程的布局打回这个窗口登记的输入语言（本机是 `0x0804` 中文）；`WM_INPUTLANGCHANGEREQUEST` 想切德语（`00000407`）也不落地——不在用户「输入语言列表」里的布局，Shell 不给切。所以**没能让 Mo 的线程真变成德语键盘**，GUI 上的德语端到端这一轮没验成。
    * 于是改成验**同一条 API 链**：`to_unicode_and_unshifted_key_agree_on_the_same_layout` 在进程内激活德语后，照抄 gpui 的调用形状（`state[VK_SHIFT]=0x80`、8 个 u16 的缓冲、flags `0x5`），`ToUnicode(VK_OEM_102, 0x56, Shift)` 交出 `>`，`unshifted_key('>')` 答回 `<`；同一个键在 US 上交出 `|`、折成 `\`。真实德语用户的 Mo 线程从进程起来就是德语，走的是这一条。
    * GUI 侧只验了**不回归**：US 线程上 `Ctrl+Shift+.` → Mo 收到 `>` → 折成 `.` → 隐藏文件照常切换（第二轮再按一次收回）。

## 23. paint 阶段的 `cx.notify()` 不叫醒帧循环：首次进回收站空白一秒

* **用户报**：首次进入回收站先是一片空白，几百毫秒到 1 秒后才冒出斑马纹；要求「一进来就有，不管有没有文件」。
* **机制**：斑马纹补足行的条数要靠 `on_prepaint` 量回来的内容区高度（§见回收站面板那段注释），所以**首帧没有补足行是设计使然**。坏在第二帧永远等不到：prepaint 里 `set_trash_body_h` 写完高度后 `cx.notify()`，gpui 只把视图**标脏**，不会去 `schedule_frame` / 唤醒平台帧循环——进程没有下一个事件就没有下一帧。于是「高度已经拿到了，但没人重画」，条纹只能等条目到货、图标扫描回来或用户动一下鼠标。
* **日志证据**（`RUST_LOG` + 锁屏下用 `SendMessageTimeout` 投递点击，见 `.tmp/postclick.ps1`）：
    * 修之前：`render body_h=0 fill=0` → `prepaint h=607 changed=true` → **之后再没有 render**，直到别的事件进来。
    * 修之后：`render body_h=0 fill=0`（.112）→ `prepaint h=607 changed=true`（.121）→ `render body_h=607 fill=24`（**.129，7 毫秒后**），中间没有任何外部事件。
* **修法**：`window.request_animation_frame()` —— gpui 自己给动画元素用的那条唤醒（内部 `on_next_frame` → `schedule_frame` + `invalidator.wake_platform`，回调里再 notify 一次当前视图）。只在高度**变了**时调，第二帧起高度恒定，不会自续成满帧重绘。文件列表的 `list_origin` 是同一个模式、同一个缺口（首次进目录的补足斑马纹也靠边等事件），一并加上。
* **测试怎么钉**：headless 里「下一帧」是测试自己调的，`render_frame` 两次就能让旧代码看起来是对的（既有的 `trash_zebra_stripes_fill_the_viewport` 就是这个形状）。所以新测试 `trash_zebra_fill_requests_its_own_second_frame` 断言的不是布局，是**首帧有没有替自己排队**：先把队列排空（`simulate_next_frame` 会顺手把文件列表那条唤醒吃掉），进回收站画一帧，再数 next-frame 回调数——必须 ≥1。反向验证做过：把那一行注释掉，测试红；放回去，绿（空回收站、0 条目，补足行铺到 y>550）。
* **仍未验**：锁屏状态下拍不到图，所以「肉眼看不到那 7 毫秒」这条只有日志，没有截图。

## 24. 从资源管理器拖进 Mo：`on_drop::<ExternalPaths>` 三处落点

* **做到哪一步**：这一轮只做**进来**（OS → Mo）——目录行、窗格空白处、侧栏快捷访问、侧栏回收站四个落点，拖上去有 hover 高亮。应用内的拖动早就通了；**出去**（Mo → 资源管理器）还断着，卡在 Windows 没有 `DoDragDrop`，见文末待办。
* **gpui 已经把活干了一半**：`gpui-pre` 的 Windows 后端实现了 `IDropTarget`（`CF_HDROP`），拖进窗口就翻译成三件事——`Entered{position, paths}` 挂一个 `active_drag`（值就是 `Arc<ExternalPaths>`）并合成一次左键 `MouseMove`；`Pending` 继续合成 move；`Submit{position}` 合成一次左键 `MouseUp`。所以 UI 侧只要 `on_drop::<ExternalPaths>` + `drag_over::<ExternalPaths>`（hover 上色），从「事件进窗口」到「文件落盘」这条链在 headless 测试里也是真的。
* **⚠️ 坑一：不能用 `can_drop` 做「非目录拒绝」**。gpui 在判定可行性**之前**就把 `cx.active_drag.take()` 掉了（`div.rs` 的 MouseUp 分支：命中、TypeId 对得上就先取走）。用 `can_drop` 返回 false 来拒，事件会被**吞掉且不再冒泡**，窗格空白处那层永远收不到。正确写法是**只在接得住的元素上注册 `on_drop`**——目录行注册，普通文件行一个监听都不挂，让事件自然冒泡到窗格容器。
* **⚠️ 坑二：外部拖进来只能复制，做不了移动**。`Submit` 合成 MouseUp 时把修饰键写成 `Modifiers::default()`，Alt / Shift 全丢，应用内那套「按住 Alt 就是移动」的判断在外部拖放里根本没有输入。所以 `submit_os_drop` 恒传 `move_ = false`。这不是偷懒：判不准时偏向复制，猜成移动会把用户的文件**搬走**。
* **⚠️ 坑三：测试里导航会被启动流程盖掉**。`tab_loop` 启动时异步「按需打开 Home」，它比 `navigate_for_tests` 慢到：面板一度停在临时目录、`list_count` 也对，随后 Home 落地把窗口快照整个换掉（`window.len()` 归 0，`mo-file-row-0` 消失）。修法是**先等 Home 那次打开落地、再导航**，并且断言用「行数精确相等」（`panel_window_ready_for_tests(rows)`）而不是「≥1 行」——后者会被 Home 的内容蒙过去。另：`AppState::opening_path()` 在读取完成时发布成 `None`，不能当导航判据，新加了 `panel_path_for_tests`（读面板自己的 `path`）。
* **验证**：`crates/mo-ui/tests/os_drop.rs` 三个 headless 用例各投一次真的 `FileDropEvent`（`Entered` + `Submit`，经 `to_platform_input()` 走 `dispatch_event`）——拖到目录行 → 文件出现在那个子目录且**源文件留着**；拖到窗格空白 → 进当前目录；拖到回收站 → 源文件消失。反向对照三处：把对应注册点注释掉，三条测试各自红。
* **侧栏快捷访问没进测试**（代码里挂了监听，但没有用例）：`quick_locations` 用的是真 `dirs`，`isolate_user_dirs_for_tests` 只钉配置和缓存两个目录，钉不住 Desktop/Documents。在开发机上跑这个用例等于往用户的真实桌面写文件。**这条缺口是已知的，不是漏测。**

## 25. 把文件拖出 Mo：Windows 的 OLE 拖拽源，跑在一条没有窗口的线程上

* **gpui 这条路走不通，两条各自的原因**：① `WindowPlatform::start_external_drag` 的 Windows 实现压根没有，走的是 trait 默认的 `false`（`gpui-pre-0.3.6/src/platform.rs:1009`）；② 触发它的那段 `promote_external_drag_to_platform`（`window.rs:5585`）要求这次拖拽是 gpui 自己的 `on_drag` 起的——它取的是 `cx.active_drag.external_payload_source`，而 Mo 的拖拽是手写鼠标事件来的，gpui 不知道有一次拖拽正在进行。把 Mo 迁到 `on_drag` 不是不行，但它会撞上 §24 坑一那条派发行为（`active_drag` 在命中判定之前就被 `take()` 掉），而这正是「拖进来」现在依赖的。所以 Windows 这半自己接 OLE；macOS 那半同样没接（gpui 那边 `beginDraggingSessionWithItems` 是实现了的，可它同样要 `on_drag`）。
* **⚠️ `DoDragDrop` 不能在 UI 线程上调**：它是模态的——自己起一个消息循环一路泵到落子。而 Mo 的起拖点在 gpui 的输入回调里，那时 `App` 内部的 `RefCell` 正被 `borrow_mut` 持有（`app.rs:99`，普通 `borrow_mut`，不是 `try_`）。鼠标链本身有防重入（`callbacks.input.take()`），但 OLE 的循环里只要派发进一条 `WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD`（`dispatcher.rs:125` → `platform.rs:1091` → `execute_runnable:1190`，这条**没有**任何守卫）就是二次借用，直接 panic。于是拖拽活在**单独一条 STA 线程**上：`CoInitializeEx` + `OleInitialize`（拖拽代理注册表是 per-thread 的，gpui 在 UI 线程上做过一遍不算这条线程的），OLE 只泵这条线程的队列，碰不到 gpui 的隐藏窗口消息，UI 线程照常渲染。
* **数据对象不自己写**：`ILCreateFromPathW`（每条路径一个 PIDL）→ `SHCreateShellItemArrayFromIDLists` → `BindToHandler(BHID_DataObject)`。这样递出去的不止 `CF_HDROP`，还带 `FileGroupDescriptor`（对面能看懂「一个文件夹里的若干文件」）和拖拽图像；手搓 `IDataObject` 只给得到 `CF_HDROP`，落到资源管理器以外的目标上会明显掉相。任一条路径 shell 不认（不存在 / 名字非法）就**整批不拖**——拖出「一半成功一半 404」比不拖更糟。
* **`IDropSource` 的落子判据取「或」**：OLE 递来的 `grfKeyState` 里有 `MK_LBUTTON`，但这条线程没有窗口也没拿到鼠标捕获，那一位偶发缺帧；所以再问一次 `GetAsyncKeyState(VK_LBUTTON)`，**两条都认定已抬起**才回 `DRAGDROP_S_DROP`。误判成「还按着」只是多问一轮，误判成「已抬起」会把文件掉在指针底下随便哪个窗口上。Esc 先认，立即 `DRAGDROP_S_CANCEL`。
* **复制还是移动交给对面**：`DoDragDrop` 只声明 `COPY|MOVE` 两个都允许，具体哪个由目标按 Ctrl / Shift 和是否跨盘定——按键状态 OLE 自己会递过去，比 Mo 在 UI 线程上猜准。
* **回了 `MOVE` 就得自己删源**（OLE 的约定：目标只负责在原地放一份，搬走源是源端的活；跨盘移动时资源管理器就是只复制、再回 `DROPEFFECT_MOVE`）。Mo 删的时候走**回收站**而不是硬删，与它其它删除同一口径，而且比资源管理器自己的移动可撤回。删之前先按 `exists()` 筛一遍——同盘移动时 Shell 已经把源搬走了，不筛就会往回收站操作里塞一批必然失败的任务。
* **结论怎么回 UI 线程**：`AsyncApp` 握着 `Rc<AppCell>`，不是 `Send`，塞不进 `background_spawn` 的 future。用一条 `tokio::sync::oneshot`，在 `cx.spawn` 里 await——与 `EventBus`（`tokio::sync::broadcast`）同一条跨线程唤醒路子。
* **⚠️ 出界判定不能挂在元素上**（这轮真正学到的一条，而且是最开始写完就错的那种）：本来把判断写在 `root` 的 `on_mouse_move` 里，理由是「gpui 按下时 `SetCapture` 过本窗口，出界的 move 照样投递」。投递是对的，**派发**不是：gpui 把 `MouseMove` 交给元素之前要先过 `hitbox.is_hovered`，而 `Frame::hit_test` 是纯几何的——指针一出窗口什么都不命中，于是任何 `on_mouse_move` 回调都不会被调用，尽管 `Window::mouse_position` 一直在被更新（`dispatch_event` 里那句赋值）。gpui 自己那段 promotion 就是在派发**之前**做的，才躲过这一层。现在改成**轮询**：起拖时开一条 16ms 的探测任务，用 `App::with_window(根视图的实体 id)` 读位置（`Window` 不公开自己的 `AnyWindowHandle`，而根视图的实体 id 在 `current_window_by_entity` 里就有）；`drag` 一变空——普通点击、或已经在应用内落下——任务自己退出，不持锁不占帧。
* **windows 0.58 的几处形状**（都是编译期撞出来的）：`IDropSource_Impl` 之于一整套 `*_Impl` 藏在 `implement` feature 后面（`windows` 的 `impl.rs` 是 `#[cfg(feature = "implement")]` include 进来的）；`#[implement]` 展开成 `::windows_core::…`，所以 `windows-core` 得直接进依赖；`ITEMIDLIST` 在 `Win32_UI_Shell_Common` 而不是 `Win32_UI_Shell`；`STGMEDIUM` / `ReleaseStgMedium` 在 `Win32_System_Com_StructuredStorage`；`DragQueryFileW` 这一版没有 `cch` 参数（缓冲长度从切片取）；`HRESULT` 在 `windows::core` 而 `S_OK` / `DRAGDROP_S_*` 在 `Win32::Foundation`；`DRAGDROP_S_DROP` 与 `DRAGDROP_S_CANCEL` 都是**成功段**，判失败只能用 `is_err()`。
* **验到了哪一步**：单测验数据对象（`CF_HDROP` 表与给它的逐条相等，含中文名与目录；掺一条不存在的路径就整批不递）和 `QueryContinueDrag` 的两个确定分支（Esc → `CANCEL`；OLE 的按键位说按着 → `S_OK`；第三条「松手就落子」依赖 `GetAsyncKeyState`，环境相关，不写成断言）。
* **⚠️ 端到端这一轮没验**：「真按住文件拖到资源管理器窗口里松手」headless 做不到——要真鼠标、要另一个进程当落点。我本想用 `SendInput` 演一遍（探针程序都写好了，后来删掉），但那会把光标从用户手底下挪走、也被工具侧的权限判定挡下，不该在没人盯着的会话里做。**所以「出界起拖」和「对面真接住」这两条只有代码审读，没有实测**。人工验一次就够：开着 Mo，按住一个文件拖出窗口、落到资源管理器里松手 → 应复制进去；按住 Shift 再拖 → 移动，源进 Mo 的回收站；中途按 Esc → 什么都没发生。

## 26. 收口 mo-app 的集成测试：一个 helper，把「改环境变量 + 构造」绑成一件事

* **为什么要收**：§21 那轮只改了 `mo-ui` / `mo-platform` 这条路，`mo-app` 的十四个集成测试二进制还是各写各的——`AppState` 一构造就接上 `<缓存>/mo/search.sqlite` 的真索引，每个跑过的临时目录都被记进开发者机器上那份全局索引里（用户按搜索键会搜出一堆早已删掉的 `mo-trash-*`），而偏好文件 `<配置>/config.json` 也是真读真写：同一份代码在不同机器上结论不同。
* **helper 的形状**：`crates/mo-app/tests/common/mod.rs` 的 `isolated(tag, build)`——一次做完「建临时目录 → 钉 `MO_CONFIG_DIR` 与 `MO_CACHE_DIR` → 在这个前提下跑 `build`」，返回 `build` 的值。配置与缓存合成**同一个根**：少一个变量就少一处漏钉。**关键是把两步绑死**，而不是留一个 `use_temp_index()` 让用例自己记得先去拿锁——漏一处就是随机红，而这种随机在单线程 `--test-threads=1` 下永远复现不出来。
* **锁只覆盖构造，不覆盖整个用例**：`AppState` 构造完已经握住了那批 sqlite 句柄，别的用例此后把环境变量改走也影响不到建好的这一个，所以锁没有理由一直抱着。**但「每次调用都现读环境变量」的接口不在这条保护里**——`preview_pdf_page` 落渲染产物时现场读 `MO_CACHE_DIR`，那种用例（`pdf_first_page_is_rendered_and_cached`）必须把整段塞进一次 `isolated` 的闭包里。
* **⚠️ 一次调用清一遍目录**：`store()` 每次都 `remove_dir_all` + 重建（防 pid 复用读到上一轮的脏索引），于是「重开应用后索引还在不在」这类要**同一个库上跑两趟**的用例不能写成「同一个 `tag` 调两次 `isolated`」——第二次会把上一轮写下的索引抹成空库。`index_survives_a_restart`、`metadata_cache_primes_entries_on_reopen` 因此改成两次构造夹着等待全放进**一次**闭包。这条是跨平台雷：POSIX 上 `unlink` 对已打开的文件照样生效，Windows 上句柄占着恰好抹不动，所以只在 mac/Linux 上红。
* **顺带改了一处异步形状**：`metadata_cache_primes_entries_on_reopen` 从 `#[tokio::test]` 改成 `#[test]` + 自己起 `tokio::runtime::Runtime`。原因是 `isolated` 收的是**同步**闭包（它抱着那把 `Mutex`），而「等元数据写回缓存」必须是真异步——只能在闭包里对一个新起的 runtime 调 `block_on`；留在 `#[tokio::test]` 里做不了，在正在跑的 runtime 内部再 `block_on` 会直接 panic（*Cannot block the current thread from within a runtime*）。自己握 runtime 也顺便让「构造在锁内、异步在 `block_on` 内」这个顺序显式可见。
* **`mo-operations` 不在这次的范围里**（不是漏了）：`conflict.rs` / `transfer.rs` 只碰自己的临时树，`grep` 下来整个 crate 的测试既不构造 `AppState` 也不读这两个变量，写进真实索引这条路压根不存在。
* **验证方式**：还是 §21 那一条——看 mtime，不看日志。`cargo test -p mo-app --all-features`（50 单测 + 十四个集成二进制共 76 个用例）前后，`%LOCALAPPDATA%\mo\search.sqlite` 的修改时间分毫未动（`2026-09-27 10:14:54`），而 TEMP 里多出 418 个 `mo-app-store-*` 目录——它们的存在就是「每个用例都写到了自己的库里」的实证。堆积的代价已解（TTL 清扫，见 devlog/engine-testing.md 与 `mo_fs::sweep_stale_temp_dirs`）。

## 27. 顺手修掉一个跑了五轮的随机红，以及「全绿」这两个字是怎么读出来的

* **现象**：§26 那轮收口之后跑全量测试，`mo-ui/tests/layout.rs` 的 `trash_space_previews_and_double_click_opens` 红了：双击回收站里的目录行，「退出面板」那条断言过了，「当前窗口浏览该目录」（`mo-file-row-0` 存在）没过。单独跑三条全过，整条二进制跑四次红一次。
* **根因**：目录读取是**真 IO**（`spawn_blocking` / tokio worker 线程读完再回头唤醒 GPUI 任务），而 headless 的 `run_until_parked` 只是 `while tick() {}`——它排自己的任务队列，**不等那条外部线程**。所以「导航已生效、列表还没回来」这个中间态会被一次 `render_frame` 撞上。§24 坑三撞到的是同一件事，当时只在 `os_drop.rs` 就地补了轮询，没抽出来，`layout.rs` 里这条双击的老写法就一直留着。
* **修法**：把那段轮询提成 `wait_for_panel_rows(vcx, window, cx, rows)`（`navigate_and_wait` 改成调它），双击测试等 `rows == 1`。判据必须是**行数精确相等**，不是「≥1 行」——后者会被启动时那次「按需打开 Home」的内容蒙过去，那正是 §24 坑三的原始形态。修完连跑八次全过。
* **⚠️ 顺带纠正一处我自己报过的假绿**：之前的全量验证写的是 `cargo test --workspace ... | grep ... | tail -60`，**退出码是管道最后那个命令的**，测试真失败也报 0。§26 那句「全量绿」里混着这么读出来的一次。现在一律 `out=$(cargo test ...); code=$?` 再判断，或者干脆不加管道。凡是靠日志尾巴得出的结论，都要先问一句这个 0 是谁的 0。



## 待办（还没做，别当成已完成）

* 全篇（§1~§13）都是**落地之后补记**的，当时第一手的调试感（比如 `$I` 扫了几千条才反查通、`explorer` 退出码是怎么误报的）已丢了一些；§14 之后是当轮写的。
* Windows 的 `windows_pdf` 别名是权宜：若哪天要把 Shell 那套也升到 0.62，一并把两个版本收成一个，别再叠第三份。
* **§22 macOS 的「US 表」缺口已补**（2026-09-30，`dcbe702`）：macOS 也走 `TIS + UCKeyTranslate`
  查当前布局折符号键，非 US 布局不再串键；德语真值由 `unshifted_key_consults_current_layout`
  在德语机器上断言（US 自动跳过）。**Linux 侧 §15 也补了（2026-10-08）：`linux::unshifted_key` 走
  libxkbcommon 读 XKB 布局，非 US 布局不再串键；单测 `unshifted_key_follows_german_layout` 在真机
  跑（`XKB_DEFAULT_LAYOUT=de` 下 `?`→`ß`）。gpui 的 Linux 后端怎么报 Shift+符号**这件事本身**仍只
  在真机验过编译、未在真机点过——属「整体真机行为」验收项，不在本补丁范围。
* **拖放「出去」的 Windows 实测仍欠着**（§24 进来、§25 出去，macOS 侧 §29 已补齐并统一成复制语义、用户真机已验）：按住文件真拖一次到资源管理器上松手，看它落不落子、结论是不是复制——headless 做不到，得人来。收账盘点见 §35。
* ~~**应用内拖拽零 headless 覆盖**（§35）~~ **已补**（2026-10-01，§36）：`crates/mo-ui/tests/internal_drag.rs` 五条，含 Alt=移动。
* ~~**网格 / 画廊没接应用内拖拽**（§35）~~ **已接**（2026-10-01，§37）：`grid::cell` 挂上 down→`begin_drag` / up→`drop_on_entry`，`tests/grid_drag.rs` 五条。
* ~~**网格 / 画廊的目录单元没接「OS 拖入」**（§37 发现）~~ **已接**（2026-10-01，§38）：目录格子挂上行级 `on_drop::<ExternalPaths>`，`os_drop.rs` 两条 e2e（含冒泡对照）。~~列视图（`columns.rs`）两种拖拽仍都没接~~ **也已接**（2026-10-01，§39）：至此四种视图 × 两种拖拽全部有接线和 headless 覆盖，拖放线只剩「Windows 拖出人工实测」这一条人做的活。
* **编辑类交互的列视图/网格缺口已接**（2026-10-03，§40）：列视图单击此前不动 app 选择（编辑命令打在隐形旧选区），行内改名编辑器此前只在列表视图渲染。**新欠**（§40 缺口 3，P1）：同目录内容变更后列视图不刷新——`ensure_columns` 只认第 0 列路径变了才重建。**已还**（2026-10-03，§41）：`tab_loop` 消费 `DirectoryChanged` 竖牌，`ensure_columns` 下一拍**原地重读**各列（列栈与下钻保住、cursor 钳位），`tests/column_refresh.rs` 三条钉住。
* **拖拽中的视觉反馈已补齐**（2026-10-06，§42）：跟随光标的 ghost（「首项名 · N 项 · 复制/移动」，4px 阈值内不闪、单击无扰动）+ 落点高亮（三视图目录行 / 网格目录格 / 列视图目录行 + 跨窗格空白整块淡染），「亮着的落点抬起必定传输」由 `tests/drag_feedback.rs` 六条兑现（ghost 动词的判定表在 §43 换了引擎，文案形状不变）。**剩真机实感**（headless 验不了）：ghost 配色观感、快拖有无闪烁、分栏跨窗格淡染观感、拖出交接 OS 瞬间 ghost 是否即消——清单见 §42 末尾。
* ~~**应用内拖放恒复制，与资源管理器的直觉不符**~~ **已改判定表**（2026-10-06，§43）：同卷宗直拖=移动、跨卷宗=复制、Ctrl 强制复制、Shift 强制移动、Alt 保留为移动别名；ghost 动词与抬起结算出自同一张 `drag_resolves_to_move`。**OS 拖入（资源管理器 → Mo）仍恒复制**——gpui 在 FileDrop 提交时把修饰键清零，拿不到键就守最安全的动作，这是与资源管理器**有意分叉**的一处，见 §43。
* ~~**新建文件后图片行的图标闪一下（缩略图被打回默认图标）**~~ **已修**（2026-10-06，§44）：整目录重读会丢掉旧快照里的 `Loaded` 位图，异步回填那一两帧画的是 fallback；现在换上新快照前按 `FileId` 搬运（只搬 `Loaded`），`crates/mo-app/tests/thumbnail_carry.rs` 三条钉住。**剩真机确认**：新建 / 删除 / 传输落盘之后再扫一眼图片行，是否还看得到那一帧默认图标；**切目录**首屏仍会「先默认后缩略图」——那是真·首次解码（没东西可搬），属预期，别当回归。
* ~~**拖动多个文件做移动时界面卡住几秒钟**~~ **已修**（2026-10-06，§45）：账在搜索索引，不在拖拽也不在传输——`FileIndex::remove_under` 的 `LIKE '前缀/%'` 是 58 万行全表扫（实测一次 147ms，未命中一样），每个被移走的条目一次，期间索引锁被攥着；主线程那边 `sync_panel` 每拍同步问一次「已索引 N」（`COUNT(*)`，实测 64ms），排在同一把锁后面，两边互相放大。`remove_under` 改 path 字节范围（走 `idx_path`，未命中 33µs），`index_count` 改「第一次就地数、之后读缓存 + 1s TTL 派发后台重算」。**新欠**（同一族，本轮没动→**已还**，2026-10-06，§46）：命令面板的 `global_search` 仍是主线程一次 `LIKE '%q%'` 全表扫（同一份库约 150ms/次按键），那是搜索线的账，不在拖动路径上。
* ~~**全局搜索每敲一个字卡一下**~~ **已搬出主线程**（2026-10-06，§46）：`Modal::GlobalSearch` 的按键处理原来同步跑 `AppState::global_search`（`name_lower LIKE '%词%' OR path LIKE '%词%'`，中缀通配用不上任何索引，58 万行实测约 150ms/次）；现在查询整趟派到 blocking 池，回来只对「趟号」——过期那趟落地即丢，关掉模态重新打开看不见上一次查询的行。`crates/mo-ui/tests/global_search_bg.rs` 三条钉住，两轮变异反验。**仍欠**：`FileIndex::search` 本身还是全表扫，后台化只是让它离开主线程；要真快得上 FTS/trigram，那是搜索线的下一笔账。
* ~~**左下角任务卡片左右间距不一样**~~ **已修**（2026-10-07，§47）：卡片宽写在常量里是 176，壳的左边距是 10，侧栏宽 188——右边只剩 2px，肉眼就是「贴边」。`CARD_W` 改 168（188−10−10，与卡片 `left(10)`、状态栏 `px(10)` 同一条节奏），`tests/layout.rs::badge_is_centered_in_the_sidebar_column` 拿真实 bounds 把 `SIDEBAR_WIDTH`／`left`／`CARD_W` 三个常量绑在一起，谁改宽了卡片就红。
* ~~**进度条改成环形、百分比放进环里**~~ **已改→已撤**（2026-10-07，§48 + §48 订正）：环（kit 的 `ProgressCircle`，直径 36 由「9px 的『100%』实测宽 22px」反推）做完在真机上被用户判「太难看了」，正向 revert 摘掉，**连它带出的 `theme.rs` Theme 补装一起撤**。现形状：卡片左「N 个任务」右一个聚合百分比、行下行「百分比 + 速度 · 剩余时间」（结束态只给中文状态），`CARD_H` 回 30、`ROW_H` 回 56；`tests/layout.rs` 两条新判据（选择器带渲染值本身 / 只有还活着的行显示百分比）三个变异反验。环形那节的 API 实测结论留着复用，别重量一遍。


## 28. macOS 文件剪贴板「出去」补齐（2026-09-29，bf4519e）

* §20 只做了 Windows 方向：`supports_file_clipboard()` 在 mac 上恒假，Mo 里
  Ctrl+C 之后去别的程序粘贴什么都没有。补上 AppKit 那条：
  `macos::write_file_clipboard` = `NSPasteboard.generalPasteboard` 上
  `clearContents`（`writeObjects:` 的前置，返回新 changeCount，0 = 失败）+
  `NSMutableArray<NSURL>` + `writeObjects:`。NSURL 自己遵守 `NSPasteboardWriting`，
  写出 `public.file-url` / `NSFilenamesPboardType`——正是访达读写的那几个类型；
  百分号编码（中文、空格）由 NSURL 处理，不自己拼 `file://` 字符串。
* `cut` 参数收下但按**复制**处理：Finder 的「剪切」记在私有 pasteboard 标记里
  （读侧 §19 同款判据），公开 API 写不出剪切语义——宁可让对方粘出一份复制，
  与「猜错方向会把用户的文件搬走」同一立场。
* 走 `on_main_thread`（复制在 `spawn_blocking` 里发起）；`supports_file_clipboard()`
  翻成 mac/win 都真，mo-ui 的门控自然接上，失败只记日志不报错（§20 那条纪律不变）。
* **真机验证记两条**（`examples/clipboard_probe.rs`，写三条路径含中文+空格，
  JXA 跨进程读回三条 URL 全对）：
  * `writeObjects` 是**惰性**的：数据提供者是本进程里的 NSURL 对象，别的应用
    来读时剪贴板服务进程回头找写方要数据——写完立刻退出的进程什么都留不下
    （探针第一版就是这么「空」的）。探针要保活几秒；Mo 本体长驻无此问题，且
    剪贴板服务进程会在写方终止时接管数据——实测探针退出 4 秒后读回仍在，
    退出 Mo 后粘贴不丢。
  * 读回用 JXA：`readObjectsForClassesOptions`（多段选择器连写，不是
    `ForClassesForOptions`）+ **空字典**作 options——传 `null` 会被桥成 NSNull，
    炸 `unrecognized selector count`。
* **真机验证通过**（2026-09-29 用户亲测）：Mo 里复制几个文件 → 访达 ⌘V，粘得出。

## 29. macOS 文件拖出补齐：`beginDraggingSessionWithItems`（2026-09-29，04d6e51）

* §25 只接了 Windows 的 OLE 源；macOS 上 `supports_file_drag()` 恒假，拖出窗口后
  应用内拖拽悄悄死掉。补上 AppKit 那半：`macos::begin_drag` =
  `NSView.beginDraggingSessionWithItems:`，`NSDraggingItem` 按 `NSURL` 走
  `NSPasteboardWriting`（与 §28 剪贴板同一类型面）。**mo-ui 零改动**——出界轮询
  （§25 那条 16ms 探测）与 oneshot 回传本来就是跨平台的，`supports_file_drag()`
  一翻自然接上。
* **NSEvent 从哪来**：手写拖拽没像 gpui 那样保留 mouseDown 事件；但按住拖动期间
  窗口**持续**收到 `mouseDragged`（事件循环跟着按键走，与指针在不在窗口里无关），
  `[NSApp currentEvent]` 就是最新那条——起拖直接用它，拖图框也锚在它的
  `locationInWindow` 上（AppKit 记「拖图离事件位置的距离」当光标偏移，gpui-pre
  注释里点明了这一点）。类型不是 `LeftMouseDown(1)` / `LeftMouseDragged(6)` 就
  拒绝起拖，不硬来。
* **拖图按扩展名取**（`iconForFileType:`，gpui-pre 同款）：`iconForFile:` 同步打
  LaunchServices 且会抖（见 devlog/macos-platform.md 的图标服务坑），一批几十条
  能把起拖卡住。目录给 `public.folder`，无扩展名给 `public.data`。
* **NSDraggingSource 手工 `ClassDecl` 注册**（不用 `declare_class!`——要挂协议）：
  复制与移动都声明、目标定（与 Windows 同一口径）；新式会话问
  `draggingSession:sourceOperationMaskForDraggingContext:`，旧式问
  `draggingSourceOperationMaskForLocal:`，两个都答。结论在
  `draggingSession:endedAtPoint:operation:` 里按回的操作给：
  Move→`Some(true)`（上层走回收站删源，`finish_file_drag` 现成）、
  Copy→`Some(false)`、零→`None`。回调槽 `Box<Option<…>>` 裸指针存 ivar，只消费一次。
* **objc 0.2 的两个坑**：
  * `Encode` 没有结构体编码——`NSPoint`/`NSRect` 用 `#[repr(C)]` 镜像 +
    `Encoding::from_str("{NSPoint=dd}")` 手工补；方法签名里的 `NSPoint` 参数
    （`endedAtPoint:`）就靠它。
  * **`BOOL` 按架构变型**：x86_64 是 `c_schar`，**aarch64 上是 `bool`**（0.2.7 的
    cfg）。断言别写 `yes != 0`，用 `assert_eq!(yes, YES)` 两种架构通吃。
* **测试边界**：起真拖拽会抓住用户的指针跟着走——不进测试（与剪贴板同一条红线）。
  能测的是：空名单拒绝起拖（回调不被调）；`MoDragSource` 注册冒烟（三个方法
  `instancesRespondToSelector:` 全响应、`OnceLock` 幂等）——方法漏挂的错 AppKit
  要到用户拖出去那一刻才炸，注册冒烟在这一步就拦住。
* **真机验证清单**（人工）：①按住文件拖出 Mo 窗口 → 落到访达 → 应复制进去、
  源文件留着；②拖出后按 Esc → 什么都没发生；③拖到访达**同一卷宗**里看对面给
  的结论（若对面判移动 → 源进 Mo 回收站）。Windows 侧 §25 的同款实测仍欠着。

### §29 订正：起拖闪退（2026-09-29，0ad4b59）

* 用户真机一拖即崩：`- [NSDraggingItem setImageContents:]: unrecognized selector`。
  根因：起拖把便捷入口拆成了两条消息——`setDraggingFrame:`（只给框）+
  `setImageContents:`（给图）。**后者不存在**，正确 API 是一条
  `setDraggingFrame:contents:`，框与图同发。
* 教训（§29 自己写的「方法漏挂的错要到拖出去那一刻才炸」正好应验在自己头上）：
  objc 消息**编译期不校验**，冒烟测试不能只核自己注册的类，**我们对系统类发的
  每个选择器**也要 `instancesRespondToSelector:` 核一遍——测试补了
  NSDraggingItem（setDraggingFrame:contents: / initWithPasteboardWriter:）与
  NSWorkspace（iconForFileType:）。

### §29 订正二：拖拽语义统一成复制（2026-09-29，3a9b811）

* 用户实测发现交互**不对等**：Mo 拖出到访达（同卷）被目标判成移动（源进
  回收站），反向从访达拖进 Mo 却恒是复制。根源在接收侧：gpui 的外部 drop
  通道拿不到修饰键（`FileDropEvent::Submit` 翻成 MouseUp 时清成
  `Modifiers::default()`）、也回不了操作码（不是原生 `NSDraggingDestination`）
  ——拖入想做移动既没有意图信号也没有协议位；而拖出「复制与移动都声明、
  目标定」就让目标（访达同卷）选了移动。
* 决策：**拖拽语义统一成复制**。拖出两侧（macOS `DRAG_MASK`、Windows
  `do_drag` 的 `DROPEFFECT`）都只声明 Copy，目标只能回复制；两端方向一致、
  最可预期，移动走剪切粘贴。
* 留口子：`drag_ended` 的 MOVE 分支、`finish_file_drag` 的移动收尾（源走
  回收站）都留着——将来接修饰键透传（Windows `IDropTarget::Drop` 的
  keyState / macOS 平台层自接 `NSDraggingDestination`）后一起开移动语义。

### §30：诊断日志在 Windows 上不可见（2026-09-29，1628603）

* 现象：`$env:RUST_LOG="mo_ui=debug"; cargo run` 一条日志不出。根因：`main.rs`
  顶部 `windows_subsystem = "windows"`（§「修法」那条）把 exe 链成 GUI 子系统，
  被终端拉起时进程**没有** stdio——`GetStdHandle` 是空句柄，tracing 写 stderr
  石沉大海。macOS 是控制台子系统，所以那边一直好好的。
* 修法：`mo_platform::attach_parent_console()`——`AttachConsole(ATTACH_PARENT_PROCESS)`
  挂回父进程（cargo / PowerShell）的控制台；⚠️ 它**不设置**标准句柄（与
  `AllocConsole` 不同），要自己开 `CONOUT$` 再 `SetStdHandle` 补 STDOUT/STDERR。
  调用点在 `init_tracing` 最前面、std 首次用 stderr 之前（std 会缓存首次句柄，
  晚了接不上）。只在设了 `RUST_LOG`（= 要诊断）时调：双击启动父进程是资源
  管理器、没有控制台，AttachConsole 失败返回 false，GUI 行为分毫不变。
* windows 0.58 的 API 位置：`AttachConsole` / `SetStdHandle` / `STD_HANDLE`
  （结构体型枚举，含 `STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE` 关联常量）都在
  `Win32::System::Console`，feature `Win32_System_Console`。
* 验证：本机装了 `x86_64-pc-windows-msvc` target 后
  `cargo check -p mo-platform --target x86_64-pc-windows-msvc` 能在 mac 上
  **编译期**验 Windows FFI 代码的签名——以后改 windows.rs 都可以先这么过一遍。

### §31：地址栏无盘符根归一（2026-09-29，3a2df49，方案 A）

* 用户复测：输 `/` 跳到 F:\ 根，面包屑「此电脑 › Mac」。两层根因：①`/` 解析成
  `\`（RootDir 无前缀），std 语义落**进程 cwd 所在盘**——从哪启动就哪盘，与正在
  看哪个盘无关；②`toolbar::segments()` 的 RootDir 分支硬编码「Mac」（unix 根标
  签），Windows 走到就泄漏。
* 修法（用户选 A，对齐资源管理器）：`resolve_address_input` 增加 `current`（本机
  浏览态当前目录，`Panel::path` clone 进去——**match scrutinee 的临时借用会横跨
  所有分支**，与分支里的 `&mut self` 打架，必须先 clone），Windows 上经
  `absolutize_drive_root` 归一：正在看 `D:\foo` → `\` 到 `D:\`；停在「此电脑」
  退回进程 cwd 所在盘；带盘符 / 相对路径原样。归一拼路径靠 `PathBuf::join` 的
  文档行为（「有根无前缀的路径保留 base 的前缀、替换其余」）。
* `segments()` RootDir 分支 Windows 上显示分隔符本身，不再出「Mac」（正常走不到，
  历史 / 书签兜底）。
* 验证边界：mo-ui 交叉 `cargo check --target x86_64-pc-windows-msvc` 卡在
  aws-lc-sys 的 C 构建脚本（要 Windows C 工具链），**cfg(windows) 代码本机验不了**
  ——靠用户侧编译 + `driveless_root_uses_the_browsed_drive_on_windows`（cwd 落点
  用同一 API 算期望值，不写死盘符）兜。

### §32：显示名统一——全应用显示目录本名（2026-09-29，80a677b / 9b5eebc）

* 那张「已知文件夹 → 中文标签」表（`mo_app::known_folder_labels`）本是修 Windows
  上「侧栏写桌面、地址栏写 `Desktop`」引出来的，结果跨到了 macOS：unix 根被标签
  成「Mac」（§31 修的一处泄漏）。用户 2026-09-29 定调：**文件夹是什么名字就显示
  什么**。
* 落地顺序：地址栏面包屑（`toolbar::segments`，80a677b）→ 侧栏书签、列视图列头、
  标签页标题（`sidebar::bookmark_rows` / `columns` 列头 / `Panel::title`，9b5eebc）。
  三处都从 `path_label::folder_label` 换成 `last_segment` 后，`folder_label` /
  `known_folder_label` / `same_dir` 无人使用，一并删掉（含钉中文标签的三条测试），
  换成 `known_folders_show_their_real_name`（拿真机的表问：显示目录本名 ≠ 中文标签）。
* **保留**：侧栏「快捷访问」那一区（`AppState::quick_locations`）仍是中文标签——
  它是**入口**，不是「当前位置」的标识，跟访达侧栏同理。要改再说。
* 判据：**同一个目录在窗口里任何一处只能有一个名字**。再加新的显示位置，默认
  取 `last_segment`，别再开一份「这边翻译、那边不翻译」的表。

### §33：解不开的压缩格式要说人话（2026-09-29，74164e0）

* **现象**：`.7z` / `.rar` / `.tar.bz2` / `.tar.xz` 都掉进 `extract_tar`，报一句
  底层库的「不是 tar」——用户不知道发生了什么，也不知道该怎么办。
* **根因（又是「同一件事两处作答」）**：`extract_archive` 按 `.zip` 判完之后
  **一律**交给 tar 分支；而右键菜单的 `looks_like_archive` 自己列了八个后缀，
  其中 `.tar.bz2` / `.tar.xz` 其实解不开（tar 分支只接 gzip，flate2 不管 bz2/xz）
  ——菜单给了「解压」，点下去报错。
* **修法**：解压的判据收口成 `mo_operations::extract_format`（zip / tar /
  tar.gz / tgz，其余 `None`），**与打包用的 `ArchiveFormat::from_path` 分开**——
  打包认不出按 zip 处理（给个能用的结果），解压认不出必须直说。菜单改问
  `is_extractable`（解不开就不给这一项），`extract_archive` 认不出就当场报错
  并列出支持的格式；UI 的解压结果带上失败原因。
* 要不要真接外部工具（`7z` / `bsdtar`）另说——那是新依赖 + 进程调用，不在这一刀里。

### §34：压缩接外部工具解 7z / rar / tar.bz2 / tar.xz（2026-09-30）

* **现象**：§33 把 `.7z` / `.rar` / `.tar.bz2` / `.tar.xz` 一律报「暂不支持」，右键
  菜单也不给「解压」。用户选「功能补全」把这个缺口补上——Rust 原生库覆盖不到的
  格式交给外部工具（7-Zip / 系统 tar）。仍**不引新依赖**：`std::process::Command` 足矣。
* **判据收口（三条解压判据，不合并）**：`extract_format`（内置 zip/tar/tar.gz/tgz）
  与新增的 `external_extract_format`（7z/rar→`ExternalFormat::SevenZip`；tar.bz2/
  tar.xz→`ExternalFormat::Bsdtar`）是独立两条，都汇进 `is_extractable`。打包那侧
  的 `ArchiveFormat::from_path` 仍是第三条、不掺和（§33 的纪律不变）。
* **`extract_external` 的写法**：`Command` 的 **args 数组**（归档路径与目标目录都是
  独立 `OsString`，绝不拼进 shell）——路径含空格、中文安全，且无命令注入。7z 用
  `x <归档> -o<目标> -y`（`-o` 与路径紧挨无空格）；tar 用 `-xf <归档> -C <目标>`。
  Windows 加 `CREATE_NO_WINDOW` 防 GUI 应用弹黑框（§11 同一条）。整条落在
  `AppState::extract_archive` 的 `spawn_blocking` 里，不碰 GPUI 执行器。
* **工具不在怎么办**：候选二进制逐个试（`7z`→`7za`；`tar`），`spawn` 报 `NotFound` 就
  试下一个；全找不到才报错，且**指名道姓**——「找不到能解压 tar.xz 的工具
  系统 \`tar\`（libarchive 版），请先安装 …」（不复活底层退出码那句天书）。退出非 0
  取 stderr 前 3 行夹进提示，不把整坨输出塞进界面。
* **`is_extractable` 不探测工具存在**：基于扩展名判，菜单该不该给「解压」不随用户
  有没有装 7z 而闪烁；真解不开时由 `extract_archive` 当场说人话（上面那条）。
* **测试**：`mo-operations` 34 单测全绿——`external_formats_are_now_recognized`
  （外部家族映射正确、现可被识别）+ `truly_unknown_format_still_says_which_one`
  （`.zzz` 这种完全不认识的仍报错列格式，守 §33 反向验证精神）；`mo-ui` 的
  `context_menu` 15 测试绿（`.7z`/`.tar.bz2` 等现给「解压」）。临时真实解压测试
  （造 .tar.xz 真调 `extract_archive`）跑通后已删，不污染套件。
* **仍欠（已知，非漏做）**：7z/rar 的真机验证要用户机器装了 7z（本机未装，只验了
  tar 路径命令形态 + Rust 外部分支对 .tar.xz 真解压）；Windows 的 `CREATE_NO_WINDOW`
  是编译期 cfg，端到端没在 Windows 跑过黑框验证。

### §35：拖放收账——两端实况与真正还欠着的（2026-10-02）

* **为什么有这一节**：移植清单上的「拖放」项（任务 #17）要关账。盘点结论：
  **Windows 侧功能完整、测试在场**；记忆里的「macOS 欠账（拖出 / 文件剪贴板写）」
  **并不存在**——§28（剪贴板写）、§29（拖出）2026-09-29 都已补齐且真机验证过
  （§29 的两条订正恰恰就是用户实测的产物），不必另立待办。
* **Windows 侧对账**：
  * 拖入：gpui 的 `IDropTarget` 翻译 `FileDropEvent`（§24），UI 四处落点注册；
    `crates/mo-ui/tests/os_drop.rs` 三条 headless e2e 投**真的** Entered+Submit
    事件、真盘验落点，跨平台跑（无 cfg 门控）。
  * 拖出：OLE 全套（§25，STA 线程 + Shell 数据对象 + `IDropSource`），
    `mo-platform/src/windows/drag.rs` 三条单测钉数据对象与两个确定分支
    （掺幽灵路径整批拒、Esc→CANCEL、按键位按着→S_OK）+ `dropfiles_bytes`
    字节级钉 `CF_HDROP` 布局（windows.rs）。
  * 文件剪贴板读写：`CF_HDROP` 写 + `Preferred DropEffect` 读（§19/§20），
    引擎侧 `crates/mo-app/tests/clipboard.rs` 五条（cut 消费、系统剪贴板接管）。
  * 收账当日全量：`cargo test --workspace --all-features` 60 个套件 0 失败。
* **真正还欠着的（都是已知项，不是这次查出来的新窟窿）**：
  1. Windows 拖出**端到端人工实测**仍未做（待办里那条；§25 的清单：拖进
     资源管理器松手应复制、Esc 无副作用）。macOS 侧同款清单 09-29 已走完。
  2. **应用内拖拽零 headless 覆盖**：`begin_drag` / `drop_on_entry` /
     `drop_on_pane` / `run_transfer`（含 Alt=移动）没有一个直接测试；底下
     传输引擎在 mo-app 集成测试里有（remote_local.rs 分栏那几条），缺的是
     UI 派发层。
  3. **网格 / 画廊视图没接应用内拖拽**：§14 的手拼鼠标事件只挂在列表视图
     行上（`file_list.rs`），grid 无 `begin_drag`。
  4. 移动语义两端统一成复制（§34 订正二=§29 订正二的决策，留了 MOVE 分支
     口子）——这是设计选择，记此备查，不是欠账。

### §36：应用内拖拽的 headless 覆盖补齐（2026-10-01）

* **补的是 §35 欠账 2**：`begin_drag` / `drop_on_entry` / `run_transfer` 这条
  UI 派发链此前零直接测试。新增 `crates/mo-ui/tests/internal_drag.rs` 五条，
  全真链路（真导航、真行、真落盘，墙钟轮询——传输在进程级 tokio runtime 上，
  不受 GPUI 测试调度器驱动，理由同 `os_drop.rs`）：
  * 拖文件行到目录行 = 复制进去、源留着；
  * **Alt 抬起 = 移动**（把 `ev.modifiers.alt` 从鼠标事件一路钉到落盘）；
  * 同行按下抬起 = 点击不是拖拽，且拖拽状态没被消费卡死（随后真拖照常）；
  * 同窗格拖到**文件行**上谁都不接（`drop_on_entry` 拒非目录、不回抛窗格兜底）；
  * 多选拖拽 = 整个选中集一起复制（`begin_drag` 的 `selection.count() > 1` 分支）。
* **行序不靠猜**：fixture 里目录排在第几行受排序规则影响，新增
  `mo_ui::panel_row_paths_for_tests`（已渲染快照逐行给 `(路径, 是否目录)`），
  测试先查行号再摆鼠标；多选那条还断言两条文件行**相邻**，排序变了会响。
* **坑位（headless 测试姿势）**：`VisualTestContext::from_window` 之后，
  **派发鼠标/渲染用 vcx，读 `RootView` 状态必须走外层 `TestAppContext`**
  （`window.update(tcx, ...)`）——从 `vcx.update` 闭包里递进来的那个 `App`
  查不到这个窗口，直接「window not found」。
* **变异反验两处**（红在预测断言、还原字节一致）：`file_list.rs` 的
  `alt = ev.modifiers.alt` 写死 `false` → Alt 移动条红在「源文件还在」；
  `begin_drag` 的 `multiple` 写死 `false` → 多选条红在「a2 没进箱子」。
  原地点击的守卫分支查过等价（`self_drop` 已挡住传输），不做该变异。

### §37：网格 / 画廊接上应用内拖拽（2026-10-01）

* **补的是 §35 欠账 3**：§14 手拼的拖拽机器此前只挂在列表行上。`grid::cell`
  补两笔接线（与 `file_list.rs` 同款）：`on_mouse_down(Left)` → `begin_drag`、
  `on_mouse_up(Left)` → `drop_on_entry(pane, tab, path, is_dir, alt)`——
  网格/画廊共用同一个 `cell`，一处接线两种视图同时生效（画廊那条测试就是钉这个的）。
* **与 `on_click` 共存的契约**（列表行没有这条，网格特有）：网格的选中走
  `on_click`，行内改名收尾的 `end_inline_rename_on_click` 网格本来就没有——
  不动。原地按下抬起：`drop_on_entry` 同地判据返回、`on_click` 照常落地选中；
  跨单元拖：up 不在源单元上，click 根本不触发。`grid_drag.rs` 的
  `grid_press_and_release_is_a_click_that_still_selects` 两头都钉住
  （选区 = 1、不传输、随后真拖照常）。
* **测试**：`crates/mo-ui/tests/grid_drag.rs` 五条——目录单元接拖=复制、
  Alt=移动、原地点击、文件单元拒接、画廊同链路。视图切换**真点工具栏按钮**
  （`vcx.update(|w, cx| w.click("view-mode-grid", cx))`，按 ElementId 定位），
  不碰内部状态；单元定位 `mo-grid-cell-{条目位}`，条目位与窗口快照同序，
  目录在第几格仍由 `panel_row_paths_for_tests` 查出来。
* **变异反验两处**（还原字节一致）：网格里 `alt` 写死 `false` → 只有 Alt 条红；
  掐掉 down 接线的 `begin_drag` → 四条传输类全红、「文件单元拒接」照常绿
  （它钉的是「不动盘」，没接线时本来不动——正说明那条要配着前四条读）。
* **查记时新发现的缺口（记进待办，非本轮范围）**：网格/画廊的目录**单元**没挂
  OS 拖入的行级监听，从资源管理器拖到目录格子上会落到窗格兜底、进的是**当前**
  目录而非那个目录（列表视图有 `on_drop::<ExternalPaths>`，网格没有）；
  列视图（`columns.rs`）应用内拖拽与 OS 拖入都没接。（网格那半已在 §38 补上。）

### §38：网格目录单元补上 OS 拖入的行级监听（2026-10-01）

* §37 记的缺口还上：`grid::cell` 的目录单元挂 `drag_over` 高亮 +
  `on_drop::<ExternalPaths>` → `drop_os_paths_on_entry`，与列表行（§24）逐字同款，
  含那条纪律：**非目录单元不注册监听**而不是 `can_drop` 拒绝——gpui 在判定
  可行性前就取走 `active_drag`，拒绝等于吞事件、窗格兜底再也收不到。
* **测试**（`os_drop.rs` 追加两条，投真的 Entered+Submit 平台事件）：
  * 拖到目录单元 → 进**那个目录**、且**不**进当前目录（fixture 的 `here/` 里只有
    `inbox/`，两个位置的有无一对就分得开行级与兜底）；
  * 拖到文件单元 → 冒到窗格兜底、进当前目录——这条是「不注册」判据的正面证据
    （若改成 `can_drop` 拒绝，事件被吞，兜底也等不到）。
* **变异反验**：目录单元的 `if is_dir` 掐成 `if false && is_dir`（等于退回没接线）
  → 只有「进那个目录」条红，冒泡对照条照常绿——正是预期的分裂。
* **剩的拖拽缺口只有列视图**（`columns.rs` 两种都没接），在待办里。（已在 §39 补上。）

### §39：列视图接上两种拖拽——拖放线到此收齐（2026-10-01）

* **列视图的特殊性在起点不在结算**：它的条目来自 `AppState::list_dir`
  （`LightEntry`，不走主目录模型），没有 FileId、没有选区概念，所以新增
  `RootView::begin_drag_single`（单源版起点）；`drop_on_entry` / `drop_on_pane` /
  `run_transfer` 与列表、网格**共用同一个 `DragState` 机器**，一行没改。
* `columns.rs` 行级接线与另两个视图同款：down→记源、up→结算（Alt=移动）、
  目录行 `drag_over` 高亮 + `on_drop::<ExternalPaths>`、非目录行不注册（冒到窗格
  兜底）。行补了 `debug_selector` `mo-col-row-{pane}-{tab}-{列}-{行}`（原来只有
  ElementId，坐标派发够不着）。
* **测试**（`tests/column_drag.rs` 六条）：应用内四件套（目录行复制、Alt 移动、
  原地点击不误拖且状态不卡、文件行拒接）+ OS 拖入两条（目录行进**那个目录**、
  文件行冒到窗格兜底进当前目录）。列视图数据面另加 `column_rows_for_tests`
  （问 `Panel::columns` 第 0 列，不能用主目录窗口快照）。
* **变异反验两轮**（还原字节一致）：第一轮 alt 写死 + 掐目录行 OS 监听 →
  恰红 Alt 条与 OS 目录条、其余 4 条绿（两条链独立性顺带钉住）；第二轮掐
  `begin_drag_single` → 应用内三条全红、OS 两条与拒接条绿。
* **收账**：§35 欠账 2/3 与 §37/§38 记下的缺口至此全清——四种视图 × 两种拖拽
  都有接线与 headless 覆盖。拖放线上唯一剩下的还是那条**人做的**：Windows 拖出
  端到端实测（§25 清单）。

### §40：编辑类交互接线——列视图单击整替选择、网格/画廊/列视图可见改名（2026-10-03）

* **盘点（四视图 × 编辑命令）**：网格 / 画廊的单元 `on_click` 走 SelSync，
  panel.selection 与 app 选择**双同步**，F2 / 复制粘贴 / 删除 / 新建的目标都对。
  两个缺口：
  * **缺口 1（数据安全，P0）**：列视图单击只 `set_column_cursor`，app 选择停在
    列表视图遗留的旧值——在列视图按 F2 / 删除 / ⌘C，动的是**看不见的旧选区**。
  * **缺口 2（P0）**：行内改名的编辑器只在 `file_item::view`（列表）渲染，而
    `sync_inline_rename` 不分视图地建 `InputState` 并聚焦——网格 / 画廊 / 列视图
    按 F2，焦点进一个**没挂载的隐形输入框**，打字看不见、Enter/Esc 由全局拦截器
    处理，改名能落盘但用户全程盲操作。
* **缺口 1 的修法沉在 mo-app**：`AppState::select_path_exclusive`——清空
  （选择集 + 锚点 + 键盘光标）与单选在**同一把写锁**里做完，连点两行也不会交错
  成「清了 B 又选上 A」；路径不在当前目录（深层列不走主模型）清完即返回 false，
  选择留**空**——编辑命令收到「没有选中文件」，宁空勿误伤。UI 侧
  `RootView::column_row_clicked` = 挪 cursor + 整替 + `pull_selection` 回灌面板。
  契约由 `mo-app/tests/productivity.rs` 一条钉住（含「键盘光标也被清」断言）。
* **缺口 2 的修法**：`grid::cell` 与 `columns::column_box` 把编辑行的名字格换成
  `Input::new(state)`（关外观、无边框，选择器同列表的 `mo-inline-rename`），
  提交走与 `file_list.rs` 同款的**两条路**：别的行 mouse_down →
  `end_inline_rename_on_click(Some(…))`；编辑行自身 `on_mouse_down_out` →
  commit。容器级 `end(None)` 没补——出格必过 mouse_down_out，覆盖已闭合。
* **测试**（`tests/inline_rename.rs` 六条）：网格 F2 编辑器可见 + Enter 落盘、
  网格点别的格提交（并断言编辑器收干净）、画廊共用 cell（Esc 取消、文件没动）、
  列视图点行 F2 落盘、列视图旧选区对照（删的必须是刚点的行，旧选区文件一根毛
  不掉）、深层列点击后选择必须为空。
* **harness 两条新坑位**：① kit 的 `window.click(selector)` 只认**元素路径注册表**
  里的元素（工具栏 / 侧栏 / 状态栏），中央虚拟化 / 滚动区里的单元会
  `missing ElementId … in scope []`——单元 / 行一律按 `debug_bounds` 中心坐标
  派发 down/up（grid_drag 的 `drag` 同款姿势，本次补成 `click_at` helper）；
  ② `Input` 是 gpui-component 组件、吃全局 `Theme`，测试窗口要先
  `cx.update(gpui_kit::init)`（layout.rs :2064 同款）。
* **变异反验四轮**（还原字节一致，红集互不重叠）：掐 `column_row_clicked` 的
  整替调用 → 恰红列视图三条；掐 grid 编辑器换名格 → 恰红网格 / 画廊三条；
  只掐 grid 行 mouse_down 提交路 → **全绿**（说明 `on_mouse_down_out` 单路即可
  闭合「点别处提交」，两条是冗余防线）；把两条一起掐 → 恰红提交用例；
  掐 columns 换名格 → 恰红列视图 F2 条。
* **剩缺口 3（P1，欠着）**：`ensure_columns` 只在第 0 列路径 ≠ tab.path 时重建，
  同目录内的增删改名之后**列视图不刷新**——本轮改名落盘成功，但列里显示的还是
  旧名（列数据来自 `list_dir` 快照，与 watcher 是两回事）。（已在 §41 补上。）

### §41：列视图接上同目录刷新——事件侧竖牌 + 原地重读（2026-10-03）

* **补的是 §40 缺口 3**：列视图数据走 `AppState::list_dir`（`LightEntry`，不走主
  目录模型），`ensure_columns` 此前**只按「第 0 列路径 ≠ 当前目录」判过期**——
  目录没换、内容变了（增删改名正落在显示的列里）时永远显示旧条目。列表视图靠
  `sync_panel` 回灌到主模型，这条路对列视图**结构上够不着**（两份数据）。
* **接线是「消费已有的事件」，不是新造机制**：watcher / 刷新链路本就在广播
  `AppEvent::DirectoryChanged{path}`（`mo-core::event`），列表视图靠它回灌。列视图
  补一条消费侧：`tab_loop` 收到该事件、且 `path` 正被这一页的某一列显示 →
  `mark_column_stale` 竖 `Panel::column_stale` 牌（只记事实、不读盘）→ 下一拍
  `ensure_columns` 的 `column_stale` 分支走 `reload_stale_columns` **原地重读**。
  牌与加载共用 `column_busy` 闸，事件来得再急也只排一次重读。
* **原地重读 ≠ 重建**：`reload_stale_columns` 逐列 `list_dir` 后**只换 `entries`、
  把 `cursor` 钳进新表**，列栈结构与下钻深度一律保住——用户正看着第 1、2 列，
  不能因为根目录多了个文件就给他拆回一列。这与 `load_column(.., None, ..)` 的
  「换根重建」（丢整条列栈）是两条不同路径，`ensure_columns` 里按 `stale`/`stale2`
  分流。重建路径顺手清 `column_stale`（读的就是此刻磁盘，旧牌随旧列栈作废）。
  在途若列栈被换目录 / 截断，按**路径**回找列：找不着说明该列已不显示这目录，
  过期事实自然作废（尽力而为，同 `watch_then_refresh_task`）。
* **路径门槛是硬判据**：`mark_column_stale` 只在 `columns.iter().any(|c| c.path ==
  path)` 时才竖牌。深层列没在听的目录广播不该惊动这一页——这条门槛由变异反验
  专门钉住（拆成无条件 → 对照用例红）。
* **测试**（`tests/column_refresh.rs` 三条）：① 广播新增文件 → 第 0 列原地读出、
  旧条目一根不少；② 下钻后广播删除 → 第 0 列缩、第 1 列还在、被点的最后一行
  cursor 钳到新表末行，且**对照组**（广播没在显示的目录 + 墙钟按住 800ms）里
  那目录的文件不冒出来；③ 竖牌后立刻换目录 → 重建到 new 目录、旧牌的异步重读
  结果不混进来。headless 里首标签页 watcher 泵不在（生产由 `run()` 起），测试直接
  握 `app.bus()` 投 `DirectoryChanged`——被测的是消费侧这条新链，发布侧语义不变。
* **变异反验四轮**（还原字节一致，红集互不重叠、各落预期断言）：
  * 掐 `tab_loop` 的事件消费 → 恰红「新增读出」与「下钻钳位」两条（重建不依赖牌）；
  * 拆 `mark_column_stale` 的路径门槛（无条件竖牌）→ 恰红对照用例
    （没在显示的目录的广播惊动了这一页，extra2 冒出）；
  * 把 `reload_stale_columns` 换成 `load_column(.., None, ..)` 重建 → 恰红下钻条
    （原地重读结果 `[]`：重建先把列栈清了，异步没落回）；
  * 拆 cursor 钳位 → 恰红下钻条，且正落在钳位断言（cursor=3 未收进 3 条的新表）。
* **收账**：§40 的三个缺口至此全清。列视图与主目录模型「两份数据」的根本张力
  仍在（刷新靠事件回灌、不靠共享模型），但四视图 × 编辑 / 拖拽 / 刷新都有接线
  与 headless 覆盖了。

### §42：拖拽中的视觉反馈——跟随光标的 ghost + 落点高亮（2026-10-06）

* **用户报的原始体验**（Windows 真机）：「拖动时没有任何反馈，只有松开鼠标才能
  看到效果；鼠标位置至少该有一个副本表示正在拖这个文件。」此前 `DragState` 只记
  `{pane, tab, paths}`——**没有位置、没有悬停、没有任何渲染侧消费它**，拖拽全程
  界面上一根毛都不动。接线与落点结算（§36~§39）都是对的，缺的是「进行中」这半。
* **数据源现成**：gpui 的 MouseMove 事件在按下后照常派发（Windows `SetCapture`
  保证窗口收得到），根容器给框选挂的**冒泡 `on_mouse_move`**（`update_box_selection`
  那条路）本来就拿得到窗口坐标；`MouseMoveEvent` 还带 `modifiers`，Alt 的
  复制 / 移动语义能逐拍流进状态。所以这轮**没有新造任何监听机制**，只是把
  这条已经在跳的脉接给了渲染。
* **`DragState` 加了五个反馈字段**：`cursor` / `begin_at`（按下起点）、`alt`、
  `engaged`、`hover`+`hover_at`（落点认领 + **坐标指纹**）。全部随 `drag` 一起
  取走即清零——结算、交出系统（`hand_off_drag_to_os`）、Esc 之外不存在过期
  反馈的第三条路。
* **落点认领的「一拍一指纹」协议**（本轮最不显然的一段）：同一次鼠标移动从
  最内层冒泡到根，行 / 窗格 / 根顺次看到**同一个坐标**。认领方先写
  `hover=(目标, hover_at=本坐标)`；窗格级发现 `hover_at` 已等于本坐标就不覆盖
  （目录行优先于整块窗格）；根是最后一站，`hover_at != 本坐标` 即「这一拍没有
  任何人认领」→ 清空高亮。于是「指针移开目录行 16ms 内高亮就撤」，不会有
  亮着的假靶子——而高亮判据与 `drop_on_entry` 逐条对齐（目录、不在被拖集合里、
  窗格级只在跨窗格），**亮着的落点抬起必定传输**，这条由测试直接兑现。
* **位移阈值从按下起点算（累计）**：`engaged` 只在离 `begin_at` 超 4px 后置位，
  原地按下抬起（普通单击）**全程无 ghost**——否则每次点击都闪一张卡，比没反馈
  更糟。认领方与根用同一个 `drag_in_motion` 判据、先到先置位，快拖第一拍落在
  目录行上也能当场亮。
* **渲染**：ghost 是根 render 末尾的绝对定位浮层（与橡皮筋同坐标系、同款
  无 interactivity 不参与命中），内容是「首项名 · N 项 · 复制/移动」，靠右缘
  自动翻到指针左侧；高亮是三视图行 / 格 / 列行的 `hover_bg()`（列表的 `bg`
  链排在选中前，网格 / 列视图同款），跨窗格空白处另给整块窗格一层
  `hover_bg().opacity(0.25)` 的绝对定位淡染。`begin_drag` / `begin_drag_single`
  改收 `&MouseDownEvent`：起点与 Alt 都从按下事件里取，参数个数不升反降，
  三个视图的调用点同步收敛。
* **测试**（`tests/drag_feedback.rs` 六条）：① ghost 过阈值才出现、逐拍跟随、
  抬起即消失，且同窗格空白落点不传输；② 目录行认领 / 文件行不认领 / 移到空白
  一拍即清 / 回到目录行抬起**真的复制进去**；③ Alt 逐拍翻动并按移动结算（源没了）；
  ④ 原地按下抬起全程无 ghost、单击选中照常（反馈没抢点击的活）；⑤⑥ 网格目录格、
  列视图目录行同款认领并兑现传输。headless 里 **MouseMove 第一次打进派发链**
  （dispatch + render_frame），行级认领与冒泡到根都验得住；`mo-drag-ghost` 走
  `debug_bounds`（无 interactivity 的裸浮层也进 selector 注册表，橡皮筋先例）。
* **变异反验六轮**（还原字节一致）：掐根上的拖拽分支 → 红①②③（光标不更新、
  过期不清、Alt 不翻）；掐 `stale_hover` 清空 → 恰红②且正落在「移开该灭」断言；
  掐列表行认领 → 恰红②落在「该亮」断言（与上一条红集同测试、异断言）；掐网格
  认领 → 恰红⑤；掐列视图认领 → 恰红⑥；拆 ghost 渲染的 `engaged` 闸 → 恰红①④
  （点击闪 ghost）。
* **人工实感清单（真机才能验）**：ghost 的字色 / 阴影在浅色与深色主题下是否
  协调；快速拖动时淡染 + 高亮有无闪烁感；分栏（跨窗格）窗格淡染的实际观感
  （headless 只验了单窗格，窗格级认领路径未直接测试覆盖）；拖出交接 OS 那一步
  （§25 老那条）顺带也该再看 ghost 是否在出手瞬间正确消失。
* **§42 遗留的语义旧账**：这一轮 ghost 上的「复制 / 移动」还是 §36 的旧规则
  （非 Alt 一律复制）——同卷宗直拖显示「复制」被判为不对，当轮改判，见 §43。

### §43：应用内拖放改资源管理器判定表——同盘拖=移动（2026-10-06）

* **用户报的原始场景**（Windows 真机）：「在一个目录里新建文本文件，拖到它的
  子目录上，显示的是复制——按道理应该是移动吧」，随后拍板「改成跟资源管理器
  一样的逻辑」。此前 §36 定的规则是**非 Alt 一律复制**：macOS 访达本来就是
  「应用内拖动恒复制」，移植 Windows 时把这条肌肉记忆带了过来，在资源管理器
  语义是默认移动的同盘场景里就成了别扭。
* **判定表**（`mo_app::drag_resolves_to_move`，三档优先级，从高到低）：
  1. **修饰键反向覆盖**：Ctrl=强制复制、Shift=强制移动（两颗键与资源管理器
     逐一对位）；**Alt 保留为移动别名**——资源管理器里 Alt 是「创建快捷方式」，
     Mo 没做这个功能，不留空位，且 mac 侧 ⌥=移动（§36 的既有肌肉记忆）不该断。
  2. **跨会话=复制**：本地↔远程、两条不同远程会话之间永远复制——「拖一下就把
     别人服务器上的文件挪走」不该是默认动作。会话同一性用 `Endpoint::same_session`
     （Local/Local 真；Remote/Remote 拿 `Arc::ptr_eq`；混合假）。
  3. **同会话看卷宗**：两端都本地按 `mo_platform::same_volume` 判（同盘移动、
     跨盘复制）；同一远程会话按同卷宗走，默认移动。
  多源取**第一个源**定整批动作（与资源管理器一致：引擎逐条 rename，混盘的
  那几条自己会退回 copy+删源，整批语义不破）。
* **`same_volume` 是纯路径形状的判定，不打盘**：Windows 取 `Component::Prefix`
  做卷宗钥匙（小写化、`\`→`/`、剥 `\\?\` 与 `\\?\UNC\` 前缀、裁 `/:`，于是
  `c:\`==`C:/`、verbatim==常规、同 UNC 共享同钥匙）；unix 的 `RootDir` 在
  `/Volumes/<名>` 下取挂载点名、否则记 `/`；判不出的形态（相对路径）返回
  `None`，**两端任一认不出就按同卷宗**（保守侧）。为什么保守不丢数据：
  `MoveOperation` 先 `rename`，失败自动退回「复制 + 删源」——判错的代价只是
  跨盘没坐上 rename 快车（慢一点），永远不会「既没复制成也没删掉」。
  纯字符串实现一份代码三平台可测（`\\?\` 前缀在 unix 的 Path 解析里照样成形，
  Windows 专属断言 cfg 门控，`/Volumes` 分裂断言全平台跑），也守住「UI 不碰
  文件系统」的分层——判据放在 mo-app，mo-platform 只答钥匙。
* **UI 接线**：`DragState` 加 `ctrl`/`shift`（按下与逐拍从 `ev.modifiers` 取，
  注意 gpui 的字段名是 `control`）+ `hover_dest`/`hover_move`。**认领那一拍就
  用 `drag_move_for` 算好动词**写进状态，抬起结算走**同一个函数**——ghost 上
  显示的与落下做的出自一张表，这就是「所见即所得」。悬停中改按键、坐标不动
  也要翻动词：`update_drag_cursor` 里 `redo_verb` 一支（认领指纹没变但
  `mods_changed` 时按新键重算）。动词只在**有落点认领**时显示（`hover.is_some()`），
  空白落点抬起什么都不传输，ghost 就不该许诺一个动词。注意 gpui（fork）在
  `Modifiers` 传进 `FileDrop` 提交时把键清零，所以 **OS 拖入不改**、仍恒复制
  ——这是与资源管理器的有意分叉，拿不准键就选最安全的动作（§36 的老纪律）。
* **测试**：`crates/mo-app/tests/drag_semantics.rs` 六条纯判定表（同盘移动 /
  跨盘复制 / Ctrl 覆盖含三键组合 / Shift+Alt 强制移动含跨盘跨会话 / 跨会话
  复制 / 同远程会话移动）；`mo-platform` 两条 `same_volume`（cfg(windows) 的
  盘符与 verbatim/UNC + 全平台的 `/Volumes` 分裂）。UI 集成四个文件按新语义
  翻改 + 加条：`internal_drag` 主用例改「移动进去、源没了」、新增 Ctrl 复制条
  （共 6）；`grid_drag` / `column_drag` 主用例同步翻改；`drag_feedback` 认领处
  加 `hover_move` 断言（同盘默认=Some(true)、悬停中按 Ctrl 翻 Some(false)、
  松开又翻回）、新增「Ctrl 悬停抬起真复制」一条（共 7）。OS 拖入两条
  `src.exists()` 断言**原样保留**——它们钉的就是那条分叉。
* **变异反验四轮**（还原字节一致，红集互不重叠）：① Ctrl 支掐成恒假 → 恰红
  `ctrl_forces_copy`+`shift_and_alt_force_move`（组合支）与 UI 三条 Ctrl 断言；
  ② `same_volume` 掐成恒不同 → 恰红 `same_volume_without_modifiers_moves` 与
  `/Volumes` 分裂条；③ Alt 别名掐掉 → 恰红 `shift_and_alt_force_move`（UI 的
  Alt 条同卷宗下与默认判定等值、红不到，别名只在跨盘/跨会话有意义，表级已钉）；
  ④ 认领侧动词掐成恒复制 → 恰红 `drag_feedback` 三条 `hover_move` 断言
  （列表 / 网格 / 列视图各一）。
* **真机才能验 / 已知分叉**：macOS 侧 Command 键**没有**映射进 Ctrl 的覆盖位
  （刻意：mac 肌肉记忆里 ⌘ 不承担「强制复制」），mac 用户反向覆盖仍走 ⌥=移动、
  跨盘默认也从「恒复制」变成了「看卷宗」——访达老用户可能觉得新鲜，devlog
  记在这里备查；Shift 强制移动在跨盘大目录上的耗时观感、Ctrl 键位与浏览器 /
  资源管理器肌肉是否打架，都得人手拖一把。

### §44：同目录刷新不再丢掉已解码的缩略图——「图标闪一下」的账（2026-10-06）

* **用户报的现象**：「新建一个文本文件的时候，文件列表的图标会闪一下——比如一个
  图片，会变成默认的图标，然后再变回图片原本的缩略图。」
* **机制**：新建之后走的是一次**整目录重读**（`mo-ui` 里 `let _ = app.refresh().await;`
  → `mo_app::load_path`），新 `Directory` 里每条 `Entry` 都是 `Entry::new` 出来的
  `ThumbnailState::Idle`——旧快照那些 `Loaded(Arc<Bitmap>)` 随旧 `Vec<Entry>` 一起被丢。
  回填是**异步的一整圈**：UI 下一拍才给可见的 `Idle` 行排队（`file_list.rs` →
  `ThumbnailScheduler::request`）→ 信号量限 4 并发 → `spawn_blocking` 里查磁盘缓存 +
  解码 → `set_thumbnail` → 还要等 120ms 的广播合并才发一帧。中间那一两帧画的是
  fallback（系统图标 / 通用 glyph），于是看得见「闪」。`ThumbnailState::Loading`
  从来没被置位（条目从 `Idle` 直接到 `Loaded`/`Failed`，见 `thumbnail.rs` 的注释），
  所以「旧的那张图」在模型里没有任何东西持有——刷新一次必然回到起点。
* **修法**：`load_path` 把新快照换上去**之前**，`thumbnail::carry_thumbnails(&mut dir, old)`
  按 `FileId` 把旧目录里的 `Loaded` 位图搬过来。三处都是刻意的：
  * **只搬 `Loaded`**：`Failed` 搬过去会把一次偶发解码失败钉死在这一行上（刷新本来
    是该给重试机会的），`Idle` 没东西可搬；
  * **键取 `FileId`，不是下标也不是路径**：刷新时新文件可能**插在老文件前面**
    （按名排序），按下标抄会同时错两处——新行拿到别人的图、老行丢了图；
  * **只在旧目录存在时搬**：换目录时新旧 `FileId` 集合不相交，这一步自然什么都
    搬不动，不会串图。
* **搬旧位图不引入「过期缩略图」这个新类别**：磁盘缩略图缓存的键本来就是 `FileId`
  （`ThumbnailCache::cached_path_with`，判据里没有 mtime），内容变了要靠缓存侧去
  更新这件事，搬运前后一个样；搬运只是让**同一张已解码的位图**多活过一次刷新。
* **守卫落在 `mo-app` 而不是 UI**：被改的就是模型层这一处搬运；UI 那侧
  `Panel::window` 是 `list_window` 直抄（`Entry::clone` 连着 `Arc` 一起带），
  `file_list.rs` 只给 `Idle` 行排队，`file_item.rs` 见 `Loaded` 就同步画位图——
  没有第二处状态会被刷新打掉。而 headless 里要真起 tokio 驱动 `set_thumbnail`、
  再真解码一张图才摸得到点上，与 `watcher_refresh.rs` 是同一取舍（理由写在测试
  文件头）。
* **测试**（`crates/mo-app/tests/thumbnail_carry.rs` 三条）：① 主 case——`pic.png`
  落了 `Loaded`，新建 `new.txt` 触发刷新，那一格还是**同一张**位图（拿
  `Bitmap::id()` 比，防「又排了一次队、恰好也成功」蒙过去）；② 对照——新出现的
  `a_late.png` 必须仍是 `Idle`（不许凭空造图）且 `pic.png` 保住图，前置断言钉住
  「新文件确实排在 `pic.png` 前面」，不然这条对照就退化成 ①；③ 只搬 `Loaded`——
  置 `Failed` 后刷新该回到 `Idle`。
* **变异反验三轮**（还原字节一致，红集互不重叠）：掐掉 `carry_thumbnails` 的调用 →
  恰红①②（③ 绿，它判的就是回到 `Idle`）；把 `Failed` 也一起搬 → 恰红③；改成
  **按下标抄** → 恰红①②（② 里两处断言各错一边：新行被塞图、老行丢图）。
* **没修 / 边界**：列视图走 `LightEntry`，那一层本来就没有缩略图（§41 记过的
  「两份数据」张力），不在本轮范围；**切目录**后首屏仍会从 fallback 长成缩略图——
  那是真·首次解码，没有可搬的东西，不属于用户抱怨的这一下。

### §45：拖动多个文件做移动时的几秒钟冻结——账在搜索索引，不在拖拽（2026-10-06）

* **用户报的现象**：「拖动多个文件进行移动时，界面会卡住几秒钟」。截图是
  `D:\Users\wmf12\Pictures`，四个 `新建文本 N.txt` 选中，ghost 上写着
  「新建文本 2.txt · 4 项 · 移动」，状态栏「15 项 · 已选 4 · 可撤销」「已索引 581003」。
* **先排掉的两条**：把 §42/§43 那条链路整个读了一遍——`begin_drag` 只收路径、
  `update_drag_cursor` / `drag_move_for` / `note_drag_*_hover` 全是纯计算（`same_volume`
  只比字符串形状，不碰盘），ghost 每帧只拼两段文案；抬起那侧 `run_transfer` →
  `transfer_between` → `submit_operation` 的真活在 `spawn_blocking` 里。逐拍与结算都没有
  主线程 IO，所以卡顿不可能是「拖四个文件」这个动作本身。
* **机制（两处，都在索引上，且互相放大）**：
  1. §43 把同盘直拖改成默认**移动**之后，一次拖动 = N 次「rename 出当前目录」
     （`apply_watcher_event` 的 `Renamed` 且父目录不同 → `remove_listing_entry`
     → `sync_index_removed`）。它落到 `FileIndex::remove_under`，旧写法是
     `DELETE ... WHERE path LIKE '前缀/%'`：LIKE 默认大小写不敏感，前缀通配用不上
     索引，`EXPLAIN QUERY PLAN` 给的是 `SCAN files`。在**用户机器上那份真索引的副本**
     （580997 行 / 276MB）上实测**一次 147ms**——而且**未命中也一样贵**（要扫完才知道没命中），
     四个文件就是四趟，期间 `PlMutex<FileIndex>` 一直被攥着。
  2. 同一时间，主线程的事件总线每收到一条 `DirectoryChanged` 就跑一次 `sync_panel`，
     里面 `let indexed = app.index_count();` 是同步的 `index.lock().count()`，而 `count()`
     是一条 `SELECT COUNT(*)`：同一份库实测 64ms 全表扫。**一个在扫、一个在等锁**，
     几拍叠起来就是「几秒钟」。这与当年「主目录自举期间隔几秒冻几秒」是同一族病
     （那一轮收的是爬取侧的锁，见 `index_root_capped` 与 `crawl.rs` 的注释），
     这次剩下的两把刀是 `remove_under` 和 `index_count`。
* **顺手抓到两条正确性**（都在 `remove_under` 那一行上）：
  * LIKE 把 `_` 与 `%` 当通配符——删 `a_b.txt` 会连邻居 `axb.txt` 的子树一起抹掉；
  * Windows 落库的是**反斜杠**路径，`{前缀}/%` 一个孩子都匹配不上：本机删 / 移一个
    目录，等于只删了目录自己那一条，整棵子树留在索引里当孤儿（「搜得到、点不开」）。
* **修法一**（`mo-search::index.rs`）：`remove_under` 改走 path 的**字节范围**——
  `/`=0x2F、`\`=0x5C，各取「下一个字节」当右开上界，两段 `[前缀/, 前缀0)` 与
  `[前缀\, 前缀])` 各扫一遍（两种分隔符都扫，同一个函数在 macOS 与 Windows 上都成立），
  再加原来那条等值 `remove`。`EXPLAIN` 于是变成 `SEARCH files USING INDEX idx_path
  (path>? AND path<?)`，同一台机器上未命中 **33µs**（147ms → 33µs）。
* **修法二**（`mo-app`）：`index_count()` 不再在调用线程上碰 SQL——进程内第一次仍就地数
  一遍（「重开应用后索引还在」那条判据要当场成立），此后读 `(Instant, usize)` 缓存，
  过期时按 `INDEX_COUNT_TTL`（1s）派发一次后台重算，派发有单飞标志（`indexed_pumping`）
  免得每拍往 blocking 池塞一条 COUNT。`ensure_index_started` 把它启动时那一次 COUNT
  顺手种进缓存，主线程从此不再为状态栏那个装饰数字排队等索引锁。
  与 `net_shares` / `volumes_cache` 是同一族做法（那两个本来就有 TTL）。
* **探针留在仓库**：`crates/mo-search/examples/probe_freeze.rs`——
  `cargo run -p mo-search --release --example probe_freeze -- <索引库副本>`，
  打 `COUNT(*)` / LIKE / 范围三种墙钟与三条查询计划。上面的数字都是它量出来的，
  下轮再遇到「索引慢」不必重新搭一遍。
* **测试**：`mo-search` 单测两条（`remove_under_covers_backslash_children` 反斜杠子树、
  `remove_under_does_not_treat_underscores_as_wildcards` 邻居不误伤）+
  `crates/mo-app/tests/index_sync.rs` 三条：① 移走的文件从搜索里消失；② 移走的**目录**
  连子记录一起消失（这一条在 Windows 上正是旧写法的错法）；③ 缓存的那个数字会被后台
  追上来。③ 之所以必须存在：变异反验时发现把「派发重算」摘掉，**既有**的
  `global_index.rs` 与 `productivity.rs` 全绿（前者断言打在 `global_search` 上，后者的
  重启那半走「第一次就地数」的分支）——那条路原本没人守。
* **变异反验两轮**（还原字节一致）：`remove_under` 改回 LIKE → 恰红单测两条 +
  `index_sync` ②（红在「子记录还留在索引里」，端出来的正是那条反斜杠路径）；
  摘掉重算派发 → 恰红 `index_sync` ③，其余全绿。
* **没修 / 边界**：
  * 命令面板那条 `global_search` 仍是主线程一次 `LIKE '%q%'` 全表扫（同一份库约
    150ms/次按键）。同一张表同一个量级的病，但那是搜索线的账，不在这条拖动路径上，
    已记进上面的待办。
  * `files.path` 的 `UNIQUE` 已经自带一份索引，`idx_path` 是第二份（写放大一倍）。
    本轮反而靠它做范围扫（`EXPLAIN` 显示等值走 `sqlite_autoindex_files_1`、范围走
    `idx_path`），要不要收成一个留给搜索线一起判。
  * 跨目录移动后**新位置**不进索引：watcher 只听当前目录，`note_visited` 要用户真走进去
    才爬。这一条本轮没改（旧行为一样），所以「刚移进来的东西暂时搜不到」仍在。

### §46：全局搜索每敲一个字的那一下卡顿——第三把刀也离开了主线程（2026-10-06）

* **来路**：§45 的收账里点名留下的同一族第三处：`Modal::GlobalSearch` 的按键处理
  里，每敲一个字符（以及每次退格）都**同步**跑一次 `AppState::global_search(&q, 50)`。
  底层是 `FileIndex::search` 的
  `WHERE name_lower LIKE '%词%' OR path LIKE '%词%' ORDER BY … LIMIT 50`——中缀通配
  用不上任何索引，用户那份 58 万行真索引上实测约 **150ms/次**（同一份库的探针见
  `crates/mo-search/examples/probe_freeze.rs`，§45）。症状与 §45 不同但病灶同族：
  那一次是「拖四个文件冻几秒」，这一次是「搜索框打字一下一下地卡」。
* **修法一**（`mo-app`）：加 `global_search_bg(query, limit)`——整趟查询派到进程级
  blocking 池，调用线程只等一个 future，一次都不碰索引锁、一条 SQL 都不发。同步门面
  `global_search` **保留**（测试拿它当量具最直接），文档里写明「别在主线程调」。
  空查询在进池之前就返回空，不占池。
* **修法二**（`mo-ui`）：`run_global_search` 派发 + **趟号**（`search_epoch`）对账：
  出发时记下号，回来只对得上当前号且模态还开着的那一趟，其余落地即丢。另外把散在
  五处的「开/关搜索模态就清这几个字段」收进 `reset_global_search`（清词、清结果、
  归零选中行、作废旧趟——旧写法只有字段赋值，没有作废这一手）。结果区空着时
  「搜索中…」与「没搜到」「索引正在建立」三种提示分开。
* **取舍**：不去抖、也不中断上一趟（索引查询没有中断点，SQL 已经在跑了）。打字期间
  确实会同时飞好几趟，代价是池上空跑几趟，换来回显永不等 SQL；这与 §45 的
  「第一次就地数、之后读缓存」是同一个判断——**装饰性的等待一律挪出主线程，
  而不是想办法让它变快**。
* **测试**：`crates/mo-ui/tests/global_search_bg.rs` 三条——① 敲下那一拍「回显已跟上、
  在途已立、结果仍空」，命中下一拍才来；② 关掉模态又打开，看不见上一次查询的行；
  ③ 退格到空查询就地清空且不派发。索引库按测试进程共享，故 fixture 名带 tag，
  断言只认「不止一条」。
* **① 起初是条会翻脸的测试（当天全量闸门红过一次）**：① 原来用
  `cx.simulate_keystrokes` 派发那个 "z"，再断言「这一拍必须在途」——而 `simulate_keystrokes`
  顺带跑一轮 parking，测试索引又只有三枚文件，后台那趟完全可能在**同一个 parking 里**
  就落了地，于是 `busy` 变成掷硬币。单跑六次全绿、全量里红一次，正是这种测试的样子：
  它测的不是守卫，是谁先跑完。换成 `vcx.update(|window, app| window.press("z", app))`
  那条**不泵调度器**的派发（与 ② 统一）之后连跑九次全绿。**同一轮里凡是「断言在途/过期」的按键，
  派发方式必须一致**——只改 ② 不改 ①，等于把同一个坑留了两份。
* **变异反验两轮，外加一条测试纪律**：
  * 摘掉趟号/模态守卫 → 恰红 ②（6 行进新模态），①③ 绿。
  * 改回主线程同步扫表（`run_global_search` 里直接 `app.global_search` 后同步写结果）
    → 恰红 ①（「查询已派发、还没落地：这一拍必须在途」）与 ②（「先确认查询确实在途：
    query=z hits=3 busy=false」），③ 绿。这一轮是在 ① 换成 `press` 之后**重跑**的，
    免得那条改成只测运气的断言冒充成守卫。
  * **踩坑记录**：② 最初用 `simulate_keystrokes` 派发，摘掉守卫**仍然全绿**——那条
    派发会顺带跑一轮 parking，在途那趟在「关闭模态」之前就落了地，而关闭本身清结果，
    于是守卫看着像起作用、其实压根没被测到。换成 `window.press`（同步派发、不泵
    调度器）才真造出「过期结果落在重开之后的模态里」这一格。**测「在途 / 过期」这一类
    行为，按键必须用不泵调度器的派发**，否则测的是清场动作而不是守卫。
* **没修 / 边界**：
  * `FileIndex::search` 本身仍是全表扫——本轮只是让它**离开主线程**。要真的快得起来
    得上 FTS5 或 trigram 索引（中缀匹配没有索引可依），那是搜索线的下一笔账，
    动的是库结构与爬取成本，不该顺手改。
  * §45 记过的 `files.path` UNIQUE 自带索引 + `idx_path` 重复一份，仍未收。
  * 在途那趟不被中断：极端慢盘 + 大索引时，池上可能同时挂着几趟旧查询。上限就是
    打字速度，且 `LIMIT 50` 决定了结果行数，不是无界放大。

### §47：左下角任务卡片的左右留白不一样——一条注释里的 6px 从没进过布局（2026-10-07）

* **来路**：用户截图（`D:\Users\wmf12\Pictures`，「8 个任务 100%」那张常显卡片），
  卡片左右两边各画了一个红箭头，配一句「这里左右间距不一样」。不是对齐漂移，
  是**右边几乎贴到侧栏边缘、左边留着一段**。
* **根因是一道算术题**：侧栏宽 `SIDEBAR_WIDTH = 188.0`（`app.rs:30`），卡片的定位壳
  相对**窗口**摆位、`left(px(10.0))`（`progress_panel.rs:66`），卡片本身宽 `CARD_W`。
  旧值 176 时右缘落在 `10 + 176 = 186`，离侧栏右边界只剩 **2px**——于是左 10 / 右 2。
  `CARD_W` 那行注释写的是「与侧栏同宽（188）减去左右 6px 留白」，可**这 6px 从来没进
  过布局**：左边距实际由壳的 `left(10)` 决定，右边距只是「剩下的」，注释里那个对称的
  留白数字是凭想象写的。常量对不上布局，布局就会照着布局走。
* **修法**：`CARD_W` 176 → **168**（`188 − 10 − 10`）。为什么是 10：这条竖线上别处
  都用 10——卡片的壳 `left(10)`、状态栏容器 `px(px(10.0))`，卡片跟着同一条节奏才不像
  贴边。注释改成把算式写全，并点明「两边留白必须相等」这条才是这张卡的约束。
  其余尺寸（`CARD_H 30`、浮层 `POPOVER_W 440`、进度条 `mx(10)`、壳 `bottom(32)`）不动。
* **测试**：`crates/mo-ui/tests/layout.rs::badge_is_centered_in_the_sidebar_column`
  ——读 `mo-sidebar` 与 `mo-ops-badge` 的真实 bounds，算出左右留白，断言
  ① 两者之差 ≤ 1.5px（布局按整像素取整，两侧各可能差 1px），② 左留白落在 8~12px
  即 10px 节奏上。这条断言把 `SIDEBAR_WIDTH`、壳的 `left`、`CARD_W` **三个常量绑在
  一起**：以后谁再改宽了卡片，红的就是它，而不是用户的眼睛。
* **变异反验**：`CARD_W` 改回 176 → 恰红这一条，报错把用户那句话原样还了回来——
  「卡片在侧栏列里左右留白不等：左 10px / 右 2px」；还原（cp 回来 + `touch`，再 `diff`
  确认与备份一字不差）后复跑绿。断言读的是 bounds，所以变异体里动的只有产品代码那一个
  数字，测试一字未改。
* **边界**：卡片壳是相对**窗口**摆位的（`left(10)`），隐藏侧栏时它照样贴窗口左缘——
  那没有「侧栏列」可对称，也没人报过；本轮没把卡片搬进侧栏子树，那会牵动浮层的弹出锚点
  （`POPOVER_W 440` 宽于侧栏，盖在内容区上本就是浮层的本分）。

### §48：任务进度从细条改成环，百分比写进环里——直径是反推出来的（2026-10-07）

> ⚠️ **本节整体已被 [§48 订正](#48-订正环在真机上被判太难看撤掉进度只剩百分比2026-10-07) 推翻**：
> 环真机观感被用户判死，代码已 revert。留着是因为里面那几条是实测出来的组件 API 事实，
> 下次再碰环形进度不必重量一遍。**现在的形状请以订正那节为准。**

* **来路**：用户一句话「progress_panel 里面的进度条改成环形进度条，然后百分比放到环形
  里面」。两处追问定了范围：① 卡片那条通栏聚合条**和**浮层每一行的细条都换（不是只换
  卡片）；② 环里写**完整**的「47%」（连百分号），卡片可以为此长高。
* **用 kit 的环，没自拼**：`gpui_kit::component::progress::ProgressCircle`——`canvas` +
  lyon 折线段 + `Window::paint_path` 真画弧。为什么不能拿渐变糊一个：这个 fork 的
  `BackgroundTag` 只有 `Solid / LinearGradient / PatternSlash / Checkerboard`
  （`gpui-pre-0.3.7/src/color.rs:745`），**没有 conic / radial**，糊不出环；
  `svg().data(…)` 每次改值都要重解析一份文档，而进度是持续动的，不合适。
* **组件四个 API 坑，每个都实测过**：
  * `.value(f32)` 吃 **0~100**，不是 0~1 的比值（内部 clamp 到 `0..=100`）。传比值的话
    环永远只画到 1% 都不到。
  * 直径用 `Styled` 的 `.w/.h` 钉。`.size(px(…))` 与它等价（实测 `.size(px(48.))` 画
    48）；**但 `Sizable::with_size` 不是**：`progress_circle.rs:175` 把 `Size::Size(s)`
    折成 `this.size(s * 0.75)`，实测 `with_size(px(48.))` 画出来 **36**。
  * `.color()` 要 `impl Into<Hsla>`（项目里到处是 `Rgba`，有 From impl 直接传）；底色
    轨道是同一支色的 `opacity(0.2)`。children 由组件自己 `items_center/justify_center`
    居中，环内文字塞一个 child 就行。
  * 它读 `cx.theme()`，而 `Theme` 是 `gpui_kit::init` 装进 app 全局的：生产在 `run()`
    里装了，`crate::theme::apply_component` 原来在**没有 Theme 时早退不装**。第一次全量
    闸门因此红了 **24 条**（`column_drag` / `drag_feedback` / `grid_drag` / `os_drop` /
    `internal_drag` / `column_refresh` / `session` 七个测试二进制，全是
    `no state of type gpui_component::theme::Theme exists`）——我只在 `layout.rs` 的
    harness 里补了 `gpui_kit::init` 就以为收工了。**修法不是挨个补 harness**：
    `apply_component` 改成「缺就 `gpui_kit::init` 补装」（`theme.rs`），建 `RootView`
    的入口立刻全好了，`layout.rs` 里那句补的 init 也撤了。判据：**一个约束要靠「每个
    调用方都记得」才成立的，就该收到共同上游去**；七个二进制同一条 panic 就是它本来
    不该由调用方记的证据。
* **直径是算出来的，不是挑的**：笔画宽 = `min(0.15×直径, 5px)`，环内净空 = 直径 −
  2×笔画。环里最宽的字是「100%」，9px 字号在 headless 实测宽 **22px**。直径 32 → 笔画
  4.8 → 净空 22.4，只剩 **0.4px**：那是赌字体度量，换个字体/缩放字就骑到环壁上。直径
  **36** → 笔画封顶 5 → 净空 26.0，留 4px。于是 `RING_D = 36`，卡片 `CARD_H` 30 →
  **42**（36 + 上下各 3），行保底高 `ROW_H` 56 → **70**（`DESC_LINE_H 16 + gap 4 +
  RING_D 36 + py 14`），`estimated_row_h` 的算式跟着换——滚动区高度是**算**出来的，
  估算与布局必须同式。
* **改了什么**：卡片的 `mo-ops-bar`（3px 通栏细条 + 行尾一个 `{:.0}%` 文字）整块删掉，
  换成聚合环；行里原本是「描述 / 细条 + 百分比 / 速度·ETA」，现在第二行是「环（环内
  百分比）+ 行尾速度 · 剩余时间」——**进行中的行不再有两处百分比**：环说「多少」，行尾
  说「多快」。卡片与行共用同一处构造 `progress_ring()`，直径、字号、笔画算式只有一份。
  卡片上那段「细条通栏会从 8px 圆角底下戳出去」的权衡注释随细条一起删了
  （`.overflow_hidden()` 留着，卡片背景和边框本来就要裁）。
* **测试**（`crates/mo-ui/tests/layout.rs`，两条新的，断言全打在真实 bounds 上）：
  * `aggregate_ring_keeps_its_diameter_label_and_corners`：① 环是 `RING_D` 的正圆
    （宽高差 ≤ 1px）；② 环内文字实测宽 ≤ 净空 + 0.5px；③ 环的方框不与卡片四个 8px
    圆角块相交。种的任务是 100%，专门逼出最宽的「100%」。
  * `popover_rows_use_the_same_ring_as_the_badge`：行里的环与卡片的环**同一尺寸**，
    且环完整落在行框内。
  * 判据要的直径/字号从 `mo_ui::progress_ring_geometry_for_tests()` 取，不在测试里抄
    数字——§47 刚收过「注释里的几何数字对不上布局」这笔账，抄进测试是它的孪生写法。
* **变异反验**（四个，每个都 `cp` 备份 → 种 → 跑 → `cp` 还原 + `touch` + `diff` 确认
  RESTORED_OK；动的只有产品代码，测试一字未改）：
  * `.w/.h` → `.with_size(px(RING_D))`：红在①，「环画成 Size { 27px × 27px } 了：要的
    直径是 36 的正圆」——36×0.75=27，正是那条 API 坑本尊。
  * `RING_D` 36 → 30：红在②，「环内文字宽 22px，环内净空只有 21.0px（直径 30，笔画
    4.5）：字会骑到环壁上」。
  * 卡片行 `px(px(10.0))` → `px(px(2.0))`：红在③，「环与卡片第 1 个 8px 圆角相交」并
    打印两边 bounds（`flex_1` 的描述把环推到右缘，2px 内边距让它压进圆角）。
  * 行里的环另加 `.w(px(24.)).h(px(24.))`：红在「同一尺寸」那条，「行里的环与卡片的环
    尺寸不同：行=24px 卡片=36px」——防的就是两处各写一份尺寸后分家。
  * **把自己写错的一条改回来了**：注释与报错原文写的是「用 `.size()` 会折成 0.75」，
    那是把 `Styled::size`（就是 w+h）当成了 `Sizable::with_size`。实测 `.size(px(48.))`
    画 48、`with_size(px(48.))` 才画 36。文案跟着证据改了——注释里的事实也得能被布局
    验出来，不然和 §47 那条凭想象写的 6px 是同一种债。
* **边界 / 剩真机看**：环的观感 headless 判不了——9px 的「100%」在 36px 的环里够不够
  清楚、5px 笔画配这套深色卡片粗不粗、卡片长到 42px 后与状态栏（壳仍 `bottom(32)`）
  叠不叠得好看，都要用户扫一眼。另外 `.value` 走 `transition(…)` 动画（时长取主题
  motion token），headless 测不到「在动」这件事，只验了落地后的几何；环色就是
  `status_color(op)`，完成/失败/进行中的区分度与细条时代一致。

### §48 订正：环在真机上被判「太难看」，撤掉——进度只剩百分比（2026-10-07）

* **结论先说**：上面整节写的环形**已经不在了**。用户真机扫一眼给的判词是「太难看了」，
  追问后定形「环和细条都不要，只剩数字」：卡片左「N 个任务」右「65%」，行上行描述、
  下行「30% · 3 MB/s · 剩余 12s」（结束态的行只给中文状态）。`CARD_H` 退回 30、
  `ROW_H` 退回 56。
* **怎么撤的**：一个正向 `revert` 提交（`e9c9af9`）把 §48 的实现整体摘掉，不改历史；
  再在其上重写百分比版。**连 `theme.rs` 那处 Theme 补装一起撤了**——它是为了渲染
  `ProgressCircle` 才改的，环走了它没有别的用途，留着就是一条没人走的上游分支（那条
  「一个约束该收到共同上游」的判据本身仍然成立，只是这次没有约束需要它守）。
* **保留下来的账**（这节前面那几条实测结论别当白干）：kit 只有 `ProgressCircle` 一个
  真环（没有 conic/radial 渐变，糊不出来）、`.value()` 吃 0~100、`with_size` 折 0.75
  而 `size` 不折、环内文字宽度要靠实测反推直径——以后再碰环形进度直接从这里取，
  别重量一遍。
* **新形状的判据**（`tests/layout.rs`，两条，旧那条 `aggregate_bar_stays_inside_the_badge_corners` 随细条删了）：
  * `badge_percentage_is_the_aggregate_and_stays_inside_the_corners`：选择器名带的是
    **渲染出来的那串字**（`mo-ops-pct-65`），所以「卡片显示的是聚合比」这条断言只能验
    到帧上的真值；种 30% + 完成(100%) 两趟，`mo-ops-pct-30` / `mo-ops-pct-100` 出现即
    红（聚合写成「取第一条」就抓得住）。顺带把「文字不许从 8px 圆角底下戳出去」这条
    判据从进度条转交给百分比文字。
  * `popover_rows_show_percent_only_while_the_task_is_alive`：`shows_percent` 只放行
    排队 / 进行中——「完成 100%」是重复信息，「失败 40%」看着像还在走；速度还没差分
    出来时尾标**整块不画**（空 div 会白吃一个 gap）。
  * 三个变异各自恰红：聚合比改「取第一条」→ 红在聚合那条；`shows_percent` 恒真 → 红在
    「已完成的行不该再显示百分比」；去掉空尾标的 `when` → 红在「不该挂着空尾标」。
* **还在的债**：观感仍然只有真机能判（这次就是被真机判掉的）。headless 能守的只是
  「数字对不对、有没有多画一处」，配色、字号、行高的好坏它永远说不上话——**这条线的
  教训**：装饰性几何做完一轮就交给用户扫一眼，别等它长出判据、反验、发版之后再退回。

## 49. 全局搜索真优化：FTS5 trigram 取代 `LIKE '%词%'` 全表扫（2026-10-08）

* **根因（§46 只治标）**：`FileIndex::search` 一直是 `name_lower LIKE '%词%' OR path LIKE '%词%'`。
  前导 `%` 让 `idx_name` / `idx_path` 完全失效，58 万行库上实测 ~150ms/次全表扫；§46 只是把
  查询搬出主线程（`global_search_bg`），治了「卡主线程」却没治「慢」本身——后台化后每次仍是
  全表扫，只是排在 blocking 池。devlog 里那句「要真快得上 FTS / trigram」就是这一笔账。
* **修法**：bundled SQLite（rusqlite 0.40，`build.rs` 带 `-DSQLITE_ENABLE_FTS5`，SQLite 3.4x
  自带 trigram 分词器）直接上 FTS5。给 `files` 表挂一张外部内容虚表 `files_fts`
  （`content='files'` + `content_rowid='id'`，数据只存一份，rowid 复用 `files.id`），对
  `name_lower` / `path` 用 `tokenize='trigram'` 建中缀子串索引。`search` 改成
  `FROM files_fts JOIN files f ON f.id = files_fts.rowid WHERE files_fts MATCH ?`，trigram 把
  查询短语直接落到索引——不再扫基表。相关度排序（文件名前缀优先、其次路径更短）保留，只作用在
  fts 已收窄的候选集上。
* **同步交触发器**：`files_ai` / `files_ad` / `files_au` 三个触发器挂在 `files` 表上——
  `upsert`(插/改)、`remove`、`rename`、`remove_under`(批量删) 走实体表时 fts 自动跟着增删改，
  `search` 这边完全不碰同步逻辑，也不用在 `upsert_batch` 等写入路径上加一行。触发器用外部内容
  的 `'delete'` 命令做删除，避免直接动 fts 虚表。
* **旧库迁移**：升级前用户的磁盘索引没有 `files_fts`。`init()` 在建表后判断「`files` 有数据且
  `files_fts` 为空」才一次性 `INSERT INTO files_fts(rowid,name,path) SELECT id,name_lower,path
  FROM files` 回填；全新库（`files` 空）与升级后（fts 已非空）都跳过，只在那「有数据却没 FTS」
  的一刻跑一次（58 万行约一两秒）。回填后增量走触发器，两端始终一致。
* **查询转义**：`MATCH` 的查询串包成带双引号的 FTS5 短语（`q.replace('"', "\"\"")` 翻倍内部
  引号），规避括号 / `*` 等 FTS 语法字符在查询里炸开；trigram 对短语做大小写不敏感子串匹配，
  正好等价于原来的 `LIKE '%q%'`（且不再把 `_` / `%` 当通配符——比原 LIKE 更「所见即所搜」）。
* **验证**（`crates/mo-search/src/index.rs` 测试，全绿）：
  * `search_uses_fts_index_not_full_scan`：`EXPLAIN QUERY PLAN` 锁定 `SCAN files_fts VIRTUAL
    TABLE INDEX 0:M2`（trigram 命中）+ `SEARCH f USING INTEGER PRIMARY KEY`（基表走 rowid 主键
    查找），断言不再出现对 `files` 的全表 `SCAN`；并行为面确认含子串的文件仍能命中。
  * `search_finds_substring_not_just_prefix`：子串藏在文件名中段（不在开头）也必须命中——
    这正是换掉前缀索引、改走 trigram 的意义。
  * 既有 `upsert_and_search_by_name` / `search_is_case_insensitive_and_path_aware` /
    `remove_under_*` / `remove_and_rename_keep_index_consistent` 全过——触发器同步路径被这些
    删除/改名用例覆盖。`mo-app` 的 `global_search_finds_files_across_subdirs` 及 `global_index` /
    `index_sync` 三个集成测试文件（共 21 例）也全过。
* **剩的账**：
  * 查询长度 < 3 字符时 trigram 退化为对 `files_fts` 虚表全扫（仍比扫 `files` 基表快，且已随
    §46 后台化）；极短查询属少数情况，先不动。
  * FTS 只索引 `name_lower` + `path`（文件名/路径子串搜索）。**文件内容搜索是另一条线**
    （`mo_search::content::search_content`），与本索引无关，不受影响也不在此优化范围。
  * 升级后首开若索引很大，回填那一次会有可感知的两三秒停顿（一次性）；之后无感。
