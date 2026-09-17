use std::path::PathBuf;

use anyhow::Result;

use crate::AppState;

/// 目录控制器：把"打开目录"拆成两步——
/// 1) 读取目录项（立即可见）；2) 后台加载元数据（渐进式）。
///
/// 对应架构中的 DirectoryController / DirectoryModel：
///
/// ```text
/// UI → Application → DirectoryController → DirectoryModel → FilesystemService → Platform
/// ```
pub struct DirectoryController {
    app: AppState,
}

impl DirectoryController {
    pub fn new(app: AppState) -> Self {
        Self { app }
    }

    /// 打开目录。
    ///
    /// 元数据加载由 `AppState::load_path` 内部触发（首屏优先），
    /// 这里不再重复调度一次，否则等于把整目录的 stat 跑两遍。
    pub async fn open(&self, path: &PathBuf) -> Result<()> {
        Ok(self.app.open_directory(path).await?)
    }

    /// 打开父目录。
    pub async fn open_parent(&self) -> Result<()> {
        Ok(self.app.open_parent().await?)
    }
}
