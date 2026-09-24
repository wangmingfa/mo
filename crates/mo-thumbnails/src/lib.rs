//! mo-thumbnails：缩略图生成与缓存。
//!
//! 缩略图是典型的「昂贵且可缓存」任务，因此这一层刻意做成**无状态、可阻塞、可并发**：
//!
//! ```text
//! Entry（图片） → ThumbnailCache::get_or_create()
//!                      │
//!                      ├─ 命中磁盘缓存 → 直接返回路径
//!                      │
//!                      └─ 未命中 → 解码 → 缩放 → 写 PNG（原子落盘）
//! ```
//!
//! 生成过程会阻塞线程（解码是 CPU 密集），所以调用方应放到 blocking 池里执行，
//! 由 `mo-app::ThumbnailScheduler` 负责限流与优先级。
//!
//! 这里不依赖任何 UI 框架：对外只暴露「磁盘上的 PNG 路径」，
//! 由 UI 层自行解码成自己的图片类型。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::ImageFormat;
use mo_core::FileId;

/// 缩略图生成失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    #[error("不支持的图片类型：{0}")]
    Unsupported(String),
    #[error("解码失败：{0}")]
    Decode(String),
    #[error("IO 失败：{0}")]
    Io(String),
}

/// 缩略图默认边长（像素）。
pub const DEFAULT_SIZE: u32 = 128;

/// 预览图的长边上限（像素）。
///
/// 快速预览会把超过这个尺寸的图片**先降采样**再交给 UI：一张 7680×4320 的
/// JPEG 全解码约 130MB RGBA，而屏幕上通常只显示很小一块——原图既拖慢首次
/// 加载，也让 GPU 端按原尺寸建了张用不上的大纹理。
pub const PREVIEW_MAX_EDGE: u32 = 2560;

/// 缓存产物的编码格式。
///
/// 缩略图（128px）一直用 PNG；**预览**（长边 2560）以前也用 PNG，可那是把一整张
/// 24 位照片做无损压缩——实测一张 6000×4000 的 JPEG 降采样到 2560 后，debug 构建
/// 下**光 PNG 编码就要 1.28s、产物 5MB**（占冷路径 90%）。照片本来就没有 alpha，
/// 改存 JPEG q85：debug 下 0.32s、产物 1.1MB。
///
/// 带 alpha 的源（PNG / GIF / WebP…）必须留在 PNG，否则透明区会被压成黑底——
/// 判据见 [`encode_for_preview`]，判错的方向永远是「留在慢格式」这一侧。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoded {
    Png,
    Jpeg,
}

impl Encoded {
    /// 产物文件名的扩展名。
    ///
    /// ⚠️ 不只是好看：上层用 gpui 的 `img(path)` 加载，它**按扩展名**挑解码器
    /// （`gpui::Img::extensions()` 那张表），扩展名与真实编码对不上就解码失败。
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

/// 预览产物该用哪种编码：只有 JPEG 源转 JPEG，其余一律 PNG。
///
/// 判据故意保守——判错的方向只能是「本来能转 JPEG 的留在了 PNG」（慢一点），
/// 绝不能反过来（把带透明的图压成黑底）。
fn encode_for_preview(format: Option<ImageFormat>) -> Encoded {
    match format {
        Some(ImageFormat::Jpeg) => Encoded::Jpeg,
        _ => Encoded::Png,
    }
}

/// 预览 JPEG 的质量。85 是照片的常用档：肉眼与无损基本无差，体积约为
/// 「降采样后 PNG」的 1/5。
const PREVIEW_JPEG_QUALITY: u8 = 85;

/// 缩略图缓存。
///
/// * **磁盘缓存**：`<用户缓存目录>/mo/thumbs/<size>/<volume>-<id>.png`，跨会话复用；
///   以 `FileId`（inode）而非路径命名（键取 [`FileId::cache_key`]，见
///   [`ThumbnailCache::cached_path`]），所以文件改名 / 移动后缓存依然命中。
/// * **内存索引**：记录「已确认存在」的缓存路径，避免每次都 `stat` 一次磁盘。
pub struct ThumbnailCache {
    root: PathBuf,
    known: Mutex<HashMap<(FileId, u32, Encoded), PathBuf>>,
}

impl ThumbnailCache {
    /// 使用默认缓存目录（`<用户缓存目录>/mo/thumbs`）。
    pub fn new() -> Self {
        Self::with_root(default_thumb_root())
    }

    /// 指定缓存根目录（测试时常用临时目录）。
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            root,
            known: Mutex::new(HashMap::new()),
        }
    }

    /// 缓存根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 某个条目、某个尺寸对应的缓存文件路径。
    ///
    /// 文件名取 [`FileId::cache_key`] 而**不是** `FileId` 的 `Display`：后者是
    /// `volume:id`，冒号在 Windows 上是文件名非法字符，会把整棵缩略图缓存写废
    /// （`CreateFile` 回 os error 87）。见 `mo-core::file_id::tests`。
    pub fn cached_path_with(&self, id: &FileId, size: u32, enc: Encoded) -> PathBuf {
        self.root
            .join(size.to_string())
            .join(format!("{}.{}", id.cache_key(), enc.ext()))
    }

    /// 同上，按缩略图的老约定（PNG）。
    pub fn cached_path(&self, id: &FileId, size: u32) -> PathBuf {
        self.cached_path_with(id, size, Encoded::Png)
    }

    /// 命中缓存则直接返回路径（不解码）。
    pub fn peek_with(&self, id: &FileId, size: u32, enc: Encoded) -> Option<PathBuf> {
        if let Some(p) = self
            .known
            .lock()
            .ok()
            .and_then(|k| k.get(&(*id, size, enc)).cloned())
        {
            return Some(p);
        }
        let p = self.cached_path_with(id, size, enc);
        if p.exists() {
            if let Ok(mut k) = self.known.lock() {
                k.insert((*id, size, enc), p.clone());
            }
            Some(p)
        } else {
            None
        }
    }

    /// 同上，按缩略图的老约定（PNG）。
    pub fn peek(&self, id: &FileId, size: u32) -> Option<PathBuf> {
        self.peek_with(id, size, Encoded::Png)
    }

    /// 取缩略图：命中缓存直接返回，否则解码生成。
    ///
    /// **阻塞操作**，应放在 blocking 线程执行。
    pub fn get_or_create_with(
        &self,
        id: &FileId,
        src: &Path,
        size: u32,
        enc: Encoded,
    ) -> Result<PathBuf, ThumbnailError> {
        if let Some(p) = self.peek_with(id, size, enc) {
            return Ok(p);
        }
        let out = generate_to_with(src, &self.cached_path_with(id, size, enc), size, enc)?;
        if let Ok(mut k) = self.known.lock() {
            k.insert((*id, size, enc), out.clone());
        }
        Ok(out)
    }

    /// 同上，按缩略图的老约定（PNG）。
    pub fn get_or_create(
        &self,
        id: &FileId,
        src: &Path,
        size: u32,
    ) -> Result<PathBuf, ThumbnailError> {
        self.get_or_create_with(id, src, size, Encoded::Png)
    }

    /// 清空磁盘与内存缓存。
    pub fn clear(&self) -> std::io::Result<()> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)?;
        }
        if let Ok(mut k) = self.known.lock() {
            k.clear();
        }
        Ok(())
    }
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 解码 `src` 并生成边长为 `size` 的缩略图，写入 `dst`。
///
/// 先写临时文件再 `rename`，避免进程被杀时留下半张损坏的 PNG。
pub fn generate_to_with(
    src: &Path,
    dst: &Path,
    size: u32,
    enc: Encoded,
) -> Result<PathBuf, ThumbnailError> {
    let reader = image::ImageReader::open(src)
        .map_err(|e| ThumbnailError::Io(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| ThumbnailError::Io(e.to_string()))?;

    let format = reader.format();
    if !matches!(
        format,
        Some(ImageFormat::Jpeg)
            | Some(ImageFormat::Png)
            | Some(ImageFormat::Gif)
            | Some(ImageFormat::WebP)
            | Some(ImageFormat::Bmp)
            | Some(ImageFormat::Ico)
    ) {
        return Err(ThumbnailError::Unsupported(src.display().to_string()));
    }

    let img = reader
        .decode()
        .map_err(|e| ThumbnailError::Decode(e.to_string()))?;
    let thumb = img.thumbnail(size, size);

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ThumbnailError::Io(e.to_string()))?;
    }
    let tmp = dst.with_extension("tmp");
    let mut out = std::io::BufWriter::new(
        std::fs::File::create(&tmp).map_err(|e| ThumbnailError::Io(e.to_string()))?,
    );
    match enc {
        Encoded::Png => thumb
            .write_to(&mut out, ImageFormat::Png)
            .map_err(|e| ThumbnailError::Io(e.to_string()))?,
        Encoded::Jpeg => {
            let mut e =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, PREVIEW_JPEG_QUALITY);
            e.encode_image(&thumb)
                .map_err(|e| ThumbnailError::Io(e.to_string()))?;
        }
    }
    // ⚠️ 必须先 flush 再 rename：换成 `BufWriter` 之后，没落盘的字节还躺在缓冲区里，
    // 此时 rename 会「成功地」留下一个截断的产物（而且下次还会命中它）。
    out.flush().map_err(|e| ThumbnailError::Io(e.to_string()))?;
    drop(out);
    std::fs::rename(&tmp, dst).map_err(|e| ThumbnailError::Io(e.to_string()))?;
    Ok(dst.to_path_buf())
}

/// 同上，按缩略图的老约定（PNG）。
pub fn generate_to(src: &Path, dst: &Path, size: u32) -> Result<PathBuf, ThumbnailError> {
    generate_to_with(src, dst, size, Encoded::Png)
}

/// 把一张 RGBA8 位图编码成 PNG 字节（直通 alpha）。
///
/// 通用工具：现在给 PDF 首页预览那条链路用（平台层交出的预乘像素先
/// [`unpremultiply_rgba`]，再由这里编码落盘）。
///
/// `rgba` 长度必须正好是 `width * height * 4`；长度不符或编码失败都返回 `None`
/// （调用方按「这张图没生成」处理即可，不必区分）。
pub fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if rgba.len() != (width as usize) * (height as usize) * 4 {
        return None;
    }
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// 磁盘上的 PNG → 解码好的内存位图（BGRA、直通 alpha）。
///
/// 给「列表直接吃内存位图」的链路用：UI 的 `img(path)` 要异步读盘解码，位图没到
/// 之前那一格什么都不画（切目录时的图标闪烁正是这么来的）；先在这里把缓存 PNG
/// 解成 [`Bitmap`]，UI 侧就能 `ImageSource::Render` 同步上屏。
///
/// **阻塞**（读盘 + 解码），必须在 blocking 池调用。任何失败返回 `None`。
pub fn decode_bitmap(png: &Path) -> Option<mo_core::Bitmap> {
    let img = image::ImageReader::open(png)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let (w, h) = (img.width(), img.height());
    mo_core::Bitmap::from_rgba(w, h, img.into_rgba8().into_raw())
}

/// 把**预乘 alpha** 的 RGBA 还原成直通 alpha（原地）。
///
/// AppKit 把图画进位图时总会预乘（RGB 已经被 alpha 缩过一遍），而 PNG 存的是直通
/// alpha：不还原的话，抗锯齿边缘与半透明区会整体发暗。
///
/// 还原式 `c = c * 255 / a`（四舍五入）；`a == 0` 的全透明像素保持全 0，不参与除法。
pub fn unpremultiply_rgba(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a == 0 || a == 255 {
            continue;
        }
        for c in &mut px[..3] {
            *c = (((*c as u32) * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

/// 默认缩略图缓存目录：`dirs::cache_dir()/mo/thumbs`。
fn default_thumb_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("mo")
        .join("thumbs")
}

/// 默认预览图缓存目录：`dirs::cache_dir()/mo/preview`。
///
/// 与缩略图分开存放：两者尺寸差 20 倍，混在一个目录里不利于整体清理。
fn default_preview_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("mo")
        .join("preview")
}

/// 为**快速预览**准备一张降采样副本，落在 `root` 下；返回 `Some` 时应加载
/// 这个路径而不是原图。
///
/// * 原图长边已在 `max_edge` 以内 → `None`（用原图，不必多一份磁盘副本）；
/// * 命中缓存 → 直接返回缓存路径（**不解码**）；
/// * 否则生成长边不超过 `max_edge` 的 PNG（`generate_to` 内部先写临时文件
///   再 `rename`，进程被杀不会留下半张图）。
///
/// **阻塞操作**（含解码），必须在 blocking 池调用。任何失败都返回 `None`——
/// 降采样属于优化，失败必须让调用方安静地回退到原图，而不是让预览失败。
pub fn preview_scaled_in(root: &Path, src: &Path, max_edge: u32) -> Option<PathBuf> {
    // 只读文件头拿两样东西：尺寸（值不值得降采样）+ 格式（产物用哪种编码）。
    // 这里不能整图解出来，否则就失去了降采样的意义。
    let reader = image::ImageReader::open(src)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let enc = encode_for_preview(reader.format());
    let (w, h) = reader.into_dimensions().ok()?;
    if w.max(h) <= max_edge {
        return None;
    }
    let cache = ThumbnailCache::with_root(root.to_path_buf());
    cache
        .get_or_create_with(&FileId::synthetic(src), src, max_edge, enc)
        .ok()
}

/// 同上，使用默认预览缓存目录 `<用户缓存目录>/mo/preview`。
pub fn preview_scaled(src: &Path, max_edge: u32) -> Option<PathBuf> {
    preview_scaled_in(&default_preview_root(), src, max_edge)
}
