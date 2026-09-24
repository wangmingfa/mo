//! 解码好的内存位图（BGRA、直通 alpha）。
//!
//! 领域层不依赖任何 UI 框架：这里只是**像素数据 + 尺寸 + 进程内唯一 id**。
//!
//! 存在的理由：UI 侧的 `img(path)` 在图片缓存未命中时**什么都不画**——它要异步
//! 读盘 + 解码，位图没到之前那一格是空的。切目录时一整屏图标「先文字后图标」
//! 的闪烁就是这么来的。把位图在后台备成内存数据（本类型），UI 只需一次性包成
//! 渲染器的图片类型（gpui `RenderImage`）即可**同步上屏**：零 IO、零空窗。

use std::sync::atomic::{AtomicU64, Ordering};

/// 位图编号发号器：从 1 起单调递增，进程内唯一。
///
/// id 一经发出永不复用——UI 侧的转换缓存拿它做键，位图本体被清掉后留下的
/// 旧条目也不会被新位图误命中。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 一张解码好的位图，像素排布为 **BGRA、直通 alpha**——gpui `RenderImage`
/// 帧缓冲的原生格式（它的各条解码路径都只做 RGBA→BGRA 交换，不预乘）。
#[derive(Clone)]
pub struct Bitmap {
    id: u64,
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

impl Bitmap {
    /// 由**直通 alpha** 的 RGBA 构造（R/B 就地换成 BGRA 排布）。
    ///
    /// `rgba.len()` 必须正好是 `width * height * 4`，不符返回 `None`。
    /// 注意输入必须是直通 alpha：AppKit 这类**预乘**来源要先过
    /// `mo_thumbnails::unpremultiply_rgba`，否则抗锯齿边缘发暗。
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Bitmap> {
        if rgba.len() != width as usize * height as usize * 4 {
            return None;
        }
        let mut bgra = rgba;
        for px in bgra.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        Some(Bitmap {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            bgra,
        })
    }

    /// 进程内唯一编号（UI 侧转换缓存的键）。
    pub fn id(&self) -> u64 {
        self.id
    }

    /// 宽（像素）。
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 高（像素）。
    pub fn height(&self) -> u32 {
        self.height
    }

    /// BGRA 像素，长度恰为 `width * height * 4`。
    pub fn bgra(&self) -> &[u8] {
        &self.bgra
    }
}

impl PartialEq for Bitmap {
    /// 同一张位图才算相等——按 id，不逐字节比（位图可达几十 KB，且等尺寸
    /// 不同内容的两张图本来就该不相等）。
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Bitmap {}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bitmap")
            .field("id", &self.id)
            .field("size", &(self.width, self.height))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RGBA 进 BGRA 出：R 与 B 换位、G 与 A 原位；id 单调不重复。
    #[test]
    fn from_rgba_swaps_to_bgra_and_ids_are_unique() {
        let a = Bitmap::from_rgba(1, 1, vec![0x11, 0x22, 0x33, 0x44]).expect("1×1 合法");
        assert_eq!(a.bgra(), &[0x33, 0x22, 0x11, 0x44]);
        let b = Bitmap::from_rgba(1, 1, vec![0x11, 0x22, 0x33, 0x44]).expect("1×1 合法");
        assert_ne!(a.id(), b.id(), "每张位图都要有独立 id（转换缓存的键）");
        assert_ne!(a, b, "按 id 判等：内容相同也是两张图");
    }

    /// 长度不符必须拒绝——这是 UI 侧安全构造 `ImageBuffer` 的前提。
    #[test]
    fn from_rgba_rejects_bad_lengths() {
        assert!(Bitmap::from_rgba(2, 2, vec![0; 15]).is_none());
        assert!(Bitmap::from_rgba(2, 2, vec![0; 17]).is_none());
        assert!(Bitmap::from_rgba(0, 0, vec![0; 4]).is_none());
    }

    /// Debug 不打印像素（几十 KB 的位图刷屏），只报 id 与尺寸。
    #[test]
    fn debug_is_compact() {
        let a = Bitmap::from_rgba(3, 2, vec![0; 24]).expect("合法");
        let text = format!("{a:?}");
        assert!(text.contains("3"), "{text}");
        assert!(!text.contains("0, 0, 0"), "{text}");
    }
}
