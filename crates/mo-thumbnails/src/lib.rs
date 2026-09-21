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

/// 缩略图缓存。
///
/// * **磁盘缓存**：`<用户缓存目录>/mo/thumbs/<size>/<file-id>.png`，跨会话复用；
///   以 `FileId`（inode）而非路径命名，所以文件改名 / 移动后缓存依然命中。
/// * **内存索引**：记录「已确认存在」的缓存路径，避免每次都 `stat` 一次磁盘。
pub struct ThumbnailCache {
    root: PathBuf,
    known: Mutex<HashMap<(FileId, u32), PathBuf>>,
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
    pub fn cached_path(&self, id: &FileId, size: u32) -> PathBuf {
        self.root.join(size.to_string()).join(format!("{}.png", id))
    }

    /// 命中缓存则直接返回路径（不解码）。
    pub fn peek(&self, id: &FileId, size: u32) -> Option<PathBuf> {
        if let Some(p) = self
            .known
            .lock()
            .ok()
            .and_then(|k| k.get(&(*id, size)).cloned())
        {
            return Some(p);
        }
        let p = self.cached_path(id, size);
        if p.exists() {
            if let Ok(mut k) = self.known.lock() {
                k.insert((*id, size), p.clone());
            }
            Some(p)
        } else {
            None
        }
    }

    /// 取缩略图：命中缓存直接返回，否则解码生成。
    ///
    /// **阻塞操作**，应放在 blocking 线程执行。
    pub fn get_or_create(
        &self,
        id: &FileId,
        src: &Path,
        size: u32,
    ) -> Result<PathBuf, ThumbnailError> {
        if let Some(p) = self.peek(id, size) {
            return Ok(p);
        }
        let out = generate_to(src, &self.cached_path(id, size), size)?;
        if let Ok(mut k) = self.known.lock() {
            k.insert((*id, size), out.clone());
        }
        Ok(out)
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
pub fn generate_to(src: &Path, dst: &Path, size: u32) -> Result<PathBuf, ThumbnailError> {
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
    let mut file = std::fs::File::create(&tmp).map_err(|e| ThumbnailError::Io(e.to_string()))?;
    thumb
        .write_to(&mut file, ImageFormat::Png)
        .map_err(|e| ThumbnailError::Io(e.to_string()))?;
    drop(file);
    std::fs::rename(&tmp, dst).map_err(|e| ThumbnailError::Io(e.to_string()))?;
    Ok(dst.to_path_buf())
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
    // 只读文件头拿尺寸：这里不能整图解出来，否则就失去了降采样的意义。
    let (w, h) = image::image_dimensions(src).ok()?;
    if w.max(h) <= max_edge {
        return None;
    }
    let cache = ThumbnailCache::with_root(root.to_path_buf());
    cache
        .get_or_create(&FileId::synthetic(src), src, max_edge)
        .ok()
}

/// 同上，使用默认预览缓存目录 `<用户缓存目录>/mo/preview`。
pub fn preview_scaled(src: &Path, max_edge: u32) -> Option<PathBuf> {
    preview_scaled_in(&default_preview_root(), src, max_edge)
}
