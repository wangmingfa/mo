//! 路径的「短标签」：UI 各处显示路径时统一取**最后一段**。
//!
//! 同一个需求在多处被独立写过一遍（列视图的列头、侧栏书签项），统一到这里，
//! 免得再写一遍、且各写各的。
//!
//! 为什么是最后一段而不是完整路径：这些位置都是**窄容器里的标识**（列头 210px、
//! 侧栏 190px）。完整绝对路径在里面会折行 / 被截得只剩前缀，而相邻
//! 项的路径前缀本来就大量重复（列视图相邻几列会各写一遍同一个父目录），信息量几乎
//! 为零、视觉噪音却很大。需要看全路径的地方是**地址栏**（面包屑）与对话框。

use std::path::Path;

/// 取路径的最后一段作为显示标签。
///
/// * `/Users/me/Projects` → `Projects`
/// * `/Users/me/Projects/` → `Projects`（尾部斜杠由 `Path` 规范化）
/// * `/` → `/`（没有最后一段，退回整条）
///
/// ⚠️ 这里只负责取名字，**不做省略号截断**：调用方要在窄容器里 `truncate()`
/// 才能保证单行（列头折行就会把各列的内容起始线错开，见 devlog §30）。
pub(crate) fn last_segment(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    // ⚠️ 显式导入而非 `use super::*`：其他模块的 `use super::*` 会把
    // `gpui_kit::*` 一并 glob 进来，其 `test` 与 `#[test]` 撞名。
    use super::{last_segment, Path};

    #[test]
    fn keeps_only_the_last_segment() {
        assert_eq!(last_segment(Path::new("/Users/11048490")), "11048490");
        assert_eq!(
            last_segment(Path::new(
                "/Users/11048490/.bluecode-desktop/change-sessions"
            )),
            "change-sessions",
            "窄容器里不该把整条路径铺出来（相邻项前缀重复、还会折行）"
        );
        // 尾部斜杠（用户从别处粘来的路径）不该变成空标签。
        assert_eq!(
            last_segment(Path::new("/Users/11048490/Downloads/")),
            "Downloads"
        );
    }

    #[test]
    fn falls_back_to_root_path() {
        // 根路径没有「最后一段」，退回整条（就是 `/`）。
        assert_eq!(last_segment(Path::new("/")), "/");
    }
}
