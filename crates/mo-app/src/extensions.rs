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

use std::path::{Path, PathBuf};

use mo_config::UserCommand;
use serde::{Deserialize, Serialize};

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
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
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
                tracing::warn!("扩展清单 {} 读不出来：{e}", manifest.display());
                continue;
            }
        };
        let m: Manifest = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("扩展清单 {} 不是合法 JSON：{e}", manifest.display());
                continue;
            }
        };
        let dir_name = d
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(err) = validate(&m, Some(&dir_name)) {
            tracing::warn!("扩展 {} 被跳过：{err}", manifest.display());
            continue;
        }
        out.push(Extension {
            manifest: m,
            path: manifest,
        });
    }
    out
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
            c.name = format!("{} · {}", m.name, c.name);
            if c.category.trim().is_empty() {
                c.category = m.name.clone();
            }
            c.source = Some(e.path.display().to_string());
            out.push(c);
        }
    }
    out
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
            }],
            when_ext: Vec::new(),
            workflows: Vec::new(),
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
}
