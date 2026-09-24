# 图标 / 缩略图链路改走内存位图（零空窗）

**日期**：2026-09-24 · 缘起：用户录屏反馈「切换目录时文件图标闪烁」。

## 根因（先记住结论）

gpui 的 `img(path)` 在 image cache 未命中时**什么都不画**：读盘 + 解码走 asset
loader 异步，命中前每帧都是空槽（`with_loading` 占位还要等 200ms 才出现）。
而图标泵产出的每个 PNG 都是**新临时文件路径** → 必 miss → 切目录时「文字先出、
图标晚 1-3 帧」；缩略图 Loaded 后 img 源从类型图标换成缩略图路径 → 又一次
miss → 又空一帧（录屏里「图标消失再变形」）。

## 方案 C：全链改内存位图（`ImageSource::Render` 同步上屏）

```
图标：mo-platform（主线程取光栅，预乘 RGBA）
      → mo-app::icon::icon_bitmap（后台：unpremultiply + RGBA→BGRA）
      → IconCache 存 Arc<mo_core::Bitmap>
      → mo-ui::bitmap::image_source（按 Bitmap::id 缓存，首次包成 RenderImage）
      → img(ImageSource::Render(..))  ← 同步，永不空窗

缩略图：mo-thumbnails 磁盘缓存（保留，跨会话复用）
      → ThumbnailScheduler 泵在同一趟 blocking 里 decode_bitmap(PNG) → Arc<Bitmap>
      → ThumbnailState::Loaded(Arc<Bitmap>)   ← 原来是 Loaded(PathBuf)
      → 同上 Render 同步上屏（换源闪烁一并消失）
```

关键事实（gpui-pre-0.3.6）：
- `ImageSource::Render(Arc<RenderImage>)` 的 `use_data` **同步**返回 `Some(Ok(..))`，
  完全绕开 image cache 与异步加载。
- `RenderImage` 帧格式 = **BGRA、直通 alpha**（它的各解码路径只做 RGBA→BGRA
  swap，不预乘）——所以 `Bitmap::from_rgba` 里做 swap，预乘来源先过
  `mo_thumbnails::unpremultiply_rgba`。

## 分层（谁放什么）

- **mo-core** `bitmap.rs`：`Bitmap { id, width, height, bgra }`，纯数据。
  `id` 由进程级发号器发出、永不复用——UI 侧转换缓存的键。
- **mo-thumbnails**：`decode_bitmap(png) -> Option<Bitmap>`；磁盘缓存与
  `encode_rgba_png`（PDF 预览链路）原样保留。
- **mo-app**：`IconCache` 值 `PathBuf → Arc<Bitmap>`（含 `folder_fallback`）；
  泵不再写 `temp_dir()/mo-icons`，`icon_file_hash` 删除。字节账
  `bytes = Σ by_path 各条位图`（by_type 与首条 by_path 共享分配不重复记），
  条数 4000 + 字节 128 MiB 双封顶，超限整清。
- **mo-ui** `bitmap.rs`：`image_source(&Arc<Bitmap>) -> Option<ImageSource>`，
  按 id 缓存（2048 封顶整清）——热路径命中时只有拿锁查表 + `Arc` clone，
  **零分配**；首次转换拷一份像素进 `ImageBuffer`（6–65 KB，一次性）。
  `file_item` / `columns` / `grid(含画廊)` 三处渲染点全走它。

## 行为保持不变的部分

- 泵节拍（`ICON_BUDGET_MS` 配额、重试退避、prefetch 优先级）原样——图标仍是
  「晚一两帧浮现」，只是浮现那一刻不再有前置空帧。
- 目录行 folder_fallback、远程页返回 None 退内置 SVG、`Loading` 态空槽（生产
  从不置位）语义原样。
- 预览窗格（`preview.rs`）仍走 `img(path)`：单图异步可接受，不在本次范围。

## 测试

- mo-core：swap 正确性 / 长度拒绝 / id 唯一 / Debug 紧凑。
- mo-app icon.rs：原 14 条全保留（值改位图句柄），新增字节账（替换减账、整清归零）。
- mo-ui bitmap.rs：同 id 命中同一 `RenderImage`、不同位图不共享、空尺寸拒绝。
- grid.rs 两条视觉测试改用内存位图（不再落盘真 PNG）。
- 质量门：fmt 干净 / `clippy --workspace --all-targets --all-features -D warnings`
  0 警告 / 全 workspace 测试全绿。

## 踩坑

- clippy：`chunks_exact_mut(4)` 常量块宽要写 `as_chunks_mut::<4>()`；
  `.and_then(|b| f(b))` 报 redundant closure，直接传函数名。
- `ImageBuffer::from_raw(0, 0, vec![])` 会「成功」——`image_source` 入口显式
  拒绝 0 尺寸，别指望 image crate 帮你挡。
