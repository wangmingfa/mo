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

/// 目录的显示名：已知文件夹用侧边栏那套中文名，其余退回 [`last_segment`]。
///
/// 面包屑与标签页原来直接抄目录名，于是 Windows 上侧栏写「桌面」、地址栏写
/// `Desktop`（`D:\Users\wmf12\Desktop` 的真实名字）。标签表在
/// [`mo_app::known_folder_labels`]，两边同源，不会再各写一份。
pub(crate) fn folder_label(path: &Path) -> String {
    match known_folder_label(path) {
        Some(label) => label.to_string(),
        None => last_segment(path),
    }
}

/// 这条路径是不是某个已知文件夹。
pub(crate) fn known_folder_label(path: &Path) -> Option<&'static str> {
    mo_app::known_folder_labels()
        .iter()
        .find(|(known, _)| same_dir(known, path))
        .map(|(_, label)| *label)
}

/// 两个目录路径是否指同一个地方：忽略尾部分隔符；Windows 上再忽略大小写。
///
/// 大小写不是吹毛求疵：已知文件夹从 `dirs` 拿（`...\Desktop`），而用户从别处
/// 粘来的、或书签里存的可能写成 `...\desktop`，判据不一致就会出现「同一个目录
/// 一会儿显示「桌面」一会儿显示 `desktop`」。
fn same_dir(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> String {
        let s = p
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_string();
        #[cfg(target_os = "windows")]
        {
            s.to_ascii_lowercase()
        }
        #[cfg(not(target_os = "windows"))]
        {
            s
        }
    }
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    // ⚠️ 显式导入而非 `use super::*`：其他模块的 `use super::*` 会把
    // `gpui_kit::*` 一并 glob 进来，其 `test` 与 `#[test]` 撞名。
    use super::{folder_label, last_segment, Path};

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

    /// 已知文件夹必须显示成侧边栏那份标签，而不是磁盘上的目录名。
    ///
    /// 拿真机上的表来问，不写死路径：这台机器桌面被重定向到 `D:\Users\x\Desktop`，
    /// 换一台就是 `C:\...`，写死断言的测试只会在一半的机器上跑得过。
    #[test]
    fn known_folders_share_the_sidebar_labels() {
        let table = mo_app::known_folder_labels();
        assert!(!table.is_empty(), "至少该解析出主目录");
        for (path, label) in table {
            assert_eq!(&folder_label(path), label, "{path:?} 该显示成侧栏那份标签");
            // 尾部分隔符不算差异：从地址栏复制回来的路径常带一个。
            let with_slash = format!("{}{}", path.display(), std::path::MAIN_SEPARATOR);
            assert_eq!(
                &folder_label(Path::new(&with_slash)),
                label,
                "尾部斜杠不该让标签掉回目录名"
            );
        }
    }

    /// Windows 上大小写不算差异（`desktop` 与 `Desktop` 是同一个目录）。
    #[cfg(target_os = "windows")]
    #[test]
    fn known_folder_match_ignores_case_on_windows() {
        for (path, label) in mo_app::known_folder_labels() {
            let lower = path.to_string_lossy().to_ascii_lowercase();
            assert_eq!(
                &folder_label(Path::new(&lower)),
                label,
                "小写写法 {lower:?} 也要认"
            );
        }
    }

    /// 不是已知文件夹的目录照旧取最后一段——这张表不该把普通目录也改名。
    #[test]
    fn other_dirs_keep_their_real_name() {
        assert_eq!(
            folder_label(Path::new("/some/place/mo-not-a-known-folder")),
            "mo-not-a-known-folder"
        );
    }
}
