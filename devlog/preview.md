# 缩略图与快速预览

预览降采样的编码选择、窗口的「先开后到」、入场动画。涉及 `mo-thumbnails`、
`mo-ui::preview`、`mo-ui::app`。

## 1. 预览降采样别存 PNG：一张照片 1.4s（「按空格要等一下」的根因，2026-09-22）

* 现象：按空格打开图片预览，窗口要等 0.4–1.4s 才出现；同一张图再按一次几乎瞬间。
* 量化（`cargo run` = debug 构建，6000×4000 JPEG 降采样到长边 2560）：

  | 环节 | debug | release |
  |---|---|---|
  | 读文件头 | 0.4 ms | 0.3 ms |
  | 解码整图 | 56 ms | 47 ms |
  | 缩放 → 2560 | 44 ms | 83 ms |
  | **编码 PNG** | **1281 ms** | 20 ms |
  | 产物 | 5.0 MB | 5.0 MB |
  | 冷合计 | **1.43 s** | 152 ms |
  | 热（命中磁盘缓存） | 0.4 ms | 0.3 ms |

* 根因：预览缓存里存的是**照片**，却用 PNG 无损压缩；而 `png` / `miniz_oxide` 不在
  `[profile.dev.package]` 的 opt-level=3 名单里（那里只有 `image`、`zune-jpeg`），
  debug 下光编码就 1.28s，占冷路径 90%。release 下同一张只要 20ms——**这是 debug
  体感问题，不是算法问题**，所以别急着怀疑缩放算法。
* 修法（`mo_thumbnails::Encoded`）：产物格式按源图分流——只有 **JPEG 源**转 JPEG q85
  （debug 0.32s / 1.1MB），其余（PNG / GIF / WebP…，都可能带 alpha）留在 PNG。
  判据故意保守：判错的方向只能是「本来能转 JPEG 的留在 PNG」（慢一点），
  绝不能反过来（把带透明的图压成黑底）。
  * ⚠️ 缓存文件名必须带**扩展名**：上层是 gpui 的 `img(path)`，它**按扩展名**挑解码器
    （`gpui::Img::extensions()`），后缀与真实编码对不上会直接解码失败。
  * ⚠️ 缓存键（内存索引与磁盘路径）都要带上编码，否则同尺寸的两种产物会互相命中。
  * ⚠️ 写完先 `flush` 再 `rename`：原来是直接写 `File`，换成 `BufWriter` 提速之后，
    不 flush 就 rename 会「成功地」留下一个截断的产物——而且下次还会命中它。
* 没走的那条路：给 `png` / `miniz_oxide` 加 opt-level=3 能把 debug 的 PNG 编码拉到
  ~30ms 级，但它不改「产物 5MB/张」的磁盘占用，release 下也没有收益。

## 2. 窗口别等降采样：先切内容、后到图（同一事故的另一半）

* `open_quick_look` 原来是 `await prepared_preview(...)` **之后**才 `show_preview`——
  于是上面那 1.4s 里**窗口根本不出现**，体感是「按了空格没反应」，而不是「窗口开了、
  图在转」。延迟是两件事，修法也要两条。
* 修法：`app.preview()`（只读元信息，同步且快）→ 立刻 `show_preview`（`kind == Image`
  时把 `image` 留空，`PreviewWindow` 见到空图位就画「载入预览…」占位）→ 后台
  `spawn_blocking(preview_image_scaled)` → `set_preview_image` 换上。
* ⚠️ **代际校验**（`RootView::preview_seq`，每次 `show_preview` 递增）：降采样回来时
  用户可能已经翻页，代际对不上必须丢弃，否则会把上一张的图贴到当前预览上。
* ⚠️ `preview_image_scaled` 返回 `None` 是「用原图」（长边本来就没超上限 / 不是图片 /
  降采样失败），**必须回落到原图路径**，否则窗口会永远停在占位上。
* ⚠️ 开窗改成**同步**（原来包在 `cx.spawn` 里、要下一拍才真的建）：小图的降采样 0.3ms
  就返回，异步开窗时那种情况会「贴图时窗口还不存在」→ 图永远贴不上、窗口停在占位。
  `Context` 通过 `Deref` 拿到 `App`，所以 `cx.open_window` 在 `&mut Context<T>` 上可用。
* ⚠️ **翻页要走同一套两拍**（同日补修）：`preview_step` 当时还是老路径——先
  `await prepared_preview(...)` 再 `show_preview`——于是按方向键后窗口里**一直挂着上一张
  的图**，直到新图就绪才跳变。用户报的「切换有延迟」就是这个。现在按空格 / 方向键翻页 /
  搜索面板回车三个入口统一走 `show_preview_twopass`（`prepared_preview` 已被它取代）。
  **判据：只要换了预览对象，就立刻把 `image` 摘掉显示占位，别等第二拍。**
* ⚠️ 摘 `image` 这步抽成了纯函数 `split_preview_for_two_pass`，并配一条单测
  （`app.rs::tests::only_images_are_split_into_two_passes`）。它守的是「占位还出不出得来」：
  将来若有人觉得这一步多余（「clone 一个路径不是更直接」），翻页就会静默退回
  「挂着上一张」的旧行为，而这条测试会红。

## 3. 入场动画：gpui 这版没有 element 级 scale

* 目标：访达快速预览那种「从缩略图位置放大展开」的展开感。
* 可用能力：GPUI 有完整的 `Animation` / `AnimationExt`（`with_animation`），
  **自动尊重系统「减弱动态效果」**，按 `ElementId` 记住进度、oneshot 只播一次
  （所以换内容 / 翻页不会重播）。
* 缺的能力：**没有 element 级 `scale`**（`gpui-pre-0.3.5` 的 `styled.rs` 里只有
  `opacity`；`Transformation` 只存在于 svg 元素）。
* 绕法：用**相对尺寸**驱动「缩放」——图片容器 `w/h` 从 `relative(0.86)` 动到
  `relative(1.0)`，同时 `opacity(0 → 1)`，200ms
  `Animation::new(...).with_easing(ease_out_quint())`。
  ⚠️ `ease_out_quint` 是**工厂函数**（返回闭包），要写 `ease_out_quint()`。
  图片是 Contain 适配容器，容器从小变大，观感就是图片从中心长出来。
* 只对**图片容器**做动画，外层（含 `theme::surface()` 背景）不动——否则动画期间
  窗口边缘会露出底色。
* 为什么不真做窗口动画：gpui 的平台 trait 只有 `resize`（改内容尺寸），没有移动窗口的
  公开 API，也拿不到内部 NSWindow 句柄；`WindowBounds::Windowed(Bounds)` 只影响**开窗
  那一刻**的位置。真要「从缩略图飞过来」，得给 mo-platform 加「按窗口标题找 NSWindow
  再 `setFrame:display:animate:`」的平台 hack，跨平台各写一套，且动画期间 gpui 会按新
  尺寸重排内容。**暂不做**（结论记录，不是待办）。

## 4. PDF 预览：系统自带渲染器就够，别引第三方（2026-09-22）

空格键预览一个 PDF 只能看到「二进制文件，N 字节」——`PreviewKind` 压根没有 PDF。

- **渲染用 CoreGraphics 的 `CGPDFDocument`**（`mo_platform::pdf_page_raster`）：
  系统自带、零新依赖，实测首页（612×792pt）渲染 + 编码 19ms。不用 pdfium / mupdf——
  那两个都要带二进制，而 macOS 的 CG 渲染质量就是「预览」要的水平。
- 与 `file_icon_raster` 的关键区别：**不需要主线程**。`CGPDFDocument` 是纯 C、不碰
  AppKit，可以放心放 blocking 池（首页几毫秒到几十毫秒，大页面更久）。
- **先铺白底再画页面**：PDF 只有笔画、页面本身透明，不铺底透明像素在 PNG 里看着
  是黑的。页面比 `max_edge` 小时**不放大**（放大只会糊）。
- **类型判定在 mo-preview，渲染在 mo-app**：`PreviewKind::Pdf` 的 `image` 恒为
  `None`——`Image` 的语义是「原路径即可 `img()` 加载」，PDF 必须先过渲染。UI 走既有的
  两段式（`show_preview_twopass`）：先占位「正在渲染首页…」，后台
  `AppState::preview_pdf_page`（渲染 + `unpremultiply` + 编码 PNG + 原子写缓存），
  图到后 `set_preview_image`；渲染失败把占位换成一句解释，别让窗口永远停在
  「正在渲染…」。
- 缓存键 = 路径 hash + **mtime**：PDF 被改过自动失效，只按路径做键会一直显示旧首页。
- 验证：一次性 example 探针（跑完即删）渲染真实 PDF 出图正确；mo-preview 单测钉
  「pdf 被识别但不带 image」。真机还要看一眼：两段式占位 → 图淡入的衔接。
- **Windows 端**改走系统自带的 WinRT `Windows.Data.Pdf`（不随二进制带 pdfium.dll），
  外加一条只在 Windows 显形的坑：**上面这个两段式对 PDF 从来没启动过**（2026-09-26 修）
  ——见 [windows-port.md](windows-port.md) §9、§10。

## 附：系统文件图标（列表行）

`AppState::file_icon` 那条链路的性能坑（原始尺寸 NSImage 转 PNG，40 张 10.77s）
记在 [macos-platform.md](macos-platform.md) §14，与这里的「渲染路径上不许有 AppKit
调用 + 位图编码 + 写盘」是同一条纪律。
