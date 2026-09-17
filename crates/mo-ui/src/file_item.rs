use gpui_kit::*;
use mo_core::{Entry, MetadataState, ThumbnailState};

/// 单个文件 / 文件夹行的纯展示（不含交互；交互在 `file_list` 中处理）。
///
/// 缩略图来自 `mo-thumbnails` 生成的磁盘缓存；GPUI 可以直接从文件路径加载图片，
/// 因此这里只需把缓存路径交给 `img()`——领域层不必知道任何 UI 类型。
pub fn view(entry: &Entry, _selected: bool) -> impl IntoElement {
    let icon = match entry.kind {
        mo_core::EntryKind::Directory => "📁",
        mo_core::EntryKind::File => "📄",
        mo_core::EntryKind::Symlink => "🔗",
        mo_core::EntryKind::Other => "❓",
    };

    let mut row = div().flex_row().items_center().gap(px(6.0));

    row = match &entry.thumbnail {
        ThumbnailState::Loaded(path) => row.child(img(path.as_path()).w(px(20.0)).h(px(20.0))),
        ThumbnailState::Loading => row.child(text!("⏳".to_string())),
        _ => row.child(text!(icon.to_string())),
    };

    row = row.child(
        div()
            .flex_1()
            .overflow_hidden()
            .child(text!(entry.name.clone())),
    );

    // 元数据是后台加载的，尚未就绪时留空而不是显示 0，
    // 避免用户把「还没加载」误读成「文件是空的」。
    let size = match &entry.metadata {
        MetadataState::Loaded(m) => format_size(m.size),
        MetadataState::Loading => String::new(),
        MetadataState::Failed(_) => "—".to_string(),
    };
    row.child(div().w(px(80.0)).child(text!(size)))
}

/// 人类可读的文件大小。
pub fn format_size(size: u64) -> String {
    const KB: f64 = 1024.0;
    let s = size as f64;
    if s < KB {
        format!("{size} B")
    } else if s < KB * KB {
        format!("{:.1} KB", s / KB)
    } else if s < KB * KB * KB {
        format!("{:.1} MB", s / KB / KB)
    } else {
        format!("{:.1} GB", s / KB / KB / KB)
    }
}
