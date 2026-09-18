//! mo-config：应用配置（持久化为 JSON）。
//!
//! 配置项刻意保持精简，后续随自定义系统扩展（主题、布局、快捷键、预览方式等）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 应用配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// 主题名（"light" / "dark" / 自定义主题 key）。
    pub theme: String,
    /// 是否显示隐藏文件。
    pub show_hidden: bool,
    /// 侧边栏书签（路径字符串）。
    #[serde(default)]
    pub sidebar_bookmarks: Vec<String>,
    /// 文件标签：路径字符串 → 颜色名（见 `mo_app::TAG_COLORS`）。
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

impl Config {
    /// 从 `path` 加载；文件不存在时返回默认配置。
    pub fn load(path: &Path) -> Result<Self, String> {
        if path.exists() {
            let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            serde_json::from_str(&s).map_err(|e| e.to_string())
        } else {
            Ok(Config::default())
        }
    }

    /// 保存配置到 `path`（自动创建父目录）。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, s).map_err(|e| e.to_string())
    }
}
