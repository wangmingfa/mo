//! 用户自定义命令：占位符展开、清单加载、执行与输出捕获。
//!
//! 这是第四阶段「自定义命令」的实现，同时是「插件 / 扩展系统」的命令面：
//! 一个扩展就是一个 `commands/*.json` 清单，声明若干条命令，Mo 负责把它们
//! 并进命令面板并执行。清单格式故意做得很小（name / category / shell），
//! 学的是一个能一眼写完、又不需要跑别人代码的东西。
//!
//! ## 安全边界
//!
//! 自定义命令**就是执行 shell**，因此：
//! * 只从用户自己的配置目录加载，绝不从「当前浏览的目录」加载清单——
//!   否则打开一个别人给的文件夹就等于跑了它的脚本；
//! * 占位符逐个加引号后再拼进命令行，文件名里的空格 / 括号 / `&` 不会
//!   把命令拆成几条语句；
//! * 执行结果只回显文本，不做任何「根据输出再触发」的事。

use std::path::{Path, PathBuf};
use std::process::Command;

use mo_config::UserCommand;

/// 命令执行结果。
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// 退出码（None = 没跑起来）。
    pub code: Option<i32>,
    /// 标准输出 + 标准错误合并后的文本。
    pub text: String,
}

/// 占位符展开用的上下文。
#[derive(Debug, Clone, Default)]
pub struct CommandContext {
    /// 当前目录。
    pub dir: Option<PathBuf>,
    /// 选中的条目（按可见顺序）。
    pub selected: Vec<PathBuf>,
}

/// 按当前平台给一个路径加引号。
///
/// Windows 的 `cmd` 只认双引号，且内部双引号用 `\"` 转；POSIX 用单引号最稳
/// （单引号内除 `'` 外一切皆字面），遇到 `'` 用 `'\''` 的经典写法收尾。
pub fn quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// 展开 `{dir}` / `{file}` / `{files}`。
///
/// 缺上下文时占位符原样留着并返回 `false`（调用方据此提示「这条命令需要先
/// 选中文件」），而不是悄悄替换成空串——空串会让命令变成另一个意思。
pub fn expand(shell: &str, ctx: &CommandContext) -> (String, bool) {
    // 先把三种替换值算好：`None` 表示缺上下文，遇到对应占位符就置失败标记。
    let dir: Option<String> = ctx.dir.as_deref().map(quote);
    let file: Option<String> = ctx.selected.first().map(|p| quote(p));
    let files: Option<String> = (!ctx.selected.is_empty()).then(|| {
        ctx.selected
            .iter()
            .map(|p| quote(p))
            .collect::<Vec<_>>()
            .join(" ")
    });

    let mut ok = true;
    let mut out = String::with_capacity(shell.len());
    let mut rest = shell;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(end) = tail.find('}') {
            match &tail[..=end] {
                "{dir}" => match &dir {
                    Some(s) => out.push_str(s),
                    None => {
                        ok = false;
                        out.push_str("{dir}");
                    }
                },
                "{file}" => match &file {
                    Some(s) => out.push_str(s),
                    None => {
                        ok = false;
                        out.push_str("{file}");
                    }
                },
                "{files}" => match &files {
                    Some(s) => out.push_str(s),
                    None => {
                        ok = false;
                        out.push_str("{files}");
                    }
                },
                // 不是认识的占位符：原样保留（用户的 `echo {foo}` 别被吃掉）。
                other => out.push_str(other),
            }
            rest = &tail[end + 1..];
            continue;
        }
        out.push('{');
        rest = &tail[1..];
    }
    out.push_str(rest);
    (out, ok)
}

/// 校验一条命令定义；返回错误描述（`None` = 合法）。
pub fn validate(cmd: &UserCommand) -> Option<String> {
    if cmd.name.trim().is_empty() {
        return Some("name 不能为空".to_string());
    }
    if cmd.shell.trim().is_empty() {
        return Some(format!("命令「{}」缺少 shell", cmd.name));
    }
    None
}

/// 从 `dir` 加载命令清单（`*.json`，每个文件是单个对象或对象数组）。
///
/// 坏文件只跳过并告警，绝不因为一个写错的清单让整批命令消失。
pub fn load_manifests(dir: &Path) -> Vec<UserCommand> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            tracing::warn!("命令清单 {} 读不出来，跳过", f.display());
            continue;
        };
        // 一个文件既可以写单条，也可以写数组。
        let parsed: Vec<UserCommand> = match serde_json::from_str::<Vec<UserCommand>>(&text) {
            Ok(v) => v,
            Err(_) => match serde_json::from_str::<UserCommand>(&text) {
                Ok(one) => vec![one],
                Err(e) => {
                    tracing::warn!("命令清单 {} 不是合法 JSON（{e}），跳过", f.display());
                    continue;
                }
            },
        };
        for mut c in parsed {
            if let Some(err) = validate(&c) {
                tracing::warn!("命令清单 {} 里有一条不合法：{err}，跳过", f.display());
                continue;
            }
            if c.category.trim().is_empty() {
                c.category = "扩展".to_string();
            }
            c.source = Some(f.display().to_string());
            out.push(c);
        }
    }
    out
}

/// 执行一条命令，返回合并后的输出。
pub fn run(shell_line: &str, cwd: Option<&Path>) -> Result<CommandOutput, String> {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", shell_line]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", shell_line]);
        c
    };
    // 工作目录用真实路径（不加引号）：`cmd /C` 的命令行里再塞一层引号会被
    // 它自己吃掉，反而更难读。
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = cmd.output().map_err(|e| format!("启动失败：{e}"))?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&err);
    }
    Ok(CommandOutput {
        code: out.status.code(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &str, sel: &[&str]) -> CommandContext {
        CommandContext {
            dir: Some(PathBuf::from(dir)),
            selected: sel.iter().map(PathBuf::from).collect(),
        }
    }

    /// 占位符展开：三种都认，顺序无关。
    #[test]
    fn expands_placeholders() {
        let c = ctx("/tmp/d", &["/tmp/d/a.txt", "/tmp/d/b.txt"]);
        let (line, ok) = expand("ls -l {file}", &c);
        assert!(ok);
        assert!(line.contains("a.txt"), "{line}");
        let (line, ok) = expand("echo {dir} && ls {files}", &c);
        assert!(ok);
        assert!(line.contains("a.txt") && line.contains("b.txt"), "{line}");
    }

    /// 缺上下文时不静默变空串：占位符留着并报告不可用。
    #[test]
    fn missing_context_is_reported() {
        let c = CommandContext::default();
        let (line, ok) = expand("open {file}", &c);
        assert!(!ok, "没选中文件应当报不可用");
        assert!(line.contains("{file}"), "{line} 不该被替换成空串");
    }

    /// 带空格 / 括号 / `&` 的文件名不能把命令拆坏。
    #[test]
    fn quoting_survives_nasty_names() {
        let c = ctx("/tmp", &["/tmp/a b (x) & y.txt"]);
        let (line, ok) = expand("cat {file}", &c);
        assert!(ok);
        if cfg!(windows) {
            assert_eq!(line, r#"cat "/tmp/a b (x) & y.txt""#);
        } else {
            assert_eq!(line, "cat '/tmp/a b (x) & y.txt'");
        }
    }

    /// 未知占位符原样保留，`{` 后面没有 `}` 也不炸。
    #[test]
    fn unknown_placeholders_pass_through() {
        let c = ctx("/tmp", &[]);
        assert_eq!(expand("echo {foo}", &c).0, "echo {foo}");
        assert_eq!(expand("echo {", &c).0, "echo {");
    }

    /// 校验：空名 / 空命令都要被拒。
    #[test]
    fn validates_definitions() {
        assert!(validate(&UserCommand {
            name: " ".into(),
            category: String::new(),
            shell: "x".into(),
            source: None
        })
        .is_some());
        assert!(validate(&UserCommand {
            name: "n".into(),
            category: String::new(),
            shell: "".into(),
            source: None
        })
        .is_some());
        assert!(validate(&UserCommand {
            name: "n".into(),
            category: String::new(),
            shell: "x".into(),
            source: None
        })
        .is_none());
    }

    /// 清单加载：数组与单对象都收，坏 JSON / 缺字段的条目跳过但不影响其它。
    #[test]
    fn loads_manifests_robustly() {
        let dir = std::env::temp_dir().join(format!("mo-cmds-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("a.json"),
            r#"[{"name":"计数","shell":"wc -l {file}"},{"name":"","shell":"x"}]"#,
        )
        .unwrap();
        std::fs::write(dir.join("b.json"), r#"{"name":"单条","shell":"pwd"}"#).unwrap();
        std::fs::write(dir.join("c.json"), "{ 坏 JSON").unwrap();
        std::fs::write(dir.join("ignore.txt"), "[]").unwrap();

        let cmds = load_manifests(&dir);
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["计数", "单条"],
            "坏条目 / 坏文件应被跳过：{names:?}"
        );
        assert_eq!(cmds[0].category, "扩展", "缺省分组应补上");
        assert!(cmds[0].source.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真的能跑起来并拿到输出（echo 在两个平台都可用）。
    #[test]
    fn runs_and_captures_output() {
        let out = run("echo mo-cmd-ok", None).expect("echo 应当能跑起来");
        assert_eq!(out.code, Some(0));
        assert!(out.text.contains("mo-cmd-ok"), "{:?}", out.text);
    }
}
