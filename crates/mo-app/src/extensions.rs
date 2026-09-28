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

/// 两个扩展抢同一个扩展名、先到先得之后，输的那一方的那条记录。
///
/// 缓存里带这一批是为了让扩展管理器能按 `loser` 归并、把「你的 `.xxx` 被别人抢先认领、
/// 这条不生效」亮在输家那一家的下面（P2 剩余 #2）——否则它只会留在 `tracing::warn!`
/// 里，写清单的人对着界面查不出自己的标签为什么没出现。
#[derive(Debug, Clone, PartialEq)]
pub struct TypeLabelConflict {
    /// 被抢的扩展名（小写、不含点）。
    pub ext: String,
    /// 赢家扩展 id（按目录名排序先到）。
    pub winner: String,
    /// 输家扩展 id（它的 `types` 里写了这个扩展名，但表里已经有人先占了）。
    pub loser: String,
}

/// [`AppState::type_labels`](crate::AppState::type_labels) 那份缓存的形状：
/// `(上次读到的清单签名, 表, 撞车记录)`，`None` = 还没读过。
///
/// 起了名字是因为
/// `Arc<Mutex<Option<(u64, Arc<TypeLabels>, Vec<TypeLabelConflict>)>>>` 这种嵌套 clippy
/// 会说「太复杂」，而它确实到了该有个名字的厚度。
pub type TypeLabelCache = Option<(u64, std::sync::Arc<TypeLabels>, Vec<TypeLabelConflict>)>;

/// 清单 `provider` 段（P3，devlog §3/§5）：声明宿主怎么起进程、说哪些方法。
///
/// 这是「带逻辑的一律走 provider」（§3）的声明形态：清单只说**怎么起**与**说什么**，
/// 进程说什么话由协议定（JSON Lines，见 `crate::provider`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSpec {
    /// 启动命令（argv 形态）：`run[0]` 是可执行文件，相对路径按扩展目录解析；
    /// `run[1..]` 原样作为参数。
    pub run: Vec<String>,
    /// 声明要用的方法。本轮（P3）只认 `classify` / `preview`；`list` 是 P4 的，
    /// 写了会被 [`validate`] 拒（写了不生效的声明不收，与认不出的菜单槽同一判据）。
    #[serde(default)]
    pub methods: Vec<String>,
    /// 握手（`initialize`）超时，毫秒。缺省 2000（协议常量见 `crate::provider`）。
    #[serde(default)]
    pub startup_timeout_ms: Option<u64>,
    /// 单次调用超时，毫秒。缺省 800。
    #[serde(default)]
    pub call_timeout_ms: Option<u64>,
}

/// 本轮承认的 provider 方法。`list` 属于 P4，故意不在表里。
pub const PROVIDER_METHODS: &[&str] = &["classify", "preview"];

/// 清单 `capabilities` 认的四把钥匙（devlog §6）。`read-names` 缺省就给，不必写。
pub const CAPABILITIES: &[&str] = &["read-names", "read-contents", "write", "net"];

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
    /// provider 进程声明（P3）。`None` = 纯声明层扩展（P2 形态，不起进程）。
    #[serde(default)]
    pub provider: Option<ProviderSpec>,
    /// 申请的能力（devlog §6）：`read-names` 缺省给；`read-contents` 决定宿主要不要把
    /// 文件头 4 KiB 附进 `classify` 入参；`write` / `net` 本轮没有执行点检查（协议没有
    /// 反向请求，进程拿不到入参以外的东西），收进来只为在确认卡上说清楚。
    #[serde(default)]
    pub capabilities: Vec<String>,
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

/// 校验一份清单的 `provider` 与 `capabilities`；返回错误描述（`None` = 合法）。
///
/// 与 `validate_types` 同一条纪律：**写了却不生效的声明当场拒**，不收「解析了却没人
/// 读」的东西——
///
/// * `capabilities` 没有执行点检查它们的唯一前提是有 provider 进程（`read-contents`
///   的执行点是 `classify` 入参附不附文件头；`write` / `net` 的执行点是进程本身）。
///   声明层扩展写它们 = 永远没人读，拒。
/// * `methods` 写了 `list`：P4 还没实现，本轮没人会调它——「写了、校验过了、界面上
///   永远看不见」的组合正是 `when_ext` + 侧栏那条被拒的理由。
/// * 超时写 0 / 负意义的值：0 ms 的超时等于进程永远不可用，这不是配置是自杀。
fn validate_provider(m: &Manifest) -> Option<String> {
    let caps = &m.capabilities;
    if m.provider.is_none() {
        if !caps.is_empty() {
            return Some(format!(
                "扩展「{}」申请了 capabilities（{}）却没有 provider 进程——能力是给进程用的，没有进程就没有执行点；要么补 provider，要么删掉 capabilities",
                m.id,
                caps.join("、")
            ));
        }
        return None;
    }
    let p = m.provider.as_ref().expect("刚判过 None");
    if p.run.is_empty() || p.run[0].trim().is_empty() {
        return Some(format!(
            "扩展「{}」的 provider.run 没写可执行文件（argv 形态，如 [\"bin/工具\"]）",
            m.id
        ));
    }
    if p.methods.is_empty() {
        return Some(format!(
            "扩展「{}」的 provider.methods 是空的——起了进程却不说要干什么，宿主永远不会调它",
            m.id
        ));
    }
    for method in &p.methods {
        if !PROVIDER_METHODS.contains(&method.as_str()) {
            let hint = if method == "list" {
                "（list 是列表源，排在 P4，本轮还没实现——先别写）"
            } else {
                ""
            };
            return Some(format!(
                "扩展「{}」的 provider.methods 写了认不出的方法「{method}」{hint}，本轮只认 classify / preview",
                m.id
            ));
        }
    }
    for (field, ms) in [
        ("startup_timeout_ms", p.startup_timeout_ms),
        ("call_timeout_ms", p.call_timeout_ms),
    ] {
        if let Some(v) = ms {
            if v == 0 {
                return Some(format!(
                    "扩展「{}」的 provider.{field} 写了 0——0 毫秒的超时等于永远不可用；要缺省就别写这个字段",
                    m.id
                ));
            }
            if v > 60_000 {
                return Some(format!(
                    "扩展「{}」的 provider.{field} 写了 {v} ms（超过 60 秒）——超时的意义是「卡了就放弃」，一分钟不是超时是死等",
                    m.id
                ));
            }
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for cap in caps {
        if !CAPABILITIES.contains(&cap.as_str()) {
            return Some(format!(
                "扩展「{}」的 capabilities 写了认不出的能力「{cap}」（只认 read-names / read-contents / write / net）",
                m.id
            ));
        }
        if seen.contains(&cap.as_str()) {
            return Some(format!(
                "扩展「{}」的 capabilities 里「{cap}」写了两遍",
                m.id
            ));
        }
        seen.push(cap);
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
    if let Some(err) = validate_provider(m) {
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

/// 从磁盘安装一个扩展（§6）：来源要是一个**含 `manifest.json` 的目录**。
///
/// 步骤：读来源清单 → [`validate`] → 复制进 `<root>/<id>/` → **改写清单为停用** →
/// 写 `installed.json` 记来源与每个文件的 sha256。中途任何一步失败都不留半个
/// 目录（复制失败会把它清掉）。
///
/// 「安装即停用」是刻意的：手放进 `extensions/` 的清单缺省是启用的（§4.10 的
/// 缺口），但**经过安装流程**装出来的不许缺省就跑——装完它以停用状态出现在
/// 扩展管理器里，点「启用」才走那张贡献确认卡。至此「装了就跑」只剩「手工摆放」
/// 一条路，安装是正门，正门有门禁。
pub fn install_from(source: &Path, root: &Path) -> Result<Manifest, String> {
    let raw = std::fs::read_to_string(source.join("manifest.json")).map_err(|e| {
        format!(
            "「{}」里读不到 manifest.json：{e}（要装的是一个含清单的扩展目录）",
            source.display()
        )
    })?;
    let mut m: Manifest =
        serde_json::from_str(&raw).map_err(|e| format!("manifest.json 不是合法 JSON：{e}"))?;
    if let Some(err) = validate(&m, None) {
        return Err(err);
    }
    std::fs::create_dir_all(root)
        .map_err(|e| format!("建扩展目录 {} 失败：{e}", root.display()))?;
    // 来源已经在扩展目录里 → 它本来就是装好的扩展，再「装」一遍只会自己复制自己。
    let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if let Ok(src_canon) = std::fs::canonicalize(source) {
        if src_canon.starts_with(&root_canon) {
            return Err(format!(
                "「{}」就在扩展目录里，它已经是装好的了",
                source.display()
            ));
        }
    }
    let target = root.join(&m.id);
    if target.exists() {
        return Err(format!(
            "扩展目录里已经有「{}」了；先卸载（删掉那个目录）再装",
            m.id
        ));
    }
    if let Err(err) = copy_tree(source, &target) {
        let _ = std::fs::remove_dir_all(&target);
        return Err(err);
    }
    m.enabled = false;
    std::fs::write(
        target.join("manifest.json"),
        serde_json::to_string_pretty(&m).map_err(|e| format!("清单写不回去：{e}"))?,
    )
    .map_err(|e| format!("清单写不回去：{e}"))?;
    // 账本：来源 + 每个文件（不含账本自己）的 sha256。逐文件记而不是整目录一个
    // 总和，是因为将来要回答「这份扩展被动过没有」得能定位到**哪个文件**动了。
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(&target).sort_by_file_name() {
        let entry = entry.map_err(|e| format!("读安装目录失败：{e}"))?;
        if !entry.file_type().is_file() {
            continue;
        }
        // 记相对路径（统一 / 分隔）：子目录里同名文件在账上不能撞成一条。
        let rel = entry
            .path()
            .strip_prefix(&target)
            .expect("walkdir 返回的都是 target 下的路径")
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "installed.json" {
            continue;
        }
        files.push(serde_json::json!({
            "path": rel,
            "sha256": sha256_file(entry.path())?,
        }));
    }
    let record = serde_json::json!({
        "source": source.display().to_string(),
        "files": files,
    });
    std::fs::write(
        target.join("installed.json"),
        serde_json::to_string_pretty(&record)
            .map_err(|e| format!("installed.json 写不出来：{e}"))?,
    )
    .map_err(|e| format!("installed.json 写不出来：{e}"))?;
    Ok(m)
}

/// 把一份 `.moext`（zip 压缩包）装进来：解压到临时目录 → 定位扩展目录 → 走
/// [`install_from`](crate::extensions::install_from)（复制 + 改写停用 + 记账）。
///
/// `.moext` 是「正门之外」的另一条正门：用户当面在一份文件上点选，比「把一个目录
/// 拖来指认」更适合分发——下载得到的就是一份文件，而不是一坨散目录。解压失败 / 定位
/// 不到清单 / 装失败，都要把临时目录清掉，不留半截在 `TMPDIR` 里。
///
/// 压缩包里的布局两种都认（给作者留余地）：清单 `manifest.json` 要么在解压根，要么在
/// 根下恰好一个子目录里（那个子目录就是扩展目录）。其它布局（根下多个目录、或散文件
/// 没有清单）一律拒——那不是一份扩展包，硬塞只会装出结构错乱的一坨。
pub fn install_from_archive(archive: &Path, root: &Path) -> Result<Manifest, String> {
    let dest = std::env::temp_dir().join(format!(
        "mo-moext-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    if let Err(e) = extract_zip(archive, &dest) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    let source = match locate_source(&dest) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e);
        }
    };
    // 解压出来的是副本，`install_from` 还会再复制一份进扩展目录；装完（成功或失败）
    // 临时目录都没用了，清掉。
    let result = install_from(&source, root);
    let _ = std::fs::remove_dir_all(&dest);
    result
}

/// 卸载一个扩展：删掉 `root/<id>` 整个目录。
///
/// 「卸载 = 删目录」是这张扩展模型的语义：一个扩展就是「一份清单 + 它带的文件」，
/// 没有注册表、没有要回收的系统资源——目录没了，它贡献的一切（命令 / 工作流 /
/// 键位 / 侧栏行 / 种类文案）在调用方下次重取时自然消失（缓存按目录指纹作废，
/// 见 `fingerprint` / `declarations_fingerprint`）。装好的与手工摆放的落点相同，
/// 卸载语义没有分别。
///
/// id 先过 [`valid_id`] 再拼路径：这是外部进来的字符串，`..` 里的点不在合法字符
/// 集里，`root.join(id)` 就指不出扩展目录，路径穿越无门。
pub fn uninstall_extension(id: &str, root: &Path) -> Result<(), String> {
    if !valid_id(id) {
        return Err(format!("id「{id}」不合法（只允许小写字母、数字、_、-）"));
    }
    let target = root.join(id);
    if !target.is_dir() {
        return Err(format!("扩展目录里没有「{id}」（可能已经卸载了）"));
    }
    std::fs::remove_dir_all(&target).map_err(|e| format!("删 {} 失败：{e}", target.display()))
}

/// 解压一个 zip 到 `dest`（dst 必须不存在或为空）。
///
/// 三条与 [`copy_tree`](crate::extensions::copy_tree) 同一条纪律：目录按名排序遍历
/// （zip 里条目顺序不定，排序让结果可复现）、符号链接拒收、路径要防 zip-slip（
/// [`zip::ZipFile::enclosed_name`] 会把 `../` 与绝对路径挡成 `None`）。压包是从外部
/// 进来的，里面的链接指向哪只有来源机器知道，复制它等于抄一段意义不明的内容进来。
fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    use std::io::Read;
    let file = std::fs::File::open(archive).map_err(|e| format!("打不开 .moext：{e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!(".moext 不是合法的 zip：{e}"))?;
    std::fs::create_dir_all(dest).map_err(|e| format!("建临时解压目录失败：{e}"))?;
    for i in 0..archive.len() {
        let mut zf = archive
            .by_index(i)
            .map_err(|e| format!("读 .moext 内容失败：{e}"))?;
        // enclosed_name：路径若想逃出 dest（../ 或绝对路径）就返回 None——防 zip-slip。
        let Some(safe) = zf.enclosed_name().map(|p| p.to_path_buf()) else {
            return Err(format!("压包里有不安全路径「{}」，不装", zf.name()));
        };
        let out = dest.join(&safe);
        if zf.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| format!("建 {} 失败：{e}", out.display()))?;
            continue;
        }
        // 拒收符号链接：与 copy_tree 同一句判据（unix 上靠 mode 识别；Windows 上 zip
        // 几乎没有 symlink 概念，这条静默跳过即可）。
        #[cfg(unix)]
        {
            let mode = zf.unix_mode().unwrap_or(0);
            if (mode & 0o170000) == 0o120000 {
                return Err(format!(
                    "{} 是符号链接，不装（扩展包里不该有链接）",
                    zf.name()
                ));
            }
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("建 {} 失败：{e}", parent.display()))?;
        }
        let mut buf = Vec::with_capacity(zf.size() as usize);
        zf.read_to_end(&mut buf)
            .map_err(|e| format!("读 {} 失败：{e}", zf.name()))?;
        std::fs::write(&out, &buf).map_err(|e| format!("写 {} 失败：{e}", out.display()))?;
    }
    Ok(())
}

/// 在解压根里找到「扩展目录」：清单 `manifest.json` 要么直接在此，要么在根下恰好一个
/// 子目录里。
fn locate_source(extracted: &Path) -> Result<PathBuf, String> {
    if extracted.join("manifest.json").is_file() {
        return Ok(extracted.to_path_buf());
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut entries = std::fs::read_dir(extracted).map_err(|e| format!("读解压目录失败：{e}"))?;
    while let Some(e) = entries
        .next()
        .transpose()
        .map_err(|e| format!("读解压目录失败：{e}"))?
    {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        }
    }
    if dirs.len() == 1 && dirs[0].join("manifest.json").is_file() {
        return Ok(dirs.swap_remove(0));
    }
    Err(
        "压包里找不到 manifest.json（.moext 应当是一份扩展目录，或顶层直接含 manifest.json）"
            .into(),
    )
}

/// 一个尽量不撞车的临时目录后缀：进程 id 不够（同一进程里多次安装会撞），补上纳秒。
fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 把 `src` 整棵复制到 `dst`（dst 必须不存在或为空）。目录按名排序遍历，符号链接
/// 拒收——扩展是从外部进来的，链接指到哪只有来源机器知道，复制它等于抄一段
/// 意义不明的目标内容进来。
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in walkdir::WalkDir::new(src).sort_by_file_name() {
        let entry = entry.map_err(|e| format!("读来源 {} 失败：{e}", src.display()))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("walkdir 返回的都是 src 下的路径");
        let to = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| format!("建 {} 失败：{e}", to.display()))?;
        } else if entry.file_type().is_symlink() {
            return Err(format!(
                "{} 是符号链接，不装（扩展包里不该有链接）",
                entry.path().display()
            ));
        } else {
            std::fs::copy(entry.path(), &to)
                .map_err(|e| format!("复制 {} 失败：{e}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::Digest;
    let mut f =
        std::fs::File::open(path).map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
    let mut h = sha2::Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
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
/// ⚠️ P3 起 [`Self::Provider`] 收了 §6 草案的 `capabilities`：它的执行点随 provider
/// 落地（`read-contents` 决定 `classify` 入参附不附文件头；进程模型本身没有反向请求，
/// `write` / `net` 是「说了也拿不到」的声明，收进来只为在确认卡上说真话）。
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
    /// provider 进程（P3）：声明的方法与申请的能力。确认卡要让用户看见的是
    /// 「它要起一个进程、这个进程会拿到什么」——能力说的就是这件事。
    Provider {
        methods: Vec<String>,
        capabilities: Vec<String>,
    },
}

/// 把一份清单摊成「它会改动界面上的哪些地方」。
///
/// 顺序恒为 命令 → 工作流 → 类型标签 → provider（与命令面板里那批贡献项同一口径：
/// 命令在前；进程是「会跑代码的」那一级，放最后）。
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
    if let Some(p) = &m.provider {
        // `read-names` 缺省就给，不劳作者自己写；这里报的是**实际生效**的能力集合，
        // 与协议执行点（`crate::provider` 的 head_b64 门）同一条判据。
        let mut caps: Vec<String> = m
            .capabilities
            .iter()
            .filter(|c| c.as_str() != "read-names")
            .cloned()
            .collect();
        caps.insert(0, "read-names".to_string());
        out.push(Contribution::Provider {
            methods: p.methods.clone(),
            capabilities: caps,
        });
    }
    out
}

/// 把各清单的 `types` 摊成「扩展名 → 种类文案」，并顺手记下**撞车**（P2 剩余 #2）。
///
/// 两条与 [`flatten`] 同源的规矩：
/// * 关掉的扩展整体消失（它的命令消失，类型标签也该消失——留着就是「停用了还在改
///   我的显示」）；
/// * 两个扩展抢同一个扩展名时**先到先得**。[`load`] 按目录名排过序，所以「谁先」
///   是确定的；后到的那一条被记进 [`TypeLabelConflict`] 的 `loser`，由扩展管理器
///   按扩展 id 归并亮出，不再只留在 `tracing::warn!` 里。
pub fn type_labels_report(exts: &[Extension]) -> (TypeLabels, Vec<TypeLabelConflict>) {
    let mut out = TypeLabels::new();
    // 扩展名 → 占用它的扩展 id：撞车时查赢家用，也顺带挡掉「同一扩展内部重复」（
    // `validate` 已拒，这里双保险）。
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    let mut conflicts = Vec::new();
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
                if let Some(winner) = owner.get(&key) {
                    if winner != &m.id {
                        conflicts.push(TypeLabelConflict {
                            ext: key.clone(),
                            winner: winner.clone(),
                            loser: m.id.clone(),
                        });
                    }
                    continue;
                }
                owner.insert(key.clone(), m.id.clone());
                out.insert(key, t.label.clone());
            }
        }
    }
    (out, conflicts)
}

/// [`type_labels_report`] 的投影：只要表的那一半（列表渲染热路径只用表，不需要撞车记录）。
pub fn type_labels(exts: &[Extension]) -> TypeLabels {
    type_labels_report(exts).0
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
            provider: None,
            capabilities: Vec::new(),
        }
    }

    /// provider 段的写坏法都要被拒：capabilities 没有 provider、methods 空 / 认不出、
    /// run 空、超时写 0。合法的写法（含 list 被拒的措辞）各钉一条。
    #[test]
    fn validates_provider_and_capabilities() {
        let spec = |methods: &[&str]| ProviderSpec {
            run: vec!["bin/tool".into()],
            methods: methods.iter().map(|s| s.to_string()).collect(),
            startup_timeout_ms: None,
            call_timeout_ms: None,
        };

        // 合法：provider + classify。
        let mut m = manifest("a");
        m.provider = Some(spec(&["classify"]));
        assert!(validate(&m, Some("a")).is_none());

        // capabilities 没有 provider → 拒（没有执行点 = 永远没人读）。
        let mut m2 = manifest("a");
        m2.capabilities = vec!["read-contents".into()];
        let err = validate(&m2, Some("a")).expect("capabilities 无 provider 应被拒");
        assert!(err.contains("没有 provider"), "{err}");

        // methods 写 list → 拒，且要指出「P4」。
        let mut m3 = manifest("a");
        m3.provider = Some(spec(&["classify", "list"]));
        let err = validate(&m3, Some("a")).expect("list 应被拒");
        assert!(err.contains("P4"), "{err}");

        // methods 空 / run 空 / 认不出的方法。
        let mut m4 = manifest("a");
        m4.provider = Some(spec(&[]));
        assert!(validate(&m4, Some("a")).is_some());
        let mut m5 = manifest("a");
        m5.provider = Some(ProviderSpec {
            run: vec![String::new()],
            ..spec(&["classify"])
        });
        assert!(validate(&m5, Some("a")).is_some());
        let mut m6 = manifest("a");
        m6.provider = Some(spec(&["host.read_file"]));
        let err = validate(&m6, Some("a")).expect("反向请求这类方法名应被拒");
        assert!(err.contains("host.read_file"), "{err}");

        // 超时 0 与超天。
        let mut m7 = manifest("a");
        m7.provider = Some(ProviderSpec {
            call_timeout_ms: Some(0),
            ..spec(&["classify"])
        });
        let err = validate(&m7, Some("a")).expect("0 超时应被拒");
        assert!(err.contains("call_timeout_ms"), "{err}");

        // 认不出的能力。
        let mut m8 = manifest("a");
        m8.provider = Some(spec(&["preview"]));
        m8.capabilities = vec!["read-everything".into()];
        let err = validate(&m8, Some("a")).expect("认不出的能力应被拒");
        assert!(err.contains("read-everything"), "{err}");
    }

    /// 贡献表：provider 扩展多一条 `Provider`，能力集合**始终含缺省的 read-names**
    /// （作者不写也在），且排最前——确认卡按这条念。
    #[test]
    fn contributions_include_provider_methods_and_capabilities() {
        let mut m = manifest("a");
        m.provider = Some(ProviderSpec {
            run: vec!["bin/tool".into()],
            methods: vec!["classify".into(), "preview".into()],
            startup_timeout_ms: None,
            call_timeout_ms: None,
        });
        m.capabilities = vec!["read-contents".into(), "net".into()];
        let last = contributions(&m).pop().expect("provider 应在贡献表里");
        assert_eq!(
            last,
            Contribution::Provider {
                methods: vec!["classify".into(), "preview".into()],
                capabilities: vec!["read-names".into(), "read-contents".into(), "net".into()],
            },
            "read-names 是缺省能力，不用写也在；顺序：缺省在前、声明按清单序"
        );
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

    /// 安装 = 复制 + 改写为停用 + 记账。
    ///
    /// 「装了就跑」的口子从正门堵上：装出来当场就是停用的（盘上的清单也写回
    /// `"enabled": false`），启用要走扩展管理器那张确认卡。账本逐文件带 sha256、
    /// 记相对路径，作者日后能定位「哪个文件动过」。
    #[test]
    fn installs_disabled_with_provenance() {
        let tmp = std::env::temp_dir().join(format!("mo-extinst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("来源");
        let root = tmp.join("extensions");
        std::fs::create_dir_all(src.join("子目录")).unwrap();
        std::fs::write(
            src.join("manifest.json"),
            r#"{"id":"p28","name":"安装来的扩展","enabled":true,"commands":[{"name":"跑一下","shell":"pwd"}]}"#,
        )
        .unwrap();
        std::fs::write(src.join("run.cmd"), "echo hi").unwrap();
        std::fs::write(src.join("子目录/extra.txt"), "x").unwrap();

        let m = install_from(&src, &root).expect("应当装上");
        assert!(!m.enabled, "安装即停用：启用要走确认卡");
        let target = root.join("p28");
        let on_disk = std::fs::read_to_string(target.join("manifest.json")).unwrap();
        assert!(
            on_disk.contains("\"enabled\": false"),
            "盘上的清单也要写回停用（加载时读的是盘）：{on_disk}"
        );

        let record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(target.join("installed.json")).unwrap())
                .unwrap();
        assert_eq!(record["source"], src.display().to_string());
        let files = record["files"].as_array().unwrap();
        assert_eq!(
            files.len(),
            3,
            "manifest.json + run.cmd + 子目录/extra.txt 都在账上：{files:?}"
        );
        assert!(
            files.iter().any(|f| f["path"] == "子目录/extra.txt"),
            "子目录里的文件记相对路径，不与顶层撞名：{files:?}"
        );
        let manifest_entry = files
            .iter()
            .find(|f| f["path"] == "manifest.json")
            .expect("manifest.json 在账上");
        let want = sha256_file(&target.join("manifest.json")).unwrap();
        assert_eq!(
            manifest_entry["sha256"],
            want.as_str(),
            "账上的 sha 要对得上装出来的那份"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 拒收的几种来路：没有清单、validate 不过、重复安装、来源就在扩展目录里。
    /// 每一种都不许留下半个目录，也不许弄坏已经装好的那份。
    #[test]
    fn refuses_to_install_bad_sources() {
        let tmp = std::env::temp_dir().join(format!("mo-extrefuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("extensions");
        std::fs::create_dir_all(&root).unwrap();

        let empty = tmp.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(
            install_from(&empty, &root).is_err(),
            "没有 manifest.json 要拒"
        );
        assert!(!root.join("empty").exists(), "拒了就不该留东西");

        let bad = tmp.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("manifest.json"), r#"{"id":"不是id","name":"x"}"#).unwrap();
        assert!(install_from(&bad, &root).is_err(), "validate 不过要拒");
        assert!(!root.join("bad").exists(), "拒了就不该留东西");

        let good = tmp.join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("manifest.json"), r#"{"id":"p28b","name":"x"}"#).unwrap();
        install_from(&good, &root).expect("第一次应当装上");
        assert!(install_from(&good, &root).is_err(), "同一个 id 装两次要拒");
        assert!(
            root.join("p28b/manifest.json").is_file() && root.join("p28b/installed.json").is_file(),
            "拒掉第二次不能弄坏第一次装好的"
        );
        assert!(
            install_from(&root.join("p28b"), &root).is_err(),
            "来源就在扩展目录里 = 它已经是装好的"
        );

        let _ = std::fs::remove_dir_all(&tmp);
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

    /// 撞车要被记进 [`TypeLabelConflict`]、不再只留在日志里（P2 剩余 #2）。
    ///
    /// 判据：先到先得（按目录名排序），输家记录 winner / loser；赢家本身不记；关掉的
    /// 扩展既不抢也不输。这条钉的是「界面上能查到自己的标签为什么没生效」这一半——
    /// 之前它只会进 `tracing::warn!`。
    #[test]
    fn type_labels_report_records_conflicts() {
        let rule = |ext: &[&str], label: &str| TypeRule {
            ext: ext.iter().map(|s| s.to_string()).collect(),
            label: label.to_string(),
        };
        let mut off = manifest("off");
        off.enabled = false;
        off.types = vec![rule(&[".off"], "停用的标签")];
        let exts = vec![
            Extension {
                manifest: Manifest {
                    types: vec![rule(&[".log"], "赢家日志")],
                    ..manifest("win")
                },
                path: PathBuf::new(),
            },
            Extension {
                // 与 win 抢 .log，目录名靠后 → 输家。
                manifest: Manifest {
                    types: vec![rule(&[".log"], "输家日志")],
                    ..manifest("lose")
                },
                path: PathBuf::new(),
            },
            Extension {
                manifest: off,
                path: PathBuf::new(),
            },
        ];
        let (labels, conflicts) = type_labels_report(&exts);
        assert_eq!(
            labels.get("log").map(|s| s.as_str()),
            Some("赢家日志"),
            "先到先得：赢家的标签生效"
        );
        let only = conflicts
            .iter()
            .find(|c| c.ext == "log")
            .expect("应当记一条撞车");
        assert_eq!(only.winner, "win");
        assert_eq!(only.loser, "lose");
        assert!(
            conflicts.iter().all(|c| c.ext != "off"),
            "停用的扩展不该参与撞车：{conflicts:?}"
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

    /// 从 .moext（zip）安装：解压 → 定位扩展目录 → 复制 + 写回停用 + 记账。
    ///
    /// 这条钉的是「分发得到的压缩包也能走正门」——与 `install_from` 同一套「装出来即停用」
    /// 与记账逻辑，只是来源形态从「目录」换成「解压出来的目录」。布局 A：压包根下恰好
    /// 一个子目录（扩展目录）含 manifest.json。
    #[test]
    fn installs_from_archive_unzips_then_disables() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("mo-extzip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("extensions");
        std::fs::create_dir_all(&root).unwrap();

        let archive = tmp.join("p28.moext");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut zw = zip::ZipWriter::new(file);
            let opts = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("p28/manifest.json", opts).unwrap();
            zw.write_all(
                r#"{"id":"p28","name":"压缩包来的扩展","enabled":true,"commands":[{"name":"跑一下","shell":"pwd"}]}"#
                    .as_bytes(),
            )
            .unwrap();
            zw.start_file("p28/run.cmd", opts).unwrap();
            zw.write_all(b"echo hi").unwrap();
            zw.finish().unwrap();
        }

        let m = install_from_archive(&archive, &root).expect("应当从压缩包装上");
        assert!(!m.enabled, "从压缩包装出来也是停用的");
        let target = root.join("p28");
        assert!(target.join("manifest.json").is_file());
        assert!(target.join("run.cmd").is_file());
        let on_disk = std::fs::read_to_string(target.join("manifest.json")).unwrap();
        assert!(
            on_disk.contains("\"enabled\": false"),
            "盘上的清单也要写回停用：{on_disk}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 布局 B：manifest.json 直接在解压根（没有外层扩展目录）。
    #[test]
    fn installs_from_archive_with_manifest_at_root() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("mo-extziproot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("extensions");
        std::fs::create_dir_all(&root).unwrap();

        let archive = tmp.join("flat.moext");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut zw = zip::ZipWriter::new(file);
            let opts = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("manifest.json", opts).unwrap();
            zw.write_all(r#"{"id":"flat42","name":"顶层清单扩展"}"#.as_bytes())
                .unwrap();
            zw.finish().unwrap();
        }
        let m = install_from_archive(&archive, &root).expect("应当装上");
        assert_eq!(m.id, "flat42");
        assert!(root.join("flat42/manifest.json").is_file());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 压包里找不到 manifest.json（顶层全是散文件、或根下多个目录）→ 整包拒掉，
    /// 不留半个目录在扩展目录里，也不留临时解压目录在 TMPDIR。
    #[test]
    fn refuses_archive_without_manifest() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("mo-extzipbad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("extensions");
        std::fs::create_dir_all(&root).unwrap();

        let archive = tmp.join("bad.moext");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut zw = zip::ZipWriter::new(file);
            let opts = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("readme.txt", opts).unwrap();
            zw.write_all(b"no manifest here").unwrap();
            zw.finish().unwrap();
        }
        assert!(
            install_from_archive(&archive, &root).is_err(),
            "没有清单要拒"
        );
        assert!(!root.join("bad").exists(), "拒了就不该在扩展目录留东西");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 卸载 = 删整个扩展目录；装好的（带 `installed.json` 账本）与手工摆放的
    /// 落点相同，语义没有分别。
    #[test]
    fn uninstall_removes_the_extension_directory() {
        let tmp = std::env::temp_dir().join(format!("mo-extun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("extensions");
        let src = tmp.join("来源");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("manifest.json"),
            r#"{"id":"un1","name":"要被卸载的扩展"}"#,
        )
        .unwrap();
        install_from(&src, &root).expect("先装上");
        assert!(root.join("un1").is_dir());

        uninstall_extension("un1", &root).expect("应当卸掉");
        assert!(!root.join("un1").exists(), "目录必须整个消失");
        // 再卸一次：目录已经不在了，报错而不是静默成功（调用方好提示）。
        assert!(
            uninstall_extension("un1", &root).is_err(),
            "目录不存在应当报错"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// id 是外部进来的字符串，拼路径前必须过合法性：`../` 里的点不在
    /// `[a-z0-9_-]` 里，路径穿越无门。
    #[test]
    fn uninstall_rejects_ids_that_could_escape_the_root() {
        let tmp = std::env::temp_dir().join(format!("mo-extunescape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("extensions");
        std::fs::create_dir_all(root.join("victim")).unwrap();

        for bad in ["../victim", "a/b", "..", "UPPER", ""] {
            assert!(
                uninstall_extension(bad, &root).is_err(),
                "id「{bad}」不该被放行"
            );
        }
        assert!(
            root.join("victim").is_dir(),
            "邻目录必须原地不动：穿越被 valid_id 拦下"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
