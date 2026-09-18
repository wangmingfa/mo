use gpui_kit::*;
use mo_core::{Entry, EntryKind, MetadataState, ThumbnailState};
use std::time::SystemTime;

/// 右侧三列的固定宽度（表头与数据行共用，保证列对齐）。
pub const DATE_W: f32 = 150.0;
pub const SIZE_W: f32 = 80.0;
pub const KIND_W: f32 = 100.0;

/// 单个文件 / 文件夹行的纯展示（不含交互；交互在 `file_list` 中处理）。
///
/// Finder 列表视图式四列：名称（弹性）| 修改日期 | 大小 | 种类（均右对齐固定宽）。
/// 缩略图来自 `mo-thumbnails` 生成的磁盘缓存；GPUI 可以直接从文件路径加载图片，
/// 因此这里只需把缓存路径交给 `img()`——领域层不必知道任何 UI 类型。
pub fn view(entry: &Entry, selected: bool, tag: Option<String>) -> impl IntoElement {
    // 文件类型图标：统一 Lucide 风格、单色描边，颜色随选中态（蓝底用白字）。
    let icon_data = crate::icons::entry_icon(entry);
    let icon_color = if selected {
        crate::theme::selected_text()
    } else {
        crate::theme::text()
    };

    // ⚠️ `flex_1()` 不是装饰：`file_list` 的行容器是 `w_full()` + `items_center()`，
    // 里面的元素默认按**内容宽度**收缩。少了它，这一行就只有「图标 + 文件名 + 大小」
    // 那么宽，右侧的列会紧贴着文件名参差不齐，而不是对齐成固定列。
    //
    // `overflow_hidden()` 也不是装饰：flex item 的自动最小尺寸是 min-content
    // （也就是完整文件名的宽度），长文件名会把右侧列顶出行外。
    // 按 CSS 规则 hidden 会把自动最小尺寸降为 0，文件名的收缩交给下面那一层。
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_1()
        .overflow_hidden()
        .gap(px(8.0));

    // 图标槽位**定宽**：SVG 图标与图片缩略图的自然宽度不同，
    // 不定宽的话缩略图一加载文件名就会左右抖动。
    let icon_slot = |child: AnyElement| {
        div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(20.0))
            .h(px(20.0))
            .flex_shrink_0()
            .overflow_hidden()
            .child(child)
    };

    row = match &entry.thumbnail {
        ThumbnailState::Loaded(path) => row.child(icon_slot(
            img(path.as_path())
                .w(px(20.0))
                .h(px(20.0))
                .into_any_element(),
        )),
        ThumbnailState::Loading => row.child(icon_slot(text!("".to_string()).into_any_element())),
        _ => row.child(icon_slot(
            crate::icons::icon(icon_data, 16.0, icon_color).into_any_element(),
        )),
    };

    // 颜色标签（Finder 式）：有标签时文件名前显示一个色点。
    if let Some(color) = tag {
        row = row.child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .flex_shrink_0()
                .rounded(px(4.0))
                .bg(crate::dialogs::tag_color(&color)),
        );
    }

    row = row.child(
        div()
            .flex_1()
            // 文件名过长时省略号截断（`truncate` = overflow_hidden + nowrap + ellipsis），
            // 防止长名把右侧列顶出去。
            .truncate()
            .text_size(px(13.0))
            // 选中时整行是 Finder 蓝底，文字改白以保证对比度。
            .text_color(if selected {
                crate::theme::selected_text()
            } else {
                crate::theme::text()
            })
            .child(text!(entry.name.clone())),
    );

    // 右侧三列统一 12px：元数据列比文件名淡一档，视觉层次与 Finder 一致。
    let meta_color = if selected {
        crate::theme::selected_text()
    } else {
        crate::theme::muted()
    };
    let meta_cell = |w: f32, label: String, selector: &'static str| {
        div()
            .flex()
            .flex_row()
            .justify_end()
            .w(px(w))
            .flex_shrink_0()
            .text_size(px(12.0))
            .text_color(meta_color)
            .debug_selector(move || selector.to_string())
            .child(text!(label))
    };

    // 修改日期：后台加载未就绪时留空，加载失败显示 —。
    let date = match &entry.metadata {
        MetadataState::Loaded(m) => m.modified.map_or_else(String::new, format_modified),
        MetadataState::Loading => String::new(),
        MetadataState::Failed(_) => "—".to_string(),
    };

    // 元数据是后台加载的，尚未就绪时留空而不是显示 0，
    // 避免用户把「还没加载」误读成「文件是空的」。
    let size = match &entry.metadata {
        MetadataState::Loaded(m) => format_size(m.size),
        MetadataState::Loading => String::new(),
        MetadataState::Failed(_) => "—".to_string(),
    };

    row.child(meta_cell(DATE_W, date, "mo-date-cell"))
        .child(
            meta_cell(SIZE_W, size, "mo-size-cell"), // 测试用（release no-op）：本文件单测断言这一列的位置
        )
        .child(meta_cell(KIND_W, kind_label(entry), "mo-kind-cell"))
}

/// 本地时间格式化，Finder 中文样式：`2026年4月21日 10:33`。
fn format_modified(t: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format("%Y年%m月%d日 %H:%M").to_string()
}

/// 「种类」列文案：目录 / 链接固定，文件按扩展名归类（与图标分类一致）。
pub fn kind_label(entry: &Entry) -> String {
    match entry.kind {
        EntryKind::Directory => "文件夹".to_string(),
        EntryKind::Symlink => "符号链接".to_string(),
        EntryKind::Other => "文档".to_string(),
        EntryKind::File => kind_by_ext(&entry.name),
    }
}

/// 按扩展名映射种类文案。
fn kind_by_ext(name: &str) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        // 图像
        "png" => "PNG 图像",
        "jpg" | "jpeg" => "JPEG 图像",
        "gif" | "webp" | "bmp" | "svg" | "ico" => "图像",
        // 媒体
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "flv" => "视频",
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" => "音频",
        // 文档
        "pdf" => "PDF 文稿",
        "doc" | "docx" => "Word 文档",
        "xls" | "xlsx" | "csv" => "Excel 表格",
        "ppt" | "pptx" => "PPT 演示文稿",
        "md" => "Markdown 文档",
        "txt" | "log" | "rtf" => "文本文档",
        // 归档
        "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "bz2" | "xz" => "归档",
        // 代码
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "go" | "java" | "sh"
        | "json" | "toml" | "yaml" | "yml" | "html" | "css" => "源代码",
        // 可执行 / app
        "app" => "应用程序",
        "dmg" => "磁盘映像",
        "pkg" => "安装包",
        // 兜底：有扩展名 → 「EXT 文件」，无扩展名 → 「文档」
        "" => "文档",
        other => return format!("{} 文件", other.to_uppercase()),
    }
    .to_string()
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

#[cfg(test)]
mod tests {
    // 注意：这里**不能**写 `use super::*`——`file_item` 顶部的 `use gpui_kit::*`
    // 会把 gpui 的 `test` 属性宏一起带进来，遮蔽内置的 `#[test]`
    // （表现是 "recursion limit reached while expanding `#[test]`"）。
    use super::view;
    use super::{DATE_W, KIND_W, SIZE_W};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        div, px, size, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
        TestAppContext, VisualTestContext, Window,
    };
    use mo_core::{Entry, EntryKind, FileId, FileMetadata, MetadataState, Permissions};
    use std::path::PathBuf;

    /// 复刻 `file_list::render` 里的行容器（宽满行 + 垂直居中 + 4px 内边距）。
    struct RowProbe(Entry);

    impl Render for RowProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .h(px(24.0))
                .p(px(4.0))
                .debug_selector(|| "mo-probe-row".to_string())
                .child(view(&self.0, false, None))
        }
    }

    fn entry_named(name: &str) -> Entry {
        let mut entry = Entry::new(
            FileId::new(1, 1),
            name.to_string(),
            EntryKind::File,
            PathBuf::from("/tmp/x"),
        );
        entry.metadata = MetadataState::Loaded(FileMetadata {
            size: 4096,
            modified: Some(std::time::SystemTime::UNIX_EPOCH),
            created: None,
            permissions: Permissions::default(),
        });
        entry
    }

    /// 大小列必须固定在「种类」列左侧、种类列贴行右缘，各列间距一致（gap 8）。
    ///
    /// 曾经的 bug：`view()` 的根容器没有 `flex_1()`，整行按内容宽度收缩，
    /// 于是文件名一长一短、大小列就参差不齐（用户截图里 `96 B` / `128 B`
    /// 不在同一列）。行宽 600、内边距 4，大小列右缘应当是 596。
    ///
    /// 第二个 bug 藏得更深：flex item 的自动最小尺寸是 min-content（整个文件名的宽度），
    /// 所以超长文件名会把右侧列**顶出**行外（实测 712 > 596）。
    /// 根容器补 `overflow_hidden()` 才把自动最小尺寸降为 0。
    ///
    /// 这里用普通 `#[test]` + `TestAppContext::single()`：`#[gpui_kit::test]`
    /// 在 crate 内部展开会因宏递归爆栈（集成测试不受影响）。
    #[test]
    fn meta_columns_are_pinned_right_and_aligned() {
        for name in [
            "a.txt",
            "a-very-long-file-name-that-would-push-the-columns-away.txt",
        ] {
            let mut cx = TestAppContext::single();
            let window = cx.open_window(size(px(600.), px(200.)), |_, _cx| {
                RowProbe(entry_named(name))
            });
            let mut cx = VisualTestContext::from_window(window.into(), &cx);
            cx.update(|window, cx| window.render_frame(cx));

            let row = cx.debug_bounds("mo-probe-row").expect("行没有渲染");
            let date = cx.debug_bounds("mo-date-cell").expect("日期列没有渲染");
            let cell = cx.debug_bounds("mo-size-cell").expect("大小列没有渲染");
            let kind = cx.debug_bounds("mo-kind-cell").expect("种类列没有渲染");

            // 种类列贴行右缘（内边距 4）。
            assert_eq!(
                kind.origin.x + kind.size.width,
                row.origin.x + row.size.width - px(4.),
                "「{name}」的种类列没有贴在行右缘：row={row:?} kind={kind:?}"
            );
            assert_eq!(kind.size.width, px(KIND_W), "种类列宽度被压缩了");
            // 大小列在种类列左侧，间距 8（行 gap）。
            assert_eq!(
                cell.size.width,
                px(SIZE_W),
                "「{name}」的大小列宽度被压缩了"
            );
            assert_eq!(
                cell.origin.x + cell.size.width + px(8.),
                kind.origin.x,
                "「{name}」大小列与种类列间距不对：cell={cell:?} kind={kind:?}"
            );
            // 日期列在大小列左侧，同样间距 8。
            assert_eq!(date.size.width, px(DATE_W), "日期列宽度被压缩了");
            assert_eq!(
                date.origin.x + date.size.width + px(8.),
                cell.origin.x,
                "「{name}」日期列与大小列间距不对：date={date:?} cell={cell:?}"
            );
        }
    }
}
