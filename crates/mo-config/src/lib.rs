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

/// 上次退出时的窗口布局（会话恢复：重开 Mo 回到上次那些标签页）。
///
/// ⚠️ 单独一个文件（`session.json`），**不进 `config.json`**：这是机器写的状态
/// （每次导航都变），而 config.json 是给人手改的——混在一起会互相覆盖（两边
/// 都是「读整份 → 改一个字段 → 写回」，一个写会话一个写设置，后写的抹掉先写的）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// 每个窗格的标签页（外层 = 窗格，内层 = 标签页）。
    #[serde(default)]
    pub panes: Vec<Vec<SavedTab>>,
    /// 每个窗格当前是第几个标签页（比 `panes` 短就按 0 处理）。
    #[serde(default)]
    pub active_tabs: Vec<usize>,
    /// 退出时在第几个窗格。
    #[serde(default)]
    pub active_pane: usize,
    /// 是否分栏。
    #[serde(default)]
    pub split: bool,
    /// 命令面板的使用账目（命令稳定键 → 次数与最近一次时刻）。
    ///
    /// 面板按频次排序、「最近使用」标记都从这里读；键的稳定规则见
    /// `mo_ui` 的 `command_key_of`（内建命令 = 变体名，自定义命令 = `user:<名字>`，
    /// 工作流 = `workflow:<名字>`——名字改了账目就归零，可接受）。
    #[serde(default)]
    pub command_usage: HashMap<String, CommandUsage>,
}

/// 一条命令的使用账目。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommandUsage {
    /// 累计执行次数（面板排序的依据）。
    #[serde(default)]
    pub count: u64,
    /// 最近一次执行的 Unix 毫秒时刻（「最近使用」标记的依据）。
    #[serde(default)]
    pub last_used_ms: u64,
}

/// 一个恢复出来的标签页。
///
/// 本地与远程互斥：远程标签页要记**端点**（重连用）而不是路径——路径在远端，
/// 本机没有它；反过来说，只记路径就永远恢复不回远程页。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    /// 本机目录（远程标签页为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 远程端点 `scheme://host:port`（**不含**用户名 / 密码 / 路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// 远程标签页当时在看的路径（重连成功后回到这里）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

impl Session {
    /// 有没有值得恢复的东西（空会话 = 首次启动 / 上次一个标签页都没剩）。
    pub fn is_empty(&self) -> bool {
        self.panes.iter().all(|p| p.is_empty())
    }

    /// 从 `path` 加载；文件不存在 / 解析失败 → 空会话。
    ///
    /// 解析失败**不报错、只当没有**：这是机器态，坏了顶多不恢复（回到默认那一
    /// 页），不该像配置那样吵——更不能因为它把启动搞挂。
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Session::default();
        };
        let Ok(s) = std::str::from_utf8(
            bytes
                .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
                .unwrap_or(&bytes),
        ) else {
            return Session::default();
        };
        serde_json::from_str(s.trim_start()).unwrap_or_default()
    }

    /// 保存到 `path`（自动创建父目录）。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let s = serde_json::to_string(self).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 原子写：先落临时文件再 rename。settings 类写入会由 UI 高频触发，
        // 直接覆盖写撞上崩溃 / 断电就是半份 config（先例：mo-thumbnails 的缩略图缓存）。
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, s).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
}

/// 一台记住的远程服务器。
///
/// **只存地址与用户名，密码不在这里**——config.json 是明文，密码写进去等于换个
/// 地方泄露。密码存在系统钥匙串里，见 `mo_app::credentials`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedServer {
    /// 连接标识 `scheme://host[:port]`（不含用户名 / 密码 / 路径）。
    ///
    /// 规格见 `mo_remote::RemoteUrl::endpoint`：同一台机器换个用户登录、
    /// 或浏览到别的目录，都该落在同一条记录上。
    pub endpoint: String,
    /// 上次登录用的用户名（用来把「有用户名但没存密码」和「纯匿名」区分开）。
    #[serde(default)]
    pub user: String,
    /// 上次使用的 unix 时间戳（秒）。列表按它倒序，越近用的越靠前。
    #[serde(default)]
    pub last_used: i64,
    /// 用户收藏（☆→★）。收藏的排「最近」之前；旧配置缺这个字段默认 false。
    #[serde(default)]
    pub favorite: bool,
    /// FTPS 证书钉选：叶子证书的 SHA-256 指纹（十六进制，冒号可省）；`None` =
    /// 正常走系统信任库校验。自签名 / 内网 FTPS 过不了系统校验，用户在连接
    /// 对话框里粘一次指纹就长期生效。指纹是**公开证书**的哈希、不是机密，
    /// 明文存这里没有「密码进 config.json」那种问题（同 SSH known_hosts 的模型）。
    #[serde(default)]
    pub tls_fingerprint: Option<String>,
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
    /// 记住的远程服务器（最近使用的在前）。**不含密码**——密码在系统钥匙串。
    #[serde(default)]
    pub remote_servers: Vec<SavedServer>,
    /// 扩展设置（ext_id → {设置 key: 值}）。只记**用户改过的**值；与清单声明合并
    /// 后经 initialize 握手传给插件进程。声明没写 key 时用声明里的 default。
    #[serde(default)]
    pub extension_settings:
        std::collections::HashMap<String, std::collections::HashMap<String, serde_json::Value>>,
    /// 全局搜索索引的**排除规则**：条目名 glob，命中的整棵子树都不进索引。
    ///
    /// 为什么要有这张表：爬取的既有判据只有「隐藏条目不进索引」（`mo_search::crawl`），
    /// 但 `node_modules` / `target` / `dist` 这些**不点开头**的目录照样被整棵收进来——
    /// 一个项目根里它们能占九成条目，索引体积与爬取时间全耗在永远不会有人搜的
    /// 依赖上。默认表见 [`default_index_exclude`]。
    ///
    /// ⚠️ 语义是「搜不到」而不是「看不到」：列表照旧显示这些目录，只有全局搜索
    /// 的结果里没有它们。这是刻意的——排除的是搜索噪音，不是文件。
    #[serde(default = "default_index_exclude")]
    pub index_exclude: Vec<String>,
}

/// [`Config::index_exclude`] 的默认值：依赖目录与构建产物。
///
/// 刻意**写进**配置文件（而不是空着、运行时再套默认）：用户看得见才能改，
/// 「为什么搜不到 node_modules 里的东西」有地方可查。
fn default_index_exclude() -> Vec<String> {
    vec![
        "node_modules".to_string(),
        "target".to_string(),
        "dist".to_string(),
        "build".to_string(),
        "__pycache__".to_string(),
        ".venv".to_string(),
        "DerivedData".to_string(),
    ]
}

/// 一条动作出现在**哪些界面**（插件系统 P2 的投递声明）。
///
/// 写进清单 / 配置的是字符串（见 [`UserCommand::menu`]），字面量与
/// `devlog/plugin-system.md` §3 的 `where` 一致。为什么模型里存字符串而不是枚举：
/// `Config::load` 任何一处反序列化失败都会让 `AppState::config()` 把**整份**配置
/// 回落成默认值——一个拼错的槽位名不该清空用户所有设置。认不出的由
/// `mo_app::usercmds::validate` 报出来（错误可见），映射只在这一处（[`MenuSlot::parse`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSlot {
    /// 命令面板（⌘K / Ctrl+Shift+P 那一个）。
    Palette,
    /// 右键菜单：对着一个条目（文件或目录）。
    ContextFile,
    /// 右键菜单：空白处（动作对象是当前目录）。
    ContextBlank,
    /// 侧边栏的一行（P2-5）。落进哪个区由这条动作自己的 `category` 决定——**不再**
    /// 另设一个 `section` 字段，那是「同一个问题两处作答」（devlog §4.9）。
    ///
    /// ⚠️ 侧栏与快捷键一样**不看选区**：这一行永远是全局可见的，所以受 `when_ext`
    /// 约束的命令不能投到这里（判据在 `mo_app::extensions::validate`，与快捷键同一条）。
    Sidebar,
}

impl MenuSlot {
    /// 认一个写法；`None` = 不认识。冒号与连字符两种写法都收（`context:file` 是设计稿
    /// 里的形状，`context-file` 是手改配置时更顺手的那个）。
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_ascii_lowercase().as_str() {
            "palette" => Self::Palette,
            "context:file" | "context-file" => Self::ContextFile,
            "context:blank" | "context-blank" => Self::ContextBlank,
            "sidebar" | "side-bar" | "side:bar" => Self::Sidebar,
            _ => return None,
        })
    }

    /// 展示 / 回写用的规范写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Palette => "palette",
            Self::ContextFile => "context:file",
            Self::ContextBlank => "context:blank",
            Self::Sidebar => "sidebar",
        }
    }

    /// 一条命令**没写** `menu` 时的落点：只进命令面板——与 P2 之前逐条一致。
    pub fn defaults() -> Vec<Self> {
        vec![Self::Palette]
    }
}

/// 一批声明 → 落点。一条都没认出来就走 [`MenuSlot::defaults`]。
///
/// 「写错的槽位名」不在这里报——那是 [`first_bad_slot`] 的活，两处的判据不同：
/// 解析要**尽量可用**，校验要**明确报错**。
pub fn slots_of(raw: &[String]) -> Vec<MenuSlot> {
    let out: Vec<MenuSlot> = raw.iter().filter_map(|s| MenuSlot::parse(s)).collect();
    if out.is_empty() {
        MenuSlot::defaults()
    } else {
        out
    }
}

/// 一批声明里**第一个不认识的写法**（给 `validate` 报错用；`None` = 全部认识）。
pub fn first_bad_slot(raw: &[String]) -> Option<&str> {
    raw.iter()
        .map(|s| s.as_str())
        .find(|s| MenuSlot::parse(s).is_none())
}

/// 报错里列「可用」时用的那几个名字——从 [`MenuSlot`] 现拼，不写死字符串，
/// 否则加一个新槽位时这句提示会先骗人。
const SLOT_NAMES: [&str; 4] = [
    MenuSlot::Palette.as_str(),
    MenuSlot::ContextFile.as_str(),
    MenuSlot::ContextBlank.as_str(),
    MenuSlot::Sidebar.as_str(),
];

/// 「写了个认不出的界面名」那句错误。命令与工作流共用一条措辞（两处各写一份，
/// 迟早一份说三种、另一份说两种）。`what` 传「命令」或「工作流」。
pub fn slot_error(what: &str, name: &str, raw: &[String]) -> Option<String> {
    let bad = first_bad_slot(raw)?;
    Some(format!(
        "{what}「{name}」的 menu 里有不认识的界面「{bad}」（可用：{}）",
        SLOT_NAMES.join(" / ")
    ))
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
    /// 这条命令出现在哪些界面（空 = 只进命令面板）。见 [`MenuSlot`]。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu: Vec<String>,
    /// 这条命令的组合键，写成 `cmd+shift+w`（空 = 不绑）。
    ///
    /// 为什么挂在命令自己身上、而不是设计稿（devlog §3）里那个独立的 `keybindings`
    /// 数组：数组要靠 `<ext-id>.<name>` 当外键，而那是**展示名**——摊平时会被加上
    /// 「扩展名 · 」前缀，打错一个字母就是「写了没反应，也不知道为什么」。与 `menu`
    /// 同一取舍（devlog §4.6）。
    ///
    /// 键串**在这里只存原样**，解析发生在 `mo_ui::keys`（那里是它被按下的地方，
    /// 也要按平台折主修饰键）。所以这一侧不做「写法对不对」的校验：坏键串在键表
    /// 构建时整条跳过并告警，与「坏清单只跳过这一条」同一尺度。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
}

impl UserCommand {
    /// 投递落点（空 / 全写错 = 只进命令面板）。
    pub fn slots(&self) -> Vec<MenuSlot> {
        slots_of(&self.menu)
    }

    /// 这条命令有没有落到某个界面上。
    ///
    /// 侧栏那一行与「这条投到哪个界面」问的是同一句，所以判据只在这里一份
    /// （`mo_ui::actions` 建注册表时读的是 [`Self::slots`]，两处同一个来源）。
    pub fn goes_to(&self, slot: MenuSlot) -> bool {
        self.slots().contains(&slot)
    }

    /// 归在哪一组（命令面板的分组名 = 侧栏的分区名，同一个问题一份答案）。
    pub fn group(&self) -> &str {
        if self.category.trim().is_empty() {
            "自定义"
        } else {
            self.category.trim()
        }
    }

    /// 这条命令有没有声明组合键（写成一串空白也算没写）。
    pub fn has_chord(&self) -> bool {
        !self.key.trim().is_empty()
    }
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
    /// 这个工作流出现在哪些界面（空 = 只进命令面板）。见 [`MenuSlot`]。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu: Vec<String>,
    /// 组合键，语义与 [`UserCommand::key`] 同（工作流不受 `when_ext` 约束，所以绑键
    /// 没有那条限制）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
}

impl Workflow {
    /// 投递落点，语义与 [`UserCommand::slots`] 同。
    pub fn slots(&self) -> Vec<MenuSlot> {
        slots_of(&self.menu)
    }

    /// 落到某个界面上没有，语义与 [`UserCommand::goes_to`] 同。
    pub fn goes_to(&self, slot: MenuSlot) -> bool {
        self.slots().contains(&slot)
    }

    /// 工作流恒归「工作流」一组（没有 `category` 字段可写：一条工作流就是一串命令，
    /// 分组留给命令那边管）。侧栏的分区名与命令面板同一个来源，见
    /// [`UserCommand::group`]。
    pub fn group(&self) -> &'static str {
        "工作流"
    }

    /// 有没有声明组合键，语义与 [`UserCommand::has_chord`] 同。
    pub fn has_chord(&self) -> bool {
        !self.key.trim().is_empty()
    }
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
    /// 网格 / 画廊里图标（连同单元几何）的缩放倍率，`1.0` = 原始大小。
    ///
    /// 只作用于**网格 / 画廊**：列表与列视图的行高是固定 24pt
    /// （`listing::row_height`），放大图标会牵动整行布局，不在这一档里做。
    /// 写进配置的坏值（NaN / 0 / 10）由 [`clamp_icon_scale`] 收口，不让它把布局算成负数。
    #[serde(default = "default_icon_scale")]
    pub icon_scale: f32,
    /// 列表视图的分组方式：`none` / `kind` / `date`（新标签页的默认值）。
    ///
    /// 存稳定键名而不是中文标签（同 `view_mode` 的约定）；键的合法性与
    /// 归属在 mo-core 的 `Grouping::from_key` 里收口，坏值回落「不分组」。
    #[serde(default = "default_grouping")]
    pub group: String,
}

/// 图标缩放的档位边界与步长（网格 / 画廊）。
///
/// 下限不是随便定的：单元里的文件名 / 大小两行文字不参与缩放，方框缩到 27pt
/// 以下时格子会被文字撑破（`grid::cell` 是 `justify_center`，撑破的表现是
/// 文字溢出到相邻格）。上限 2× 是「画廊里 96pt 方框放到 192pt 还看得见一张缩略图」
/// 的尺度，再大就没意义了，只是把可视条数压到个位数。
pub const ICON_SCALE_MIN: f32 = 0.75;
/// 见 [`ICON_SCALE_MIN`]。
pub const ICON_SCALE_MAX: f32 = 2.0;
/// 一次 ⌘+ / ⌘- 走过的步长。
pub const ICON_SCALE_STEP: f32 = 0.25;

/// 把任意倍率收进合法档位，并量化到 0.01——步进是浮点加法，
/// `0.75 + 0.25 + 0.25` 会得到 `1.2499999`，写进配置就成了脏数据。
pub fn clamp_icon_scale(v: f32) -> f32 {
    // 只有 NaN 无从解释（回落默认）；±∞ 语义明确（太大 / 太小），交给 clamp 顶到边界。
    if v.is_nan() {
        return default_icon_scale();
    }
    let clamped = v.clamp(ICON_SCALE_MIN, ICON_SCALE_MAX);
    (clamped * 100.0).round() / 100.0
}

fn default_icon_scale() -> f32 {
    1.0
}

fn truthy() -> bool {
    true
}

fn default_view_mode() -> String {
    "list".to_string()
}

fn default_grouping() -> String {
    "none".to_string()
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            sidebar: true,
            status_bar: true,
            zebra: true,
            view_mode: default_view_mode(),
            icon_scale: default_icon_scale(),
            group: default_grouping(),
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
            remote_servers: Vec::new(),
            extension_settings: HashMap::new(),
            index_exclude: default_index_exclude(),
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
        assert_eq!(cfg.ui.icon_scale, 1.0, "缺字段走默认值");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话快照：存进去再读出来必须一样（含「远程标签页记端点」这一条）。
    #[test]
    fn session_round_trips() {
        let dir = std::env::temp_dir().join(format!("mo-session-rt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("session.json");
        let _ = std::fs::remove_file(&path);

        // 文件不存在 → 空会话（首次启动不该被它绊住）。
        assert!(Session::load(&path).is_empty());

        let s = Session {
            panes: vec![
                vec![SavedTab {
                    path: Some("/Users/me/文稿".to_string()),
                    endpoint: None,
                    remote_path: None,
                }],
                vec![SavedTab {
                    path: None,
                    endpoint: Some("sftp://host:2222".to_string()),
                    remote_path: Some("/pub/incoming".to_string()),
                }],
            ],
            active_tabs: vec![0, 0],
            active_pane: 1,
            split: true,
            ..Session::default()
        };
        s.save(&path).expect("写得出");
        let back = Session::load(&path);
        assert_eq!(back, s, "往返要一致");
        assert!(!back.is_empty());

        // 坏内容不该让启动挂掉：读不出来就当没有（回到默认那一页）。
        std::fs::write(&path, "{ not json").expect("写得出");
        assert!(Session::load(&path).is_empty(), "坏文件回落空会话");

        // 有窗格但里面一个标签页都没有，也算空。
        let blank = Session {
            panes: vec![Vec::new()],
            ..Session::default()
        };
        assert!(blank.is_empty());

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

    /// 缩放倍率是用户手改得到的字段，坏值必须在**读的时候**收口。
    ///
    /// 界面上那两个按钮不会写出坏值，但 config.json 是给人改的：`0` / `999` /
    /// `"abc"`（反序列化失败走默认）都会进来。不收口的话，`0` 会让方框边长算成 0
    /// （图标消失），`NaN` 会污染后面所有几何计算，怎么算都是 NaN。
    #[test]
    fn icon_scale_is_clamped_and_quantized() {
        assert_eq!(clamp_icon_scale(1.0), 1.0);
        assert_eq!(clamp_icon_scale(0.0), ICON_SCALE_MIN);
        assert_eq!(clamp_icon_scale(-3.0), ICON_SCALE_MIN);
        assert_eq!(clamp_icon_scale(99.0), ICON_SCALE_MAX);
        assert_eq!(clamp_icon_scale(f32::NAN), 1.0, "NaN 回落默认");
        assert_eq!(clamp_icon_scale(f32::INFINITY), ICON_SCALE_MAX);
        // 浮点步进的脏值量化到 0.01：0.75 + 0.25 + 0.25 = 1.2499999…
        assert_eq!(clamp_icon_scale(0.75 + 0.25 + 0.25), 1.25);
    }

    /// 写坏值的配置能读出来（而不是整份配置回落默认），坏值由 `clamp_icon_scale` 兜。
    #[test]
    fn bogus_icon_scale_does_not_kill_the_whole_config() {
        let dir = std::env::temp_dir().join(format!("mo-config-zoom-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"theme":"dark","ui":{"icon_scale":0.0,"zebra":false}}"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.theme, "dark", "坏字段不该拖垮整份配置");
        assert!(!cfg.ui.zebra, "同级的正常字段照常生效");
        assert_eq!(clamp_icon_scale(cfg.ui.icon_scale), ICON_SCALE_MIN);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 界面声明的读法：大小写 / 空白 / 冒号与连字符两种写法都收，认不出的报出来。
    #[test]
    fn menu_slots_are_parsed_leniently() {
        assert_eq!(MenuSlot::parse("palette"), Some(MenuSlot::Palette));
        assert_eq!(
            MenuSlot::parse("  Context:FILE "),
            Some(MenuSlot::ContextFile)
        );
        assert_eq!(
            MenuSlot::parse("context-blank"),
            Some(MenuSlot::ContextBlank)
        );
        assert_eq!(MenuSlot::parse("context:flle"), None);
        assert_eq!(MenuSlot::parse(""), None);
        // 规范写法回写用（配置里出现的应该是这一种）。
        assert_eq!(MenuSlot::ContextFile.as_str(), "context:file");
        // 侧栏（P2-5）三种写法都收，配置里手改时不必记哪种是官方的。
        for raw in ["sidebar", "side-bar", "side:bar", " SIDE-BAR "] {
            assert_eq!(MenuSlot::parse(raw), Some(MenuSlot::Sidebar), "{raw}");
        }
    }

    /// 报错里那份「可用的界面名」是从枚举现拼的（`SLOT_NAMES`），但那个数组是**手写**
    /// 的：将来加第五个槽位时，忘了加进去不会编译失败，只会让提示骗人。这一条钉住
    /// 四槽齐全，以及「没写的槽位不该混进来」。
    #[test]
    fn every_slot_is_listed_in_the_error() {
        let s = |v: &[&str]| -> Vec<String> { v.iter().map(|x| x.to_string()).collect() };
        let err = slot_error("命令", "统计", &s(&["sideabr"])).expect("该报错");
        for name in ["palette", "context:file", "context:blank", "sidebar"] {
            assert!(err.contains(name), "{err} 漏了 {name}");
        }
    }

    /// 空列表 = 「用户没想过这件事」，落回只进面板；全写错也一样（不是一条都不投，
    /// 那会让命令凭空消失）。真要报错的是 `first_bad_slot`。
    #[test]
    fn empty_or_unrecognized_falls_back_to_palette() {
        let s = |v: &[&str]| -> Vec<String> { v.iter().map(|x| x.to_string()).collect() };
        assert_eq!(slots_of(&s(&[])), MenuSlot::defaults());
        assert_eq!(slots_of(&s(&["nope"])), MenuSlot::defaults());
        assert_eq!(
            slots_of(&s(&["context:blank", "palette"])),
            vec![MenuSlot::ContextBlank, MenuSlot::Palette],
            "顺序按声明来，不重排"
        );
        assert_eq!(first_bad_slot(&s(&["palette", "context:file"])), None);
        assert_eq!(first_bad_slot(&s(&["palette", "palett"])), Some("palett"));
        let err = slot_error("命令", "统计", &s(&["palett"])).expect("该报错");
        assert!(err.contains("统计") && err.contains("palett"), "{err}");
        assert!(err.contains("context:blank"), "{err} 要列出可用的写法");
    }

    /// `menu` / `key` 为空时**不该**出现在写出的配置里（用户没声明就别往 config.json 里
    /// 塞噪音），缺字段读回来也必须是空。
    #[test]
    fn empty_menu_is_not_serialized() {
        let mut c = UserCommand {
            name: "统计".into(),
            category: String::new(),
            shell: "wc -l {file}".into(),
            source: None,
            menu: Vec::new(),
            key: String::new(),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("menu"), "{json}");
        assert!(!json.contains("key"), "{json}");
        let back: UserCommand = serde_json::from_str(&json).unwrap();
        assert!(back.menu.is_empty());
        assert!(!back.has_chord(), "没写 key = 没绑键");
        assert_eq!(back.slots(), MenuSlot::defaults());

        c.menu = vec!["context:file".into()];
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""menu":["context:file"]"#), "{json}");
        let back: UserCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(back.slots(), vec![MenuSlot::ContextFile]);
        // 读写同一份声明来回一致（回写配置时不该把用户的写法悄悄换掉）。
        assert_eq!(back.menu, c.menu);

        c.key = "cmd+shift+w".into();
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""key":"cmd+shift+w""#), "{json}");
        let back: UserCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(back.key, "cmd+shift+w", "键串必须原样往返");
        assert!(back.has_chord());
        // 一串空白不是「绑了键」——写错的人要看到的是没绑，而不是一条永远按不出来的绑定。
        let blank: UserCommand =
            serde_json::from_str(r#"{"name":"n","shell":"pwd","key":"   "}"#).unwrap();
        assert!(!blank.has_chord());
    }
}
