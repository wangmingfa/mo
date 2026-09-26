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
* ⚠️ **没修完**：mo-app / mo-operations 的集成测试（`hidden_files`、`large_dir`、`navigation`、`staging`、`sync`、`watcher_refresh` 等十余个）各自 `set_var` 之外不钉缓存，仍在写真实 `search.sqlite`。它们的 app 构造各写各的，要一起收口得先给 mo-app 补一个同款 helper。

## 22. §15 那张折表只认 US 布局：改成问当前键盘布局

* **为什么要改**：「哪个符号是哪个键的 Shift 变体」是**键盘布局**的事，不是常理。2026-09-26 用 `VkKeyScanExW` 实测两种布局的配对（`.tmp/vk.ps1`，同一套 API）：US 上 `+` 是 Shift+`=`、`?` 是 Shift+`/`；德语上 `+` 自己就是一个键、`:` 才是 `.` 的 Shift 变体、`"` 是 Shift+`2`、`{` 要 AltGr+`7`、而 `?` 那个键未加 Shift 打出的是 `ß`。照 US 表在德语上折，等于把用户按下的键绑到另一个动作上，默认键位反倒按不出来。
* **修法**：`mo_platform::unshifted_key(ch)`——`VkKeyScanExW` 查出这个字符要按哪个虚拟键、什么 Shift 态（只认「无 Shift」与「Shift」两种，AltGr / Ctrl / Alt 出来的一律不答），再用 `MapVirtualKeyExW(vk, MAPVK_VK_TO_CHAR)` 取该键**未加 Shift** 的字符；死键（bit15）与非 ASCII 结果都不答。`fold_typographic_shift` 先问布局、问不到才退 US 表。
* **两条不明显的规则**：
    * 布局**答了就不再退表**，哪怕答的是数字或字母（德语上 `"`→`2`）：表里那条 `"`→`'` 在这一列是错的，拿它兜底就把两个不相干的键又并到一起。
    * 「基本键也得是符号键」这条规则从「表里不记数字」改成了真判据（`is_symbol_char`），因为布局会给回数字（`!`→`1`）——视图模式占着 `cmd+1`~`cmd+4`，不判就白送一次误触。
* **macOS 仍是 US 表**：gpui 的 `Keystroke` 只给字符、不给虚拟键码，那边问不出布局。`unshifted_key` 在非 Windows 直接答 `None`，调用方退表——已知缺口，不是漏判。
* **单测不碰真布局**：`fold_typographic_shift` 在 `#[cfg(test)]` 下把查询退化成 `None`（开发机插什么键盘不该决定断言红绿，与 §16/§21 同一类卫生）；非 US 的行为由 `fold_with` 那组测试喂**假布局**覆盖。`mo-platform` 侧按 KLID 显式 `ActivateKeyboardLayout` 后实测 US 与德语两列答案，装不上布局的机器跳过并 `eprintln!`。
* **实机验证撞到的两件事**（记下来，免得下一个人重踩）：
    * gpui 算字符走 `ToUnicode(vk, lParam 高字节的扫描码, state)`，它只看**当前线程**的布局。而 `SetForegroundWindow` 一激活窗口，Windows 就把该线程的布局打回这个窗口登记的输入语言（本机是 `0x0804` 中文）；`WM_INPUTLANGCHANGEREQUEST` 想切德语（`00000407`）也不落地——不在用户「输入语言列表」里的布局，Shell 不给切。所以**没能让 Mo 的线程真变成德语键盘**，GUI 上的德语端到端这一轮没验成。
    * 于是改成验**同一条 API 链**：`to_unicode_and_unshifted_key_agree_on_the_same_layout` 在进程内激活德语后，照抄 gpui 的调用形状（`state[VK_SHIFT]=0x80`、8 个 u16 的缓冲、flags `0x5`），`ToUnicode(VK_OEM_102, 0x56, Shift)` 交出 `>`，`unshifted_key('>')` 答回 `<`；同一个键在 US 上交出 `|`、折成 `\`。真实德语用户的 Mo 线程从进程起来就是德语，走的是这一条。
    * GUI 侧只验了**不回归**：US 线程上 `Ctrl+Shift+.` → Mo 收到 `>` → 折成 `.` → 隐藏文件照常切换（第二轮再按一次收回）。

## 待办（还没做，别当成已完成）

* 全篇（§1~§13）都是**落地之后补记**的，当时第一手的调试感（比如 `$I` 扫了几千条才反查通、`explorer` 退出码是怎么误报的）已丢了一些；§14 之后是当轮写的。
* Windows 的 `windows_pdf` 别名是权宜：若哪天要把 Shell 那套也升到 0.62，一并把两个版本收成一个，别再叠第三份。
* **§22 在 macOS 上还是 US 表**（gpui 不给虚拟键码）；Linux 侧连 §15 的实测都还没做（gpui 的 Linux 后端怎么报 Shift + 符号未验），只保证单测三平台跑得过。
* **macOS 的文件剪贴板「出去」没做**（§20 只写了 Windows；`supports_file_clipboard()` 在 mac 上为假）。NSPasteboard 写 `NSURL` 数组是公开 API，工作量不大，但得在 mac 上验，不能空写。
* **拖放**（应用内、以及与资源管理器之间）还没动，是 §19/§20 之后自然的一段：`DROPFILES` 那套字节形状已经现成。
* §21 的缓存隔离只收了 mo-ui / mo-platform 这条路；mo-app / mo-operations 的十余个集成测试仍在写真实 `search.sqlite`。
* 地址栏不认 `/`：`D:/tmp-clip/moside` 与 `D:\tmp-clip\moside` 两种写法敲进去都停在 `D:` 根（2026-09-26 实测，未查因）。
* 测试留下的临时回收站 `mo-trash-<pid>-<seq>` 在 TEMP 里没人删（§21 那个 `remove_dir_all` 只挡 pid 复用带来的读脏，不解决堆积）。

