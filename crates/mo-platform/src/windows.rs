//! Windows 原生集成：目前只有「在资源管理器中显示」一条。
//!
//! 走 `explorer.exe /select,<path>`——资源管理器自己那条定位方式。两个
//! 约定决定了这里的写法（devlog 若有 Windows 条目，结论归那里）：
//!
//! * `/select` 认的是**反斜杠**路径：统一把 `/` 换成 `\` 再递，别赌解析器；
//! * explorer.exe 的**退出码不可信**（拉起了也常回非零），所以进程 spawn
//!   成功即算成事，不 `wait()` 收状态——收了反而会把一次正常的显示误报成失败。

use std::path::Path;

use crate::PlatformError;

/// 在资源管理器里选中 `path`（打开其所在目录并高亮该项）。
pub fn reveal(path: &Path) -> Result<(), PlatformError> {
    if !path.exists() {
        return Err(PlatformError::Failed(format!(
            "路径不存在，无法在资源管理器中显示：{}",
            path.display()
        )));
    }
    let target = path.display().to_string().replace('/', "\\");
    std::process::Command::new("explorer")
        .arg(format!("/select,{target}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| PlatformError::Failed(format!("explorer 起不来：{e}")))
}
