use std::path::PathBuf;

/// 领域层命令（与应用层 `Action` 区分：这里是文件管理领域语义）。
///
/// 应用层把键盘 / 鼠标事件翻译成 `FileCommand`，再交给 `mo-app` 处理。
/// UI 永远不直接调用 `std::fs`，只发出命令。
#[derive(Debug, Clone)]
pub enum FileCommand {
    /// 打开指定目录。
    OpenDirectory(PathBuf),
    /// 打开父目录。
    OpenParent,
    /// 后退。
    Back,
    /// 前进。
    Forward,
    /// 刷新当前目录。
    Refresh,
    /// 打开文件（用系统关联程序）。
    OpenFile(PathBuf),
    /// 重命名。
    Rename { path: PathBuf, new_name: String },
    /// 删除（当前阶段为永久删除；回收站由 `mo-platform` 后续提供）。
    Delete(Vec<PathBuf>),
    /// 复制 sources -> dest 目录。
    Copy {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
    /// 移动 sources -> dest 目录。
    Move {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
}
