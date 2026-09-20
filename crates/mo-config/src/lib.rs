//! mo-config：应用配置（持久化为 JSON）。
//!
//! 配置项随自定义系统扩展（主题、布局、快捷键、预览方式等）。所有可选字段都带
//! `#[serde(default)]`，手改过的旧配置文件不会因为缺字段而整体读不出来。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 一个自定义主题的配色覆盖。
///
/// 只写想改的角色，其余继承内置基底（`dark` 决定基底是深色还是浅色）。
/// 颜色写成 `#RRGGBB`，方便直接改配置文件。角色名见 `mo_ui::theme::Palette`：
/// `text` / `muted` / `container` / `surface` / `separator` / `selected_bg` /
/// `selected_text` / `hover_bg` / `zebra` / `divider` / `accent`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeColors {
    /// 基底是否深色（决定未覆盖角色用哪套内置色）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub dark: bool,
    /// 逐角色覆盖，键是角色名，值是 `#RRGGBB`。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub overrides: HashMap<String, String>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// 应用配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 主题名："light" / "dark" / "system"（跟随系统）/ 自定义主题 key。
    #[serde(default)]
    pub theme: String,
    /// 自定义主题：名称 → 配色覆盖（第四阶段·主题系统）。
    #[serde(default)]
    pub custom_themes: HashMap<String, ThemeColors>,
    /// 快捷键绑定：动作 id → 键描述（第四阶段·自定义快捷键）。
    ///
    /// 只记录与默认值不同的覆盖；没出现的动作用内置默认键位。
    #[serde(default)]
    pub keybindings: HashMap<String, String>,
    /// 是否显示隐藏文件。
    #[serde(default)]
    pub show_hidden: bool,
    /// 侧边栏书签（路径字符串）。
    #[serde(default)]
    pub sidebar_bookmarks: Vec<String>,
    /// 文件标签：路径字符串 → 颜色名（见 `mo_app::TAG_COLORS`）。
    #[serde(default)]
    pub tags: HashMap<String, String>,
    /// 列表视图列宽 / 列序覆盖（第四阶段·自定义布局）。
    #[serde(default)]
    pub columns: ColumnPrefs,
    /// 界面选项（布局相关，见 [`UiPrefs`]）。
    #[serde(default)]
    pub ui: UiPrefs,
    /// 用户自定义命令（写在这里 + `commands/*.json` 清单目录两处都会加载）。
    #[serde(default)]
    pub commands: Vec<UserCommand>,
    /// 自动化工作流：每条是一串按顺序执行的命令模板（任一步失败即中止）。
    #[serde(default)]
    pub workflows: Vec<Workflow>,
    /// 文件夹同步配对：源目录 → 目标目录。
    #[serde(default)]
    pub sync_pairs: HashMap<String, String>,
}

/// 用户自定义命令（第四阶段·自定义命令，也是插件系统的命令面）。
///
/// `shell` 是要执行的命令行，支持三个占位符：
/// * `{dir}` —— 当前目录；
/// * `{file}` —— 当前选中的第一个条目；
/// * `{files}` —— 全部选中条目（空格分隔并自动加引号）。
///
/// 占位符在传给 shell 之前逐个加引号转义，用户文件名里有空格 / 括号 / 中文
/// 都不会把命令拆坏。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserCommand {
    /// 命令面板里显示的名字。
    pub name: String,
    /// 分组标签（默认「自定义」）。
    #[serde(default)]
    pub category: String,
    /// 要执行的命令行（含占位符）。
    pub shell: String,
    /// 命令来源：内建配置里写的，还是从 `commands/` 目录加载的清单。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// 一个自动化工作流：名字 + 一串按顺序执行的命令模板。
///
/// 每一步的写法与 [`UserCommand`] 的 `shell` 完全相同（支持 `{dir}` /
/// `{file}` / `{files}` 占位符）；执行与中止语义在 `mo_app::workflows`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    /// 展示名（也是配置里的唯一键）。
    pub name: String,
    /// 步骤：每一项是一条命令模板。
    pub steps: Vec<String>,
    /// 来源（扩展清单带来的会写上文件路径，便于排错）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// 列表列的持久化偏好：顺序 + 各列宽度。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnPrefs {
    /// 列顺序（`name` / `date` / `size` / `kind`）；为空表示默认顺序。
    #[serde(default)]
    pub order: Vec<String>,
    /// 列名 → 宽度（逻辑像素）。缺失的列用默认宽度。
    #[serde(default)]
    pub widths: HashMap<String, f32>,
}

/// 界面布局偏好。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiPrefs {
    /// 显示侧边栏。
    #[serde(default = "truthy")]
    pub sidebar: bool,
    /// 显示状态栏。
    #[serde(default = "truthy")]
    pub status_bar: bool,
    /// 列表斑马纹。
    #[serde(default = "truthy")]
    pub zebra: bool,
    /// 默认视图模式：`list` / `grid` / `gallery` / `columns`。
    #[serde(default = "default_view_mode")]
    pub view_mode: String,
}

fn truthy() -> bool {
    true
}

fn default_view_mode() -> String {
    "list".to_string()
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            sidebar: true,
            status_bar: true,
            zebra: true,
            view_mode: default_view_mode(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "light".to_string(),
            custom_themes: HashMap::new(),
            keybindings: HashMap::new(),
            show_hidden: false,
            sidebar_bookmarks: Vec::new(),
            tags: HashMap::new(),
            columns: ColumnPrefs::default(),
            ui: UiPrefs::default(),
            commands: Vec::new(),
            workflows: Vec::new(),
            sync_pairs: HashMap::new(),
        }
    }
}

impl Config {
    /// 从 `path` 加载；文件不存在时返回默认配置。
    ///
    /// ⚠️ 先剥 UTF-8 BOM：第四阶段的配置是给用户手改的，Windows 的记事本 /
    /// PowerShell `Set-Content -Encoding UTF8` 都会写 BOM，而 `serde_json`
    /// 遇到 BOM 直接解析失败——不剥的话整份配置会静默回落默认值（表现为
    /// 「改了主题没反应」）。
    pub fn load(path: &Path) -> Result<Self, String> {
        if path.exists() {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let s = std::str::from_utf8(
                bytes
                    .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
                    .unwrap_or(&bytes),
            )
            .map_err(|e| e.to_string())?;
            serde_json::from_str(s.trim_start()).map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 带 BOM 的配置必须照常解析（Windows 手改配置的常见形态）。
    #[test]
    fn load_tolerates_utf8_bom() {
        let dir = std::env::temp_dir().join(format!("mo-config-bom-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        let body = r#"{"theme":"dark","sidebar_bookmarks":["/tmp/x"]}"#;

        std::fs::write(
            &path,
            [[0xEFu8, 0xBB, 0xBF].as_slice(), body.as_bytes()].concat(),
        )
        .unwrap();
        assert_eq!(Config::load(&path).unwrap().theme, "dark");

        std::fs::write(&path, body).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.theme, "dark");
        assert_eq!(cfg.sidebar_bookmarks, vec!["/tmp/x".to_string()]);
        assert_eq!(cfg.ui.view_mode, "list", "缺字段走默认值");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧配置文件（缺新字段）不能整体读不出来。
    #[test]
    fn old_minimal_config_still_loads() {
        let dir = std::env::temp_dir().join(format!("mo-config-old-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"theme":"light","show_hidden":true}"#).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert!(cfg.show_hidden);
        assert_eq!(cfg.theme, "light");
        assert!(cfg.custom_themes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
