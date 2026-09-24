//! 内存位图 → gpui 渲染图（`RenderImage`）的一次性转换缓存。
//!
//! `img(path)` 在图片缓存未命中时**什么都不画**：它要异步读盘 + 解码，位图没到
//! 之前那一格是空的——切目录时「文字先出、图标晚到」的闪烁正是这么来的。
//! 位图现在以 [`mo_core::Bitmap`]（BGRA）形式由后台（图标泵 / 缩略图泵）备好，
//! 这里只在**首次见到**时把它包成 `RenderImage`，之后 `ImageSource::Render`
//! 同步上屏：零 IO、零空窗，热路径上只有拿锁查表 + `Arc` clone。
//!
//! 转换有一次性的小代价（拷一份像素进 `ImageBuffer`，6–65 KB），所以按
//! `Bitmap::id` 记账——每张位图只转一次；id 由发号器保证进程内唯一且不复用，
//! 位图本体被上游清掉后残留的条目也不会被新图误命中，封顶整清即可。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::{ImageSource, RenderImage};
use mo_core::Bitmap;

/// 转换缓存条数上限：超过整清。上游清掉位图后这里的条目只是占着
/// `RenderImage`（内含一份像素拷贝）不挨骂而已，整清的代价只是下一帧重建。
const CACHE_MAX: usize = 2048;

fn cache() -> &'static Mutex<HashMap<u64, Arc<RenderImage>>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Arc<RenderImage>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 位图 → `img()` 可直接吃的渲染源。
///
/// 返回 `None` 仅当位图尺寸与像素数对不上（上游不会给这种东西，兜个底）；
/// 调用方退回内置 SVG 即可。渲染路径每帧每行都调，命中时零分配。
pub(crate) fn image_source(bitmap: &Arc<Bitmap>) -> Option<ImageSource> {
    // 空尺寸的位图（上游不会给）直接拒绝：`ImageBuffer::from_raw` 对 0×0 会
    // 「成功」构造，但那样渲染侧拿到的是一张没有内容的帧。
    if bitmap.width() == 0 || bitmap.height() == 0 {
        return None;
    }
    let mut map = cache().lock().ok()?;
    if let Some(hit) = map.get(&bitmap.id()) {
        return Some(ImageSource::Render(hit.clone()));
    }
    let frame = image::Frame::new(image::ImageBuffer::from_raw(
        bitmap.width(),
        bitmap.height(),
        bitmap.bgra().to_vec(),
    )?);
    let rendered = Arc::new(RenderImage::new(vec![frame]));
    if map.len() >= CACHE_MAX {
        map.clear();
    }
    map.insert(bitmap.id(), rendered.clone());
    Some(ImageSource::Render(rendered))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bm(w: u32, h: u32) -> Arc<Bitmap> {
        Arc::new(Bitmap::from_rgba(w, h, vec![0u8; (w * h * 4) as usize]).expect("合法"))
    }

    /// 同一张位图反复转换拿到**同一个** `RenderImage`（热路径零拷贝的前提）；
    /// 不同位图各转各的。
    #[test]
    fn conversion_is_cached_per_bitmap_id() {
        let a = bm(1, 1);
        let first = image_source(&a).expect("合法位图该转得出");
        let second = image_source(&a).expect("合法位图该转得出");
        let ImageSource::Render(f) = &first else {
            panic!("该是 Render 源");
        };
        let ImageSource::Render(s) = &second else {
            panic!("该是 Render 源");
        };
        assert!(Arc::ptr_eq(f, s), "同一位图第二次转换该直接命中缓存");

        let b = bm(2, 2);
        let ImageSource::Render(other) = image_source(&b).expect("合法位图该转得出") else {
            panic!("该是 Render 源");
        };
        let ImageSource::Render(f) = first else {
            panic!()
        };
        assert!(!Arc::ptr_eq(&f, &other), "不同位图不能共享 RenderImage");
    }

    /// 尺寸与像素对不上的位图（上游不会给）要拒绝，而不是构造出残图。
    #[test]
    fn rejects_empty_bitmaps() {
        // Bitmap::from_rgba 已经在源头挡了，这里防御 0×0 的空图。
        let empty = Arc::new(Bitmap::from_rgba(0, 0, vec![]).expect("0×0 合法"));
        assert!(image_source(&empty).is_none(), "空图构不出 ImageBuffer");
    }
}
