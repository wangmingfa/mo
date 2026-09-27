//! 扩展系统：一个扩展 = 一个带 `manifest.json` 的目录。
//!
//! ## 为什么是「声明式清单 + 外部程序」而不是动态加载代码
//!
//! Rust 没有稳定的 ABI，往一个已经发布出去的进程里塞 `.dll` / `.dylib` 当插件
//! 要么依赖不稳定的 `#[rustc_private]`、要么要求用户用与主程序**逐字节一致**的
//! 工具链重新编译——两条路在真实项目里都不可维护，而且等于把主进程的内存安全
//! 交给任意第三方二进制。
//!
//! 所以这里的「插件」边界定在：**清单声明命令，命令跑外部程序**。外部程序可以是
//! 任何语言写的可执行文件，进程隔离、崩溃不连坐、也不需要匹配工具链。这与
//! VS Code 早期、Total Commander 插件之外的主流做法一致，也是本阶段能做到的
//! 「完整实现」的定义：加载、校验、启停、命名空间、条件生效、错误可见。
//!
//! ## 目录布局
//!
//! ```text
//! <配置目录>/mo/extensions/<id>/manifest.json
//! ```
//!
//! 清单只从**用户自己的配置目录**读，绝不扫描正在浏览的目录：否则「打开别人给的
//! 文件夹」就等于装了它带来的扩展。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mo_config::UserCommand;
use serde::{Deserialize, Serialize};

/// 清单 `types` 的一条：这几个扩展名在「种类」这一问上叫什么。
///
/// 只收 `label`，**不收**设计稿（devlog §3）里的 `group` / `icon`：那两个要分别动
/// 分组那条排序路径与图标 atlas，而这里的规矩是「收一个字段就投一个字段」——不收
/// 「解析了却没人读」的字段（那种字段的代价是作者写了、界面上没有，还没人告诉他）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeRule {
    /// 扩展名，写成 `.srt`（与 `when_ext` 同形；大小写不敏感，点可省）。
    pub ext: Vec<String>,
    /// 「种类」列的文案，如「字幕」。
    pub label: String,
}

/// 摊平后的「扩展名 → 种类文案」表。键是小写、不含点的扩展名，与
/// `mo_core::types` 那三问同一条判据。
pub type TypeLabels = BTreeMap<String, String>;

/// [`AppState::type_labels`](crate::AppState::type_labels) 那份缓存的形状：
/// `(上次读到的清单签名, 表)`，`None` = 还没读过。
///
/// 起了名字是因为 `Arc<Mutex<Option<(u64, Arc<TypeLabels>)>>>` 这种嵌套 clippy 会说
/// 「太复杂」，而它确实到了该有个名字的厚度。
pub type TypeLabelCache = Option<(u64, std::sync::Arc<TypeLabels>)>;

/// 一个扩展的清单。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// 扩展 id（同时是命名空间前缀，只允许 `[a-z0-9_-]`）。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 版本号（仅展示与记录用）。
    #[serde(default)]
    pub version: String,
    /// 是否启用（缺省启用；关掉后其命令整体消失）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 扩展提供的命令。
    #[serde(default)]
    pub commands: Vec<UserCommand>,
    /// 仅当选中项的扩展名命中这里时才显示这些命令（空 = 始终显示）。
    #[serde(default)]
    pub when_ext: Vec<String>,
    /// 扩展带的工作流（多步命令顺序执行）。
    #[serde(default)]
    pub workflows: Vec<mo_config::Workflow>,
    /// 扩展贡献的「类型知识」：这些扩展名在「种类」列上叫什么。
    #[serde(default)]
    pub types: Vec<TypeRule>,
}

fn default_true() -> bool {
    true
}

/// 已加载的一个扩展及其来源目录。
#[derive(Debug, Clone)]
pub struct Extension {
    pub manifest: Manifest,
    /// 清单文件路径（错误提示与「打开目录」用）。
    pub path: PathBuf,
}

/// id 合法性：要做命名空间前缀，必须能安全地拼进展示名与文件路径。
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// 扩展名规范化：`".SRT"` / `"srt "` → `Some("srt")`；认不出返回 `None`。
///
/// 判据与 `mo_core::types` 那三问同一条（小写、不含点、按 `Path::extension()` 的
/// 口径就是**最后一段**）。所以 `tar.gz` 在这里必然被拒：清单写它，实际命中的却是
/// `gz`，「写了不生效」比「写不出」难查得多。
fn norm_ext(raw: &str) -> Option<String> {
    let s = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(s)
}

/// 校验一份清单的 `types`；返回错误描述（`None` = 合法）。
fn validate_types(m: &Manifest) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for t in &m.types {
        if t.ext.is_empty() {
            return Some(format!(
                "扩展「{}」的 types 条目「{}」没写 ext",
                m.id, t.label
            ));
        }
        if t.label.trim().is_empty() {
            return Some(format!(
                "扩展「{}」的 types 条目（.{}）没写 label",
                m.id,
                t.ext.first().map(|s| s.as_str()).unwrap_or("?")
            ));
        }
        for raw in &t.ext {
            let Some(e) = norm_ext(raw) else {
                return Some(format!(
                    "扩展「{}」的 types 写了认不出的扩展名「{raw}」（只认 .srt 这样的一段，双后缀如 .tar.gz 不支持）",
                    m.id
                ));
            };
            if seen.contains(&e) {
                return Some(format!(
                    "扩展「{}」的 types 里「.{e}」被两条规则同时认领（谁赢取决于数组顺序，界面上又只显示一条）",
                    m.id
                ));
            }
            seen.push(e);
        }
    }
    None
}

/// 校验清单；返回错误描述（`None` = 合法）。
pub fn validate(m: &Manifest, expect_id: Option<&str>) -> Option<String> {
    if !valid_id(&m.id) {
        return Some(format!(
            "id「{}」不合法（只允许小写字母、数字、_、-）",
            m.id
        ));
    }
    if let Some(want) = expect_id {
        // 清单放在 `<id>/` 目录下，里面的 id 必须与目录名一致，否则启停状态
        // （按目录名记录）会与扩展对不上号。
        if m.id != want {
            return Some(format!("清单里的 id「{}」与所在目录「{want}」不一致", m.id));
        }
    }
    if m.name.trim().is_empty() {
        return Some(format!("扩展「{}」缺少 name", m.id));
    }
    for c in &m.commands {
        if let Some(err) = crate::usercmds::validate(c) {
            return Some(format!("扩展「{}」的命令有问题：{err}", m.id));
        }
    }
    // 绑了快捷键的命令不能受 `when_ext` 约束。理由不是「实现麻烦」而是语义：组合键
    // 按下时**没有「本次右键的目标」这个上下文**，键表也不按选区过滤——绑上去就等于
    // 「选中 .txt 时按 ⌘⇧W 也会去数字幕」。与其在按下时再判一次选区（那才有两处判据、
    // 于是「有时生效有时不生效」），不如在加载时就拒掉。
    //
    // 侧栏那一行同理（P2-5）：侧栏是全局的，它不属于任何一个条目的上下文。而这一条
    // 不拦住更糟——投递给侧栏的命令来自「不受 `when_ext` 约束的那一批」（
    // `AppState::user_commands(&[])`），所以受约束的命令即使写了 `menu: ["sidebar"]`
    // 也永远到不了侧栏，界面上连一句「这条没生效」都没有。
    if !m.when_ext.is_empty() {
        if let Some(c) = m.commands.iter().find(|c| c.has_chord()) {
            return Some(format!(
                "扩展「{}」写了 when_ext，命令「{}」却绑了快捷键「{}」（快捷键不看选区；要绑键就把 when_ext 去掉，让这条命令一直可见）",
                m.id, c.name, c.key
            ));
        }
        if let Some(c) = m
            .commands
            .iter()
            .find(|c| c.goes_to(mo_config::MenuSlot::Sidebar))
        {
            return Some(format!(
                "扩展「{}」写了 when_ext，命令「{}」却投给了侧栏 sidebar（侧栏是全局的，没有「本次目标」这个上下文，这一行永远不会出现；要在侧栏里看到它就把 when_ext 去掉）",
                m.id, c.name
            ));
        }
    }
    if let Some(err) = validate_types(m) {
        return Some(err);
    }
    // 同名命令会让命令面板出现两条无法区分的条目。
    let mut seen: Vec<&str> = Vec::new();
    for c in &m.commands {
        if seen.contains(&c.name.as_str()) {
            return Some(format!("扩展「{}」里有重名命令「{}」", m.id, c.name));
        }
        seen.push(&c.name);
    }
    None
}

/// 扩展根目录（`<配置目录>/mo/extensions`）。
pub fn extensions_root(config_json: &Path) -> PathBuf {
    config_json
        .parent()
        .map(|p| p.join("extensions"))
        .unwrap_or_else(|| PathBuf::from("extensions"))
}

/// 扫描扩展目录。
///
/// 单个扩展坏了只跳过它并告警——一个写错的清单不该让其它扩展一起消失。
pub fn load(root: &Path) -> Vec<Extension> {
    load_report(root).0
}

/// [`load`] 的完整版：**坏掉的清单也带回来**（P2-7）。
///
/// 之前坏了只剩一句 `tracing::warn!`，扩展管理器上什么都不显示——手写清单的人对着
/// 空面板连「错在哪」都问不出。调用方（扩展管理器）要把这一批亮出来；其余调用方
/// （键表 / 类型表 / 侧栏）只关心能用的那份，继续走 [`load`]。
pub fn load_report(root: &Path) -> (Vec<Extension>, Vec<BrokenExtension>) {
    let mut good = Vec::new();
    let mut broken = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return (good, broken);
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let manifest = d.join("manifest.json");
        if !manifest.is_file() {
            continue; // 不是扩展目录，安静跳过
        }
        let text = match std::fs::read_to_string(&manifest) {
            Ok(t) => t,
            Err(e) => {
                let reason = format!("清单读不出来：{e}");
                tracing::warn!("扩展清单 {}：{reason}", manifest.display());
                broken.push(BrokenExtension {
                    path: manifest,
                    reason,
                });
                continue;
            }
        };
        let m: Manifest = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                let reason = format!("清单不是合法 JSON：{e}");
                tracing::warn!("扩展清单 {}：{reason}", manifest.display());
                broken.push(BrokenExtension {
                    path: manifest,
                    reason,
                });
                continue;
            }
        };
        let dir_name = d
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(err) = validate(&m, Some(&dir_name)) {
            tracing::warn!("扩展 {} 被跳过：{err}", manifest.display());
            broken.push(BrokenExtension {
                path: manifest,
                reason: err,
            });
            continue;
        }
        good.push(Extension {
            manifest: m,
            path: manifest,
        });
    }
    (good, broken)
}

/// 读不了或校验不过的一份清单。扩展管理器把这一批**原样**亮出来——判据（什么算坏、
/// 错在哪）在 [`validate`]，这里只搬运，不在 UI 里再判一遍。
#[derive(Debug, Clone, PartialEq)]
pub struct BrokenExtension {
    /// 清单文件路径。
    pub path: PathBuf,
    /// 为什么没用上（`validate` 的报错原句，或 IO / JSON 错误）。
    pub reason: String,
}

/// 把扩展的命令摊平成用户命令：展示名带扩展前缀，分类默认用扩展名。
///
/// `selected_exts` 是当前选中项的扩展名集合（小写含点）。清单写了 `when_ext`
/// 时，只有命中才给出命令——「针对 .md 的命令」不该在选中视频时出现。
pub fn flatten(exts: &[Extension], selected_exts: &[String]) -> Vec<UserCommand> {
    let mut out = Vec::new();
    for e in exts {
        let m = &e.manifest;
        if !m.enabled {
            continue;
        }
        for c in &m.commands {
            if !m.when_ext.is_empty() {
                let hit = m
                    .when_ext
                    .iter()
                    .any(|w| selected_exts.iter().any(|s| s == w));
                if !hit {
                    continue;
                }
            }
            let mut c = c.clone();
            c.name = display_name(&m.name, &c.name);
            if c.category.trim().is_empty() {
                c.category = m.name.clone();
            }
            c.source = Some(e.path.display().to_string());
            out.push(c);
        }
    }
    out
}

/// 摊平后的展示名：`扩展名 · 命令名`。**唯一一份拼接**。
///
/// 界面上有好几处要报出这个名字：命令面板、右键菜单、侧栏行 ID（P2-5）、扩展管理器的
/// 贡献清单与启用确认卡（P2-6）。各处各拼一次的话，「清单里写的」与「界面上显示的」
/// 迟早分叉，而分叉的代价是用户按确认卡找不过来。
pub fn display_name(ext: &str, cmd: &str) -> String {
    format!("{ext} · {cmd}")
}

/// 一个扩展「启用之后会改动界面上的什么」，一条一句。给扩展管理器的展开区与
/// 启用确认卡用（devlog §6 的「权限确认」在声明层的具体形态）。
///
/// 为什么不让 UI 直接遍历清单：这几句答案各有一处**已经定下来**的加工规则——命令名要
/// 摊平（[`display_name`]）、落点要过 [`mo_config`] 的宽容解析（`slots()`）、组合键要判
/// 「空白串不算绑定」（`has_chord()`）。UI 再走一遍就是两处作答，而这张表的用途恰恰是
/// **对用户说真话**：说错一句比不说更糟。
///
/// ⚠️ 里面**没有** §6 草案那个 `capabilities` 字段：它在本轮没有强制判据（声明层没有
/// 任何一处按它放行或拦下），按 §4.7「收一个字段就投一个字段」的规矩，不收「解析了却
/// 没人读」的东西。这里列的是清单**实际能改的东西**，不是作者自报的意向。
#[derive(Debug, Clone, PartialEq)]
pub enum Contribution {
    /// 一条命令：界面上的名字、出现在哪些界面、绑的键（写了才算）、它要跑的命令行。
    Command {
        label: String,
        slots: Vec<mo_config::MenuSlot>,
        chord: Option<String>,
        shell: String,
    },
    /// 一个工作流：语义同 [`Self::Command`]，但「要跑什么」是一串步骤。名字**不带**扩展
    /// 前缀——与 `AppState::workflows` 的现状一致（不对称记在 devlog §4.10 的缺口里，
    /// 这张表跟着界面走，不跟着草案走；界面说什么是什么是）。
    Workflow {
        label: String,
        slots: Vec<mo_config::MenuSlot>,
        chord: Option<String>,
        steps: Vec<String>,
    },
    /// 一条类型知识：这些扩展名在「种类」列上会被改叫什么（P2-3 那条投递）。
    TypeLabel { exts: Vec<String>, label: String },
}

/// 把一份清单摊成「它会改动界面上的哪些地方」。
///
/// 顺序恒为 命令 → 工作流 → 类型标签（与命令面板里那批贡献项同一口径：命令在前）。
/// 不看 `enabled`：停用中的扩展也要能预览「启用后会发生什么」——这正是确认卡的用法。
pub fn contributions(m: &Manifest) -> Vec<Contribution> {
    let mut out: Vec<Contribution> = m
        .commands
        .iter()
        .map(|c| Contribution::Command {
            label: display_name(&m.name, &c.name),
            slots: c.slots(),
            chord: c.has_chord().then(|| c.key.trim().to_string()),
            shell: c.shell.clone(),
        })
        .collect();
    out.extend(m.workflows.iter().map(|w| Contribution::Workflow {
        label: w.name.clone(),
        slots: w.slots(),
        chord: w.has_chord().then(|| w.key.trim().to_string()),
        steps: w.steps.clone(),
    }));
    out.extend(m.types.iter().filter_map(|t| {
        // 报的是**实际会命中的**那串扩展名（小写、含点），与 `type_labels` 查表的键同一
        // 判据；清单里写成 `.SRT` 也报成 `.srt`，免得确认卡上出现一条永远不会命中的名字。
        let exts: Vec<String> = t
            .ext
            .iter()
            .filter_map(|raw| norm_ext(raw).map(|e| format!(".{e}")))
            .collect();
        if exts.is_empty() {
            return None;
        }
        Some(Contribution::TypeLabel {
            exts,
            label: t.label.clone(),
        })
    }));
    out
}

/// 把各清单的 `types` 摊成「扩展名 → 种类文案」。
///
/// 两条与 [`flatten`] 同源的规矩：
/// * 关掉的扩展整体消失（它的命令消失，类型标签也该消失——留着就是「停用了还在改
///   我的显示」）；
/// * 两个扩展抢同一个扩展名时**先到先得**。[`load`] 按目录名排过序，所以「谁先」
///   是确定的；后到的那条告警一句，作者至少知道自己是输的那一个。
pub fn type_labels(exts: &[Extension]) -> TypeLabels {
    let mut out = TypeLabels::new();
    for e in exts {
        let m = &e.manifest;
        if !m.enabled {
            continue;
        }
        for t in &m.types {
            for raw in &t.ext {
                // `validate` 已经拦过认不出的写法；这里不 panic，只是不收。
                let Some(key) = norm_ext(raw) else {
                    continue;
                };
                if out.contains_key(&key) {
                    tracing::warn!(
                        "扩展「{}」的类型标签「.{}」已被另一个扩展认领，忽略这一条",
                        m.id,
                        key
                    );
                    continue;
                }
                out.insert(key, t.label.clone());
            }
        }
    }
    out
}

/// 清单目录的「有没有人动过」签名：扩展的个数与目录名，加上每份清单的修改时间与长度。
///
/// 存在的意义是省掉一种 IO：列表每帧每行都要问「这个后缀叫什么」，而清单在磁盘上
/// （见 `AppState::type_labels` 那条缓存）。用签名而不是 TTL 作废，是为了让用户手改
/// 清单**下一帧就生效**，不必重启。
///
/// ⚠️ 两个已知盲区，都是「签名字节没变但内容变了」：同一时间戳里改成同样长度的内容
/// （修改时间精度为秒的文件系统上理论可能，NTFS/APFS 是亚微秒），以及清单内容被
/// 换成同长度同 mtime 的另一份。真撞上就是把标签缓住了，重开一次 Mo 即好。
pub fn fingerprint(root: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            names.push(
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            );
        }
    }
    names.sort();
    names.len().hash(&mut h);
    for n in names {
        n.hash(&mut h);
        let manifest = root.join(&n).join("manifest.json");
        match std::fs::metadata(&manifest) {
            Ok(md) => {
                md.modified().ok().hash(&mut h);
                md.len().hash(&mut h);
            }
            Err(_) => None::<std::time::SystemTime>.hash(&mut h),
        }
    }
    h.finish()
}

/// 一条侧栏项背后的动作。
///
/// 与 [`crate::actions`] 那边同理：**带的是声明本身**，不是「第几条命令」的下标。
/// 侧栏每帧都在画，而下标参照的那两份 vec 每帧重取（配置里改一条、扩展启停一次都会
/// 让下标飘），点 A 跑出 B 就是这么来的。
#[derive(Debug, Clone, PartialEq)]
pub enum SidebarEntry {
    Command(UserCommand),
    Workflow(mo_config::Workflow),
}

impl SidebarEntry {
    /// 侧栏那一行显示的文案（扩展命令已带「扩展名 · 」前缀，见 [`flatten`]）。
    pub fn label(&self) -> &str {
        match self {
            Self::Command(c) => &c.name,
            Self::Workflow(w) => &w.name,
        }
    }
    /// 落在侧栏的哪个分区（判据与命令面板的分组名同一句，见 `UserCommand::group`）。
    pub fn group(&self) -> String {
        match self {
            Self::Command(c) => c.group().to_string(),
            Self::Workflow(w) => w.group().to_string(),
        }
    }
}

/// 从「这一批命令 + 这一批工作流」里挑出投给侧栏的那些。
///
/// 纯函数、不碰磁盘，所以它可以被单测直接喂数据，也可以被
/// [`AppState::sidebar_entries`](crate::AppState::sidebar_entries) 缓存起来复用。
/// 顺序：命令在前、工作流在后，与命令面板那张注册表（`mo_ui::actions::contributed`）
/// 同一条顺序（界面在两处的同一批动作，相对顺序应当一致，否则「第三行」在两处指不同
/// 的东西）。
pub fn sidebar_entries_of(
    commands: &[UserCommand],
    workflows: &[mo_config::Workflow],
) -> Vec<SidebarEntry> {
    let mut out: Vec<SidebarEntry> = commands
        .iter()
        .filter(|c| c.goes_to(mo_config::MenuSlot::Sidebar))
        .cloned()
        .map(SidebarEntry::Command)
        .collect();
    out.extend(
        workflows
            .iter()
            .filter(|w| w.goes_to(mo_config::MenuSlot::Sidebar))
            .cloned()
            .map(SidebarEntry::Workflow),
    );
    out
}

/// [`AppState::sidebar_entries`](crate::AppState::sidebar_entries) 那份缓存的形状：
/// `(上次读到的声明签名, 列表)`，`None` = 还没读过。
pub type SidebarCache = Option<(u64, std::sync::Arc<Vec<SidebarEntry>>)>;

/// 把一份文件的「有没有被人改过」摘要进哈希器（修改时间 + 长度）。
///
/// 读不到（还没建、或正被人删）也喂一个确定性的哨兵值：否则「文件没了」与「没这个
/// 文件路径」在签名上撞成同一个数，缓存就作废不掉。
fn hash_meta(h: &mut impl std::hash::Hasher, path: &Path) {
    use std::hash::Hash;
    match std::fs::metadata(path) {
        Ok(md) => {
            md.modified().ok().hash(h);
            md.len().hash(h);
        }
        Err(_) => None::<std::time::SystemTime>.hash(h),
    }
}

/// 「自定义动作的声明」整体签名：配置里的 `commands` + `commands/*.json` + 扩展清单。
///
/// 为什么不像 [`fingerprint`] 那样只签扩展目录：侧栏项的来源是那**三处**，只签一处
/// 的话用户在手改 `config.json` 加了一条 `menu: ["sidebar"]` 之后侧栏不动，而这里
/// 能不动的原因是「读一次」的代价是三条声明来源全都要重新读盘解析。
///
/// ⚠️ `config.json` 的修改时间会随**任何**一次设置改动而变（列偏好、开关都会落盘），
/// 所以这一份签名比 [`fingerprint`] 容易失效。那是安全方向的失效：多读一次盘，
/// 而不是缓住了用户的声明。
pub fn declarations_fingerprint(config_json: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_meta(&mut h, config_json);
    let mut cmds: Vec<PathBuf> = Vec::new();
    if let Some(dir) = config_json.parent().map(|p| p.join("commands")) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            cmds = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().map(|x| x == "json").unwrap_or(false))
                .collect();
        }
    }
    cmds.sort();
    cmds.len().hash(&mut h);
    for p in cmds {
        hash_meta(&mut h, &p);
    }
    fingerprint(&extensions_root(config_json)).hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: "测试扩展".to_string(),
            version: "1.0".to_string(),
            enabled: true,
            commands: vec![UserCommand {
                name: "统计".into(),
                category: String::new(),
                shell: "wc -l {file}".into(),
                source: None,
                menu: Vec::new(),
                key: String::new(),
            }],
            when_ext: Vec::new(),
            workflows: Vec::new(),
            types: Vec::new(),
        }
    }

    /// id 规则：要做命名空间与目录名，必须严格。
    #[test]
    fn ids_are_strict() {
        assert!(valid_id("word-count"));
        assert!(valid_id("md_tools2"));
        for bad in ["", "Word", "a b", "../etc", "中文"] {
            assert!(!valid_id(bad), "{bad} 不该被接受");
        }
    }

    /// 清单 id 必须与目录名一致，否则按目录记录的启停状态会错位。
    #[test]
    fn manifest_id_must_match_directory() {
        assert!(validate(&manifest("a"), Some("a")).is_none());
        let err = validate(&manifest("a"), Some("b")).expect("目录不一致应报错");
        assert!(err.contains("不一致"), "{err}");
    }

    /// 命令重名 / 缺字段都要被拒。
    #[test]
    fn rejects_duplicate_and_broken_commands() {
        let mut m = manifest("a");
        m.commands.push(m.commands[0].clone());
        assert!(validate(&m, Some("a")).is_some(), "重名命令应被拒");

        let mut m2 = manifest("a");
        m2.commands[0].shell = String::new();
        assert!(validate(&m2, Some("a")).is_some(), "空 shell 应被拒");
    }

    /// 界面声明写错 → 整个扩展被跳过（走的是命令级校验那条现成的路）。
    /// 判据与 `loads` 里其它坏清单一致：宁可这个扩展不出现，也不要一条「写了菜单
    /// 却没菜单」的命令留在列表里让人猜。
    #[test]
    fn rejects_unknown_menu_slot() {
        let mut m = manifest("a");
        assert!(validate(&m, Some("a")).is_none());
        m.commands[0].menu = vec!["context:flle".into()];
        let err = validate(&m, Some("a")).expect("认不出的界面名应被拒");
        assert!(err.contains("context:flle"), "{err}");
    }

    /// 摊平只改展示名 / 分类 / 来源，**不能**把投递声明弄丢——菜单里看不看得见
    /// 全靠它一路传到 `mo_ui::actions` 的那张注册表。
    #[test]
    fn menu_declaration_survives_flattening() {
        let mut m = manifest("a");
        m.commands[0].menu = vec!["palette".into(), "context-file".into()];
        let cmds = flatten(
            &[Extension {
                manifest: m,
                path: PathBuf::from("/x/a/manifest.json"),
            }],
            &[],
        );
        assert_eq!(
            cmds[0].menu,
            vec!["palette".to_string(), "context-file".to_string()]
        );
        assert_eq!(
            cmds[0].slots(),
            vec![
                mo_config::MenuSlot::Palette,
                mo_config::MenuSlot::ContextFile
            ]
        );
    }

    /// 「这个扩展启用后会改动界面上的什么」——启用确认卡要说的那几句全在这一条里。
    ///
    /// 名字必须与 [`flatten`] 给的一致（两处各拼一次迟早分叉，而这张表的用途是**对用户
    /// 说真话**，说错一句比不说更糟）；落点跟着 `menu` 的宽容解析走；类型标签报成
    /// **实际会命中的**那串扩展名（`.SRT` → `.srt`，否则确认卡上会出现一条永远不会
    /// 命中的名字）。
    #[test]
    fn contributions_answer_what_the_manifest_will_change() {
        let mut m = manifest("a");
        m.commands[0].menu = vec!["sidebar".to_string(), "context:file".to_string()];
        m.commands[0].key = "cmd+shift+w".into();
        m.workflows.push(mo_config::Workflow {
            name: "打包".into(),
            steps: vec!["tar -czf a.tgz {file}".into(), "echo done".into()],
            source: None,
            menu: vec!["palette".to_string()],
            // 一串空白 = 没绑键（判据在 `Workflow::has_chord`，这里跟着走）。
            key: "   ".into(),
        });
        m.types.push(TypeRule {
            ext: vec![".SRT".into(), "vtt".into()],
            label: "字幕".into(),
        });
        assert_eq!(
            contributions(&m),
            vec![
                Contribution::Command {
                    label: "测试扩展 · 统计".into(),
                    slots: vec![
                        mo_config::MenuSlot::Sidebar,
                        mo_config::MenuSlot::ContextFile
                    ],
                    chord: Some("cmd+shift+w".into()),
                    shell: "wc -l {file}".into(),
                },
                Contribution::Workflow {
                    label: "打包".into(),
                    slots: vec![mo_config::MenuSlot::Palette],
                    chord: None,
                    steps: vec!["tar -czf a.tgz {file}".into(), "echo done".into()],
                },
                Contribution::TypeLabel {
                    exts: vec![".srt".into(), ".vtt".into()],
                    label: "字幕".into(),
                },
            ],
            "顺序恒为命令 → 工作流 → 类型标签，名字与落点都跟着界面那套走"
        );
        // 确认卡上写的名字就是列表里出现的那个（同一个拼接函数，不是两份字面量）。
        let flat = flatten(
            &[Extension {
                manifest: m.clone(),
                path: PathBuf::from("/x/a/manifest.json"),
            }],
            &[],
        );
        let Contribution::Command { label, .. } = &contributions(&m)[0] else {
            panic!("第一条应当是命令");
        };
        assert_eq!(*label, flat[0].name);
        // 停用中的扩展照列——确认卡问的正是「启用它会发生什么」。
        m.enabled = false;
        assert_eq!(contributions(&m).len(), 3, "不看 enabled：停用的也要能预览");
    }

    /// 绑了快捷键、却又被 `when_ext` 约束的命令 → 整条清单不加载。
    ///
    /// 这一条钉的是「宁可不加载，也不要一条按得响但语义说不清的命令」：组合键按下时
    /// 没有「本次目标」这个上下文，让它受选区扩展名约束就会出现「同一颗键，选中 .md
    /// 时执行、选中 .txt 时也执行同一条」——那 `when_ext` 等于没写。
    #[test]
    fn rejects_chord_on_ext_gated_command() {
        let mut m = manifest("a");
        m.commands[0].key = "cmd+shift+w".into();
        assert!(
            validate(&m, Some("a")).is_none(),
            "没写 when_ext 时绑键是合法的"
        );
        m.when_ext = vec![".srt".into()];
        let err = validate(&m, Some("a")).expect("when_ext + key 应被拒");
        assert!(err.contains("统计"), "{err} 要指得出是哪条命令");
        assert!(err.contains("when_ext"), "{err} 要说出理由");
    }

    /// 摊平只改展示名 / 分类 / 来源，**不能**把 `key` 声明弄丢——键表读的是摊平之后的
    /// 那一份，这里掉了字段就是「清单写了快捷键，按下去没反应」（P2-4 的反向验证靶子）。
    #[test]
    fn key_declaration_survives_flattening() {
        let mut m = manifest("a");
        m.commands[0].key = "cmd+alt+shift+j".into();
        let cmds = flatten(
            &[Extension {
                manifest: m,
                path: PathBuf::from("/x/a/manifest.json"),
            }],
            &[],
        );
        assert_eq!(cmds[0].key, "cmd+alt+shift+j");
        assert!(cmds[0].has_chord());
    }

    /// 摊平：加前缀、补分类、记来源。
    #[test]
    fn flatten_prefixes_names() {
        let exts = vec![Extension {
            manifest: manifest("a"),
            path: PathBuf::from("/x/a/manifest.json"),
        }];
        let cmds = flatten(&exts, &[]);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "测试扩展 · 统计");
        assert_eq!(cmds[0].category, "测试扩展");
        assert!(cmds[0].source.is_some());
    }

    /// 禁用与条件生效。
    #[test]
    fn respects_enabled_and_when_ext() {
        let mut off = manifest("a");
        off.enabled = false;
        assert!(flatten(
            &[Extension {
                manifest: off,
                path: PathBuf::new()
            }],
            &[]
        )
        .is_empty());

        let mut cond = manifest("b");
        cond.when_ext = vec![".md".to_string()];
        let exts = vec![Extension {
            manifest: cond,
            path: PathBuf::new(),
        }];
        assert!(
            flatten(&exts, &[".txt".to_string()]).is_empty(),
            "扩展名不匹配应隐藏"
        );
        assert_eq!(flatten(&exts, &[".md".to_string()]).len(), 1);
    }

    /// 目录扫描：坏 JSON / 缺清单 / 非法 id 的目录都不影响其它扩展。
    #[test]
    fn loads_robustly() {
        let root = std::env::temp_dir().join(format!("mo-ext-{}", std::process::id()));
        let _ = std::fs::create_dir_all(root.join("good"));
        let _ = std::fs::create_dir_all(root.join("bad"));
        let _ = std::fs::create_dir_all(root.join("notanextension"));

        std::fs::write(
            root.join("good/manifest.json"),
            r#"{"id":"good","name":"好扩展","commands":[{"name":"跑一下","shell":"pwd"}]}"#,
        )
        .unwrap();
        std::fs::write(root.join("bad/manifest.json"), "{ 不是 JSON").unwrap();

        let exts = load(&root);
        assert_eq!(exts.len(), 1, "只应加载到合法的那个：{exts:?}");
        assert_eq!(exts[0].manifest.id, "good");
        // 缺省字段：enabled 默认 true，命令分类由扩展名补上。
        assert!(exts[0].manifest.enabled);
        assert_eq!(flatten(&exts, &[])[0].category, "好扩展");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 坏清单要**带原因**回来（P2-7）：扩展管理器要把「为什么没用上」亮出来，
    /// 而不能只留一句日志。三类坏法各验一种：JSON 解析失败、`validate` 拒收
    /// （这里用 id 与目录名不一致这例）、以及「目录里根本没有 manifest.json」
    /// 不算坏（那只是个普通目录，安静跳过的既有语义不变）。
    #[test]
    fn broken_manifests_come_back_with_a_reason() {
        let root = std::env::temp_dir().join(format!("mo-extbrk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["good", "badjson", "badid"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(
            root.join("good/manifest.json"),
            r#"{"id":"good","name":"好扩展"}"#,
        )
        .unwrap();
        std::fs::write(root.join("badjson/manifest.json"), "{ 不是 JSON").unwrap();
        std::fs::write(
            root.join("badid/manifest.json"),
            r#"{"id":"other-name","name":"对不上目录"}"#,
        )
        .unwrap();

        let (exts, broken) = load_report(&root);
        assert_eq!(exts.len(), 1, "能用的还是那一个：{exts:?}");
        assert_eq!(exts[0].manifest.id, "good");
        assert_eq!(
            broken.len(),
            2,
            "两份坏清单都要回来，不能静默消失：{broken:?}"
        );
        let json_one = broken
            .iter()
            .find(|b| b.path.parent().unwrap().file_name().unwrap() == "badjson")
            .expect("badjson 应当在坏清单里");
        assert!(
            json_one.reason.contains("JSON"),
            "原因要能让人对着清单改：{}",
            json_one.reason
        );
        let id_one = broken
            .iter()
            .find(|b| b.path.parent().unwrap().file_name().unwrap() == "badid")
            .expect("badid 应当在坏清单里");
        assert!(
            id_one.reason.contains("不一致"),
            "`validate` 的报错原句要原样带回来：{}",
            id_one.reason
        );
        // `load` 是报告的投影：老调用方（键表 / 类型表 / 侧栏）拿到的仍然只有能用的。
        assert_eq!(load(&root).len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `types` 的四种写坏法都要被拒：整条清单不加载，而不是「收了字段却没人答」。
    #[test]
    fn rejects_broken_type_rules() {
        let cases: Vec<(Vec<TypeRule>, &str)> = vec![
            (
                vec![TypeRule {
                    ext: vec![],
                    label: "字幕".into(),
                }],
                "没写 ext",
            ),
            (
                vec![TypeRule {
                    ext: vec![".srt".into()],
                    label: "  ".into(),
                }],
                "没写 label",
            ),
            (
                vec![TypeRule {
                    // 双后缀在这套判据下永远命不中（实际命中的是 gz），所以直接拒。
                    ext: vec![".tar.gz".into()],
                    label: "压缩包".into(),
                }],
                "认不出的扩展名",
            ),
            (
                vec![
                    TypeRule {
                        ext: vec![".srt".into()],
                        label: "字幕".into(),
                    },
                    TypeRule {
                        ext: vec![".vtt".into(), "SRT".into()],
                        label: "网络字幕".into(),
                    },
                ],
                "被两条规则同时认领",
            ),
        ];
        for (rules, want) in cases {
            let mut m = manifest("a");
            m.types = rules;
            let err = validate(&m, Some("a")).expect("坏写法应被拒");
            assert!(err.contains(want), "{want}：报出来的却是「{err}」");
        }
    }

    /// 摊平成表：折小写、去点、跳过关掉的扩展、抢同一个后缀时先到先得。
    #[test]
    fn type_labels_fold_case_and_keep_the_first_claim() {
        let rule = |ext: &[&str], label: &str| TypeRule {
            ext: ext.iter().map(|s| s.to_string()).collect(),
            label: label.to_string(),
        };
        let mut off = manifest("aaa");
        off.enabled = false;
        off.types = vec![rule(&[".iso"], "光盘映像")];

        let exts = vec![
            Extension {
                manifest: Manifest {
                    types: vec![rule(&[".SRT ", ".vtt"], "字幕")],
                    ..manifest("b")
                },
                path: PathBuf::new(),
            },
            Extension {
                // 与下面那条抢 `.log`：目录名靠前的赢（load 排过序，顺序是确定的）。
                manifest: Manifest {
                    types: vec![rule(&[".log"], "运行日志")],
                    ..manifest("c")
                },
                path: PathBuf::new(),
            },
            Extension {
                manifest: Manifest {
                    types: vec![rule(&[".log"], "系统日志"), rule(&[".srt"], "第二个 srt")],
                    ..manifest("d")
                },
                path: PathBuf::new(),
            },
            Extension {
                manifest: off,
                path: PathBuf::new(),
            },
        ];
        let got = type_labels(&exts);
        assert_eq!(
            got.get("srt").map(|s| s.as_str()),
            Some("字幕"),
            "大写与带点的写法要折进同一把钥匙，且先到先得：{got:?}"
        );
        assert_eq!(got.get("vtt").map(|s| s.as_str()), Some("字幕"));
        assert_eq!(got.get("log").map(|s| s.as_str()), Some("运行日志"));
        assert!(
            !got.contains_key("iso"),
            "关掉的扩展不该留下类型标签：{got:?}"
        );
    }

    /// 签名：装一个扩展、改一份清单、再加一个扩展，都要变；什么都不动则不变。
    ///
    /// 这条钉的是「用户手改清单下一帧就生效」这件事的另一半——缓存**会**作废。
    /// （改内容用不同长度，避免踩 mtime 精度这个已知盲区，见 [`fingerprint`] 的告警。）
    #[test]
    fn fingerprint_follows_manifest_changes() {
        let root = std::env::temp_dir().join(format!("mo-extfp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("one")).unwrap();
        let one = root.join("one").join("manifest.json");
        std::fs::write(
            &one,
            r#"{"id":"one","name":"一","types":[{"ext":[".srt"],"label":"字幕"}]}"#,
        )
        .unwrap();
        let base = fingerprint(&root);
        assert_eq!(fingerprint(&root), base, "没动过就该是同一个签名");

        std::fs::write(
            &one,
            r#"{"id":"one","name":"一","types":[{"ext":[".srt"],"label":"字幕文件"}]}"#,
        )
        .unwrap();
        let edited = fingerprint(&root);
        assert_ne!(edited, base, "改过清单还认成没改 = 标签缓死了");

        std::fs::create_dir_all(root.join("two")).unwrap();
        std::fs::write(
            root.join("two").join("manifest.json"),
            r#"{"id":"two","name":"二"}"#,
        )
        .unwrap();
        assert_ne!(fingerprint(&root), edited, "新装的扩展也要让缓存作废");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 投给侧栏的那几条：只挑声明了 `sidebar` 的，命令在前、工作流在后。
    ///
    /// 这条守的是 P2-5 的入口判据。两个靶子：把 `goes_to` 换成「有 `menu` 就算」→
    /// 第一条红（只投面板的那条会凭空多出一行）；把 `sidebar_entries_of` 里工作流
    /// 那段删掉 → 第二条红。
    #[test]
    fn sidebar_entries_picks_only_the_sidebar_slot() {
        let cmd = |name: &str, menu: &[&str]| UserCommand {
            name: name.to_string(),
            category: "字幕".to_string(),
            shell: "wc -w {file}".to_string(),
            source: None,
            menu: menu.iter().map(|s| s.to_string()).collect(),
            key: String::new(),
        };
        let wf = |name: &str, menu: &[&str]| mo_config::Workflow {
            name: name.to_string(),
            steps: vec!["pwd".to_string()],
            source: None,
            menu: menu.iter().map(|s| s.to_string()).collect(),
            key: String::new(),
        };
        let got = sidebar_entries_of(
            &[
                cmd("只进面板", &["palette"]),
                cmd("进侧栏和面板", &["sidebar", "palette"]),
                cmd("写错的界面名", &["siderbar"]),
            ],
            &[wf("也要进侧栏", &["sidebar"]), wf("打包", &[])],
        );
        let names: Vec<&str> = got.iter().map(|e| e.label()).collect();
        // 只有声明了 sidebar 的两条；工作流排在所有命令之后（与命令面板同一条顺序）。
        assert_eq!(names, ["进侧栏和面板", "也要进侧栏"]);
        // 分区名 = `category`，侧栏与命令面板问的是同一句话（P2-5 的取舍，见 devlog §4.9）。
        assert_eq!(got[0].group(), "字幕");
        assert_eq!(got[1].group(), "工作流");
        // 什么都没声明 → 空列表（侧栏一个区都不多出）。
        assert!(sidebar_entries_of(&[cmd("只进面板", &[])], &[wf("打包", &[])]).is_empty());
    }

    /// `when_ext` 约束下的命令不能投侧栏（与快捷键同一条判据）。
    ///
    /// 为什么这不是「实现麻烦」而是语义：投递给侧栏的那一份来自
    /// `user_commands(&[])`——**不带选区过滤**的那一批，受 `when_ext` 约束的命令根本
    /// 不在里面。所以这种声明是「写了、校验过了、界面上永远看不见」，加载期就拒掉。
    #[test]
    fn rejects_sidebar_slot_on_ext_gated_command() {
        let mut m = manifest("a");
        m.commands[0].menu = vec!["sidebar".to_string()];
        assert!(
            validate(&m, Some("a")).is_none(),
            "没写 when_ext 时投侧栏是合法的"
        );
        m.when_ext = vec![".srt".into()];
        let err = validate(&m, Some("a")).expect("when_ext + sidebar 应被拒");
        assert!(err.contains("统计"), "{err} 要指得出是哪条命令");
        assert!(err.contains("侧栏"), "{err} 要说清是哪一处的冲突：{err}");
        assert!(err.contains("when_ext"), "{err} 要说出理由");
    }

    /// 声明签名：三处来源（config.json、`commands/*.json`、扩展清单）任一处变动都要
    /// 让缓存作废；都没动则不变。
    ///
    /// 这条钉的是「用户手改任何一处声明，下一帧侧栏就跟着变」——少了 `commands/` 或
    /// config.json 那两项，改这两处的用户会看到侧栏一动不动，而且**重启才好**（最难查
    /// 的那类 bug）。同 [`fingerprint`]：改内容用不同长度，避开 mtime 精度盲区。
    #[test]
    fn declarations_fingerprint_follows_every_source() {
        let dir = std::env::temp_dir().join(format!("mo-extdeclfp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, r#"{"commands":[]}"#).unwrap();

        let base = declarations_fingerprint(&cfg);
        assert_eq!(
            declarations_fingerprint(&cfg),
            base,
            "没动过就该是同一个签名"
        );

        // 1) 配置里的 commands 段变了（侧栏项也能写在这里）。
        std::fs::write(&cfg, r#"{"commands":[{"name":"统计"}]}"#).unwrap();
        let edited = declarations_fingerprint(&cfg);
        assert_ne!(edited, base, "改了 config.json 还认成没改 = 侧栏缓死了");

        // 2) 多一个 `commands/*.json`。
        std::fs::create_dir_all(dir.join("commands")).unwrap();
        std::fs::write(dir.join("commands").join("a.json"), r#"[{"name":"转换"}]"#).unwrap();
        let with_cmd = declarations_fingerprint(&cfg);
        assert_ne!(with_cmd, edited, "新增命令清单也要让缓存作废");

        // 3) 改那份清单的内容（同长度不行，换个长度）。
        std::fs::write(
            dir.join("commands").join("a.json"),
            r#"[{"name":"转换格式"}]"#,
        )
        .unwrap();
        assert_ne!(declarations_fingerprint(&cfg), with_cmd);

        // 4) 装一个扩展。
        std::fs::create_dir_all(dir.join("extensions").join("one")).unwrap();
        std::fs::write(
            dir.join("extensions").join("one").join("manifest.json"),
            r#"{"id":"one","name":"一"}"#,
        )
        .unwrap();
        assert_ne!(
            declarations_fingerprint(&cfg),
            with_cmd,
            "新装的扩展也要让缓存作废"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
