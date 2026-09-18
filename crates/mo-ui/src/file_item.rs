use gpui_kit::*;
use mo_core::{Entry, MetadataState, ThumbnailState};

/// 单个文件 / 文件夹行的纯展示（不含交互；交互在 `file_list` 中处理）。
///
/// 缩略图来自 `mo-thumbnails` 生成的磁盘缓存；GPUI 可以直接从文件路径加载图片，
/// 因此这里只需把缓存路径交给 `img()`——领域层不必知道任何 UI 类型。
pub fn view(entry: &Entry, selected: bool, tag: Option<String>) -> impl IntoElement {
    let icon = match entry.kind {
        mo_core::EntryKind::Directory => "📁",
        mo_core::EntryKind::File => "📄",
        mo_core::EntryKind::Symlink => "🔗",
        mo_core::EntryKind::Other => "❓",
    };

    // ⚠️ `flex_1()` 不是装饰：`file_list` 的行容器是 `w_full()` + `items_center()`，
    // 里面的元素默认按**内容宽度**收缩。少了它，这一行就只有「图标 + 文件名 + 大小」
    // 那么宽，右侧的大小列会紧贴着文件名参差不齐，而不是对齐成固定列。
    //
    // `overflow_hidden()` 也不是装饰：flex item 的自动最小尺寸是 min-content
    // （也就是完整文件名的宽度），长文件名会把大小列顶出行外。
    // 按 CSS 规则 hidden 会把自动最小尺寸降为 0，文件名的收缩交给下面那一层。
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_1()
        .overflow_hidden()
        .gap(px(6.0));

    // 图标槽位**定宽**：emoji 图标与图片缩略图的自然宽度不同，
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
        ThumbnailState::Loading => row.child(icon_slot(text!("⏳".to_string()).into_any_element())),
        _ => row.child(icon_slot(text!(icon.to_string()).into_any_element())),
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
            // 防止长名把右侧大小列顶出去。
            .truncate()
            // 选中时整行是 Finder 蓝底，文字改白以保证对比度。
            .text_color(if selected {
                crate::theme::selected_text()
            } else {
                crate::theme::text()
            })
            .child(text!(entry.name.clone())),
    );

    // 元数据是后台加载的，尚未就绪时留空而不是显示 0，
    // 避免用户把「还没加载」误读成「文件是空的」。
    let size = match &entry.metadata {
        MetadataState::Loaded(m) => format_size(m.size),
        MetadataState::Loading => String::new(),
        MetadataState::Failed(_) => "—".to_string(),
    };
    row.child(
        div()
            .flex()
            .flex_row()
            .justify_end()
            .w(px(80.0))
            .text_color(if selected {
                crate::theme::selected_text()
            } else {
                crate::theme::muted()
            })
            // 测试用（release no-op）：本文件单测断言这一列贴在行右缘
            .debug_selector(|| "mo-size-cell".to_string())
            .child(text!(size)),
    )
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
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        div, px, size, Bounds, Context, InteractiveElement, IntoElement, ParentElement, Pixels,
        Render, Styled, TestAppContext, VisualTestContext, Window,
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
            modified: None,
            created: None,
            permissions: Permissions::default(),
        });
        entry
    }

    /// 渲染一行，返回（行、大小列）的真实布局。
    fn row_and_size_cell(name: &str) -> (Bounds<Pixels>, Bounds<Pixels>) {
        let mut cx = TestAppContext::single();
        let window = cx.open_window(size(px(600.), px(200.)), |_, _cx| {
            RowProbe(entry_named(name))
        });
        let mut cx = VisualTestContext::from_window(window.into(), &cx);
        cx.update(|window, cx| window.render_frame(cx));

        (
            cx.debug_bounds("mo-probe-row").expect("行没有渲染"),
            cx.debug_bounds("mo-size-cell").expect("大小列没有渲染"),
        )
    }

    /// 大小列必须固定在行的右缘，而不是紧跟在文件名后面。
    ///
    /// 曾经的 bug：`view()` 的根容器没有 `flex_1()`，整行按内容宽度收缩，
    /// 于是文件名一长一短、大小列就参差不齐（用户截图里 `96 B` / `128 B`
    /// 不在同一列）。行宽 600、内边距 4，大小列右缘应当是 596。
    ///
    /// 第二个 bug 藏得更深：flex item 的自动最小尺寸是 min-content（整个文件名的宽度），
    /// 所以超长文件名会把大小列**顶出**行外（实测 712 > 596）。
    /// 根容器补 `overflow_hidden()` 才把自动最小尺寸降为 0。
    ///
    /// 这里用普通 `#[test]` + `TestAppContext::single()`：`#[gpui_kit::test]`
    /// 在 crate 内部展开会因宏递归爆栈（集成测试不受影响）。
    #[test]
    fn size_column_is_pinned_to_the_right_edge() {
        for name in [
            "a.txt",
            "a-very-long-file-name-that-would-push-the-size-column-away.txt",
        ] {
            let (row, cell) = row_and_size_cell(name);
            assert_eq!(
                cell.origin.x + cell.size.width,
                row.origin.x + row.size.width - px(4.),
                "「{name}」的大小列没有贴在行右缘：row={row:?} cell={cell:?}"
            );
            assert_eq!(cell.size.width, px(80.), "「{name}」的大小列宽度被压缩了");
        }
    }
}
