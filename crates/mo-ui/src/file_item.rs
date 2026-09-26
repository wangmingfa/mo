use gpui_kit::*;
use mo_app::AppState;
use mo_core::{Bitmap, Entry, EntryKind, MetadataState, ThumbnailState};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use crate::list_columns::{ColId, ColumnLayout};

/// 列表行 / 列视图行的图标槽位边长（同时也是系统图标 / 缩略图这类光栅图标的绘制尺寸）。
///
/// 定得比行高（`listing::row_height` = 24px）小一圈，上下才留得出呼吸；
/// 槽位定宽是因为 SVG 图标与位图缩略图的自然宽度不同，不定宽的话
/// 缩略图一加载文件名就会左右抖动。
pub(crate) const ICON_PX: f32 = 16.0;

/// 内置 Lucide 单色 SVG 的绘制尺寸。比槽位再小一圈：描边图形的**视觉**边界
/// 比它的绘制框小（`icon()` 的 viewBox 自带留白），跟铺满槽位的位图图标
/// 摆在一起才显得一样大。
const GLYPH_PX: f32 = 12.0;

/// 这一行该不该问**系统图标**——排除了「这行画的是缩略图」的情况。
///
/// 有缩略图（已就绪 / 正在出）的行**不问**：那行画的就是缩略图，白问系统一次
/// （每张 1.5–12ms 的活，攒起来正是进目录时那一下卡顿）。
pub(crate) fn wants_system_icon(thumbnail: &ThumbnailState) -> bool {
    !matches!(
        thumbnail,
        ThumbnailState::Loaded(_) | ThumbnailState::Loading
    )
}

/// 问系统图标（访达同款位图，后台已解码成内存位图）：**四个视图共用这一条链路**。
///
/// 纯查表，命中给位图、没命中只记一笔账并返回 `None`——调用方就此退回内置
/// SVG，真活由后台图标泵补（见 `AppState::spawn_icon_pump`）。所以这里可以每帧
/// 每行地调，零 IO。位图经 [`crate::bitmap::image_source`] 包成 `RenderImage`
/// 后同步上屏——不走 `img(path)` 的异步读盘，那一格永远不会空着等。
///
/// `slot_pt` 是显示槽位的边长（[`crate::listing::icon_slot`]）：系统图标是光栅图，
/// 得先知道要放进多大的地方，才知道该取 40px 还是 128px 那一档。
///
/// 远程页整页都是远程条目、本机没有这些文件，`file_icon` 直接返回 `None`（内置 SVG
/// 顶上），所以这里不必特判。**列视图的 `LightEntry` 没有缩略图状态，直接调这一层**；
/// 有 `Entry` 的视图走 [`entry_system_icon`]，别自己拼判据。
pub(crate) fn system_icon(
    app: Option<&AppState>,
    path: &Path,
    is_dir: bool,
    slot_pt: f32,
) -> Option<Arc<Bitmap>> {
    app.and_then(|a| a.file_icon(path, is_dir, slot_pt))
}

/// 列表 / 网格 / 画廊的行（[`Entry`]）取系统图标：先过「有缩略图就不问」那道，
/// 其余交给 [`system_icon`]。
pub(crate) fn entry_system_icon(
    app: Option<&AppState>,
    entry: &Entry,
    slot_pt: f32,
) -> Option<Arc<Bitmap>> {
    if !wants_system_icon(&entry.thumbnail) {
        return None;
    }
    system_icon(app, &entry.path, entry.kind.is_dir(), slot_pt)
}

/// 单个文件 / 文件夹行的纯展示（不含交互；交互在 `file_list` 中处理）。
///
/// 列顺序 / 宽度全部取自 `layout`（表头与数据行共用同一份，保证上下对齐）：
/// 名称列弹性可伸缩，其余列固定宽右对齐。用户拖动表头改列宽 / 列顺序后，
/// 数据行下一帧就跟着变——渲染逻辑里没有任何写死的列序。
/// 位图（缩略图 / 系统图标）由后台泵备成内存位图，这里经
/// [`crate::bitmap::image_source`] 转成 `ImageSource::Render` 同步上屏——
/// `img(path)` 的异步读盘会让那一格空一两帧（切目录时的闪烁），不走。
pub fn view(
    entry: &Entry,
    selected: bool,
    tag: Option<String>,
    layout: &ColumnLayout,
    system_icon: Option<Arc<Bitmap>>,
) -> impl IntoElement {
    // 文件类型图标：统一 Lucide 风格、单色描边，颜色随选中态（蓝底用白字）。
    let icon_data = crate::icons::entry_icon(entry);
    let icon_color = if selected {
        crate::theme::selected_text()
    } else {
        crate::theme::text()
    };

    // ⚠️ `flex_1()` 不是装饰：`file_list` 的行容器是 `w_full()` + `items_center()`，
    // 里面的元素默认按**内容宽度**收缩。少了它，这一行就只有内容那么宽，
    // 右侧的列会紧贴着文件名参差不齐，而不是对齐成固定列。
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
            .w(px(ICON_PX))
            .h(px(ICON_PX))
            .flex_shrink_0()
            .overflow_hidden()
            .child(child)
    };

    // 图标 + 颜色标签 + 文件名打包成「名称列」——列可以拖到任意位置，
    // 图标始终跟着文件名走（不会跟列序脱节）。
    let mut name_cell = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .flex_1()
        .overflow_hidden();

    // 这一行「名称列图标槽」里要画的位图：缩略图优先，其次系统图标，
    // 都没有才退回内置 Lucide 描边 SVG（`Loading` 态是空槽，生产从不置位）。
    // 位图源是同步的（`ImageSource::Render`），永不出现空窗。
    let raster_source = match &entry.thumbnail {
        ThumbnailState::Loaded(b) => crate::bitmap::image_source(b),
        ThumbnailState::Loading => None,
        _ => system_icon.as_ref().and_then(crate::bitmap::image_source),
    };

    name_cell = match raster_source {
        Some(src) => name_cell.child(icon_slot(
            img(src).w(px(ICON_PX)).h(px(ICON_PX)).into_any_element(),
        )),
        None if matches!(entry.thumbnail, ThumbnailState::Loading) => {
            name_cell.child(icon_slot(text!("".to_string()).into_any_element()))
        }
        // 没有位图时退回内置 Lucide 单色 SVG。系统图标是光栅位图，没法随选中态
        // 改色，但胜在「.app 是真 App 图标、文档是所属 App 图标」，与系统一致。
        //
        // 位图铺满槽位、描边 SVG 缩一圈（`GLYPH_PX`）：两者视觉大小才对得上。
        None => {
            let icon = crate::icons::icon(icon_data, GLYPH_PX, icon_color).into_any_element();
            name_cell.child(icon_slot(icon))
        }
    };

    // 颜色标签（Finder 式）：有标签时文件名前显示一个色点。
    if let Some(color) = tag {
        name_cell = name_cell.child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .flex_shrink_0()
                .rounded(px(4.0))
                .bg(crate::dialogs::tag_color(&color)),
        );
    }

    name_cell = name_cell.child(
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
            .child(text!(entry.display_name().to_string())),
    );

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

    // 右侧元数据列统一 12px：比文件名淡一档，视觉层次与 Finder 一致。
    let meta_color = if selected {
        crate::theme::selected_text()
    } else {
        crate::theme::muted()
    };
    let meta_cell = |col: ColId, label: String, selector: &'static str| {
        let w = layout.width(col);
        let mut cell = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .w(px(w))
            .flex_shrink_0()
            .overflow_hidden()
            .text_size(px(12.0))
            .text_color(meta_color)
            // 测试用（release no-op）：本文件单测断言这些列的位置
            .debug_selector(move || selector.to_string());
        // 文本过长（列被拖窄）时截断，而不是溢出到相邻列。
        // ⚠️ `text!` 宏会按调用点位置生成元素 ID；本闭包对同一行渲染 3 次
        // （日期 / 大小 / 种类），若不显式给 ID，三段文本会得到完全相同的
        // 元素 ID 路径 → 相同的 a11y NodeId → 开启辅助功能时触发
        // "Duplicate a11y node id" panic。用每列唯一的 selector 作 ID。
        cell = cell.child(div().truncate().child(text!(id = selector, label)));
        cell
    };

    // 按布局里的列序拼装：名称列弹性，其余固定宽。
    // （`Div` 不是 `Clone`，所以名称列用 `Option` 交出唯一那份。）
    let mut name_cell = Some(name_cell);
    for col in &layout.order {
        let cell: AnyElement = match col {
            ColId::Name => name_cell
                .take()
                .expect("名称列在列序里只会出现一次")
                .into_any_element(),
            ColId::Date => meta_cell(*col, date.clone(), "mo-date-cell").into_any_element(),
            ColId::Size => meta_cell(*col, size.clone(), "mo-size-cell").into_any_element(),
            ColId::Kind => meta_cell(*col, kind_label(entry), "mo-kind-cell").into_any_element(),
        };
        row = row.child(cell);
    }
    row
}

/// 本地时间格式化，Finder 中文样式：`2026年4月21日 10:33`。
pub(crate) fn format_modified(t: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format("%Y年%m月%d日 %H:%M").to_string()
}

/// 回收站条目的「种类」文案：目录固定，文件按扩展名归类（与 [`kind_label`]
/// 同一套映射，只是回收站条目没有 `Entry` 可包——账本只有 is_dir + 原名）。
pub(crate) fn trash_kind_label(is_dir: bool, name: &str) -> String {
    if is_dir {
        "文件夹".to_string()
    } else {
        kind_by_ext(name)
    }
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
        // Windows 快捷方式：资源管理器也叫它「快捷方式」，而不是「LNK 文件」——
        // 列表里的名字已经藏起 `.lnk` 后缀了，种类再提一遍后缀就对不上。
        "lnk" => "快捷方式",
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
    use super::{kind_label, trash_kind_label, view};
    use crate::list_columns::{ColId, ColumnLayout};
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
                .child(view(&self.0, false, None, &ColumnLayout::default(), None))
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

    /// 快捷方式的「种类」：名字列已经藏起 `.lnk` 了，种类再写「LNK 文件」就对不上
    /// 资源管理器（那边两列分别是 `Atlas` 与「快捷方式」）。
    #[test]
    fn shortcut_kind_says_shortcut_not_the_suffix() {
        assert_eq!(kind_label(&entry_named("Atlas.lnk")), "快捷方式");
        assert_eq!(kind_label(&entry_named("Atlas.LNK")), "快捷方式");
        // 回收站条目那份走同一张表（没有 `Entry` 可包）。
        assert_eq!(trash_kind_label(false, "Atlas.lnk"), "快捷方式");
        // 普通类型不受影响。
        assert_eq!(kind_label(&entry_named("notes.txt")), "文本文档");
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
        let layout = ColumnLayout::default();
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
            assert_eq!(
                kind.size.width,
                px(layout.width(ColId::Kind)),
                "种类列宽度被压缩了"
            );
            // 大小列在种类列左侧，间距 8（行 gap）。
            assert_eq!(
                cell.size.width,
                px(layout.width(ColId::Size)),
                "「{name}」的大小列宽度被压缩了"
            );
            assert_eq!(
                cell.origin.x + cell.size.width + px(8.),
                kind.origin.x,
                "「{name}」大小列与种类列间距不对：cell={cell:?} kind={kind:?}"
            );
            // 日期列在大小列左侧，同样间距 8。
            assert_eq!(
                date.size.width,
                px(layout.width(ColId::Date)),
                "日期列宽度被压缩了"
            );
            assert_eq!(
                date.origin.x + date.size.width + px(8.),
                cell.origin.x,
                "「{name}」日期列与大小列间距不对：date={date:?} cell={cell:?}"
            );
        }
    }

    /// 数据行按布局里的列序渲染：把「种类」调到最前，它就该出现在行首。
    #[test]
    fn columns_follow_layout_order() {
        let mut layout = ColumnLayout::default();
        assert!(layout.move_col(3, 0), "把种类列拖到最前");

        let mut cx = TestAppContext::single();
        let window = cx.open_window(size(px(600.), px(200.)), |_, _cx| {
            OrderProbe(entry_named("a.txt"), layout.clone())
        });
        let mut cx = VisualTestContext::from_window(window.into(), &cx);
        cx.update(|window, cx| window.render_frame(cx));

        let kind = cx.debug_bounds("mo-kind-cell").expect("种类列没有渲染");
        let date = cx.debug_bounds("mo-date-cell").expect("日期列没有渲染");
        assert!(
            kind.origin.x < date.origin.x,
            "种类列没有被排到日期列前面：kind={kind:?} date={date:?}"
        );
    }

    /// 复刻数据行容器，但用自定义列布局。
    struct OrderProbe(Entry, ColumnLayout);

    impl Render for OrderProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .h(px(24.0))
                .p(px(4.0))
                .child(view(&self.0, false, None, &self.1, None))
        }
    }
}
