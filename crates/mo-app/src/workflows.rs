//! 自动化工作流：把一串命令按顺序跑完，任一步失败就中止。
//!
//! ## 为什么是「顺序步骤 + 立即中止」
//!
//! 文件管理器的自动化场景基本都是「一串固定动作」：转码 → 挪进归档目录 →
//! 删原文件。这类流水线**后面几步依赖前面成功**——第 2 步失败还继续跑第 3 步，
//! 轻则白跑，重则把还没归档的原文件删掉。所以默认遇错即停，不做「尽力继续」。
//!
//! 步骤复用自定义命令的占位符与执行层（[`crate::usercmds`]），因此工作流里
//! 每一步能写的东西和单条命令完全一样：`{dir}` / `{file}` / `{files}`。
//!
//! ## 与「插件 / 扩展」的关系
//!
//! 扩展清单也可以带工作流（见 [`Workflow::source`]）：扩展负责提供能力，
//! 工作流负责把能力串成流程。两者共用同一套校验与执行代码。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::usercmds::{CommandContext, CommandOutput};
use mo_config::Workflow;

/// 校验：空名字、零步骤、步骤里有空串都算不合法。
pub fn validate(w: &Workflow) -> Option<String> {
    if w.name.trim().is_empty() {
        return Some("工作流缺少 name".to_string());
    }
    if w.steps.is_empty() {
        return Some(format!("工作流「{}」没有任何步骤", w.name));
    }
    if w.steps.iter().any(|s| s.trim().is_empty()) {
        return Some(format!("工作流「{}」有空步骤", w.name));
    }
    None
}

/// 一步的执行结果。
#[derive(Debug, Clone)]
pub struct StepResult {
    /// 展开后真正执行的命令行（回显给用户核对）。
    pub line: String,
    /// 退出码（None = 没跑起来）。
    pub code: Option<i32>,
    /// 合并后的输出。
    pub output: CommandOutput,
}

impl StepResult {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// 一次运行的结果。
#[derive(Debug, Clone, Default)]
pub struct WorkflowReport {
    pub workflow: String,
    /// 已执行的步骤（含失败那一步；被取消时 `steps` 里不含未跑的）。
    pub steps: Vec<StepResult>,
    /// 在第几步失败（0 起，None = 全部成功）。
    pub failed_at: Option<usize>,
    /// 是否在跑完前被取消。
    pub cancelled: bool,
    /// 因缺上下文而没能展开的提示。
    pub blocked: Option<String>,
}

impl WorkflowReport {
    pub fn succeeded(&self) -> bool {
        self.failed_at.is_none() && !self.cancelled && self.blocked.is_none()
    }
}

/// 按顺序执行工作流。
///
/// `cancel` 在**每一步之前**检查：步骤本身就是外部程序，跑一半强杀既不可靠
/// 也不是用户期待的语义（用户按取消 = 不要再开下一步）。
pub fn run_workflow(
    w: &Workflow,
    ctx: &CommandContext,
    cancel: &AtomicBool,
    mut on_step: impl FnMut(&str),
) -> WorkflowReport {
    let mut report = WorkflowReport {
        workflow: w.name.clone(),
        ..Default::default()
    };
    for (i, step) in w.steps.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            return report;
        }
        let (line, ok) = crate::usercmds::expand(step, ctx);
        if !ok {
            // 占位符缺上下文：整条工作流停下来，而不是拿半截命令去跑。
            report.blocked = Some(format!(
                "第 {} 步「{step}」需要选中条目，但当前没有选中任何文件",
                i + 1
            ));
            return report;
        }
        on_step(&line);
        let out = match crate::usercmds::run(&line, ctx.dir.as_deref()) {
            Ok(o) => o,
            Err(e) => {
                report.failed_at = Some(i);
                report.steps.push(StepResult {
                    line,
                    code: None,
                    output: CommandOutput {
                        code: None,
                        text: e,
                    },
                });
                return report;
            }
        };
        let good = out.code == Some(0);
        report.steps.push(StepResult {
            line,
            code: out.code,
            output: out,
        });
        if !good {
            report.failed_at = Some(i);
            return report;
        }
    }
    report
}

/// 校验一批工作流并丢掉不合法的（返回保留下来的）。
pub fn sanitize(list: Vec<Workflow>) -> Vec<Workflow> {
    list.into_iter()
        .filter(|w| match validate(w) {
            Some(err) => {
                tracing::warn!("忽略一个工作流：{err}");
                false
            }
            None => true,
        })
        .collect()
}

/// 从扩展清单里挑出工作流用的占位符上下文构造器（UI 侧调用）。
pub fn context_for(dir: Option<PathBuf>, selected: Vec<PathBuf>) -> CommandContext {
    CommandContext { dir, selected }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "mo-wf-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// 校验：空名 / 零步骤 / 空步骤都要被拒。
    #[test]
    fn validates_workflows() {
        assert!(validate(&Workflow {
            name: "".into(),
            steps: vec!["x".into()],
            source: None
        })
        .is_some());
        assert!(validate(&Workflow {
            name: "n".into(),
            steps: vec![],
            source: None
        })
        .is_some());
        assert!(
            validate(&Workflow {
                name: "n".into(),
                steps: vec!["ok".into(), " ".into()],
                source: None
            })
            .is_some(),
            "空步骤应被拒"
        );
        assert!(validate(&Workflow {
            name: "n".into(),
            steps: vec!["ok".into()],
            source: None
        })
        .is_none());
    }

    /// 多步顺序执行，全部成功。
    #[test]
    fn runs_steps_in_order() {
        let dir = tmp("ok");
        let w = Workflow {
            name: "两步".into(),
            steps: vec!["echo one".into(), "echo two".into()],
            source: None,
        };
        let ctx = CommandContext {
            dir: Some(dir.clone()),
            selected: vec![],
        };
        let r = run_workflow(&w, &ctx, &AtomicBool::new(false), |_| {});
        assert!(r.succeeded(), "{r:?}");
        assert_eq!(r.steps.len(), 2);
        assert!(r.steps[0].output.text.contains("one"));
        assert!(r.steps[1].output.text.contains("two"));
        fs::remove_dir_all(&dir).ok();
    }

    /// 第 2 步失败：必须停在第 2 步，绝不跑第 3 步（后面往往是要删原文件的动作）。
    #[test]
    fn stops_at_first_failure() {
        let dir = tmp("fail");
        let marker = dir.join("third-ran");
        let third = format!("echo x > \"{}\"", marker.display());
        let w = Workflow {
            name: "会失败".into(),
            steps: vec!["echo a".into(), "exit 3".into(), third],
            source: None,
        };
        let ctx = CommandContext {
            dir: Some(dir.clone()),
            selected: vec![],
        };
        let r = run_workflow(&w, &ctx, &AtomicBool::new(false), |_| {});
        assert_eq!(r.failed_at, Some(1), "应停在第 2 步：{r:?}");
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.steps[1].code, Some(3));
        assert!(!marker.exists(), "失败后的步骤绝不能执行");
        fs::remove_dir_all(&dir).ok();
    }

    /// 步骤用到 `{file}` 而没选中项：整条不跑，并说明原因。
    #[test]
    fn blocked_without_selection() {
        let dir = tmp("blocked");
        let w = Workflow {
            name: "要选中".into(),
            steps: vec!["wc -l {file}".into()],
            source: None,
        };
        let ctx = CommandContext {
            dir: Some(dir.clone()),
            selected: vec![],
        };
        let r = run_workflow(&w, &ctx, &AtomicBool::new(false), |_| {});
        assert!(r.blocked.is_some(), "缺上下文必须报出来：{r:?}");
        assert!(r.steps.is_empty(), "一步都不该跑");
        fs::remove_dir_all(&dir).ok();
    }

    /// 取消位：置了就一步都不跑。
    #[test]
    fn honours_cancel() {
        let dir = tmp("cancel");
        let w = Workflow {
            name: "取消".into(),
            steps: vec!["echo a".into()],
            source: None,
        };
        let ctx = CommandContext {
            dir: Some(dir.clone()),
            selected: vec![],
        };
        let r = run_workflow(&w, &ctx, &AtomicBool::new(true), |_| {});
        assert!(r.cancelled);
        assert!(r.steps.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    /// 每步执行前会回调展开后的命令行（UI 用它显示进度）。
    #[test]
    fn reports_expanded_lines() {
        let dir = tmp("lines");
        let f = dir.join("a.txt");
        fs::write(&f, "hi").unwrap();
        let w = Workflow {
            name: "带占位符".into(),
            steps: vec!["echo {file} && pwd".into()],
            source: None,
        };
        let ctx = CommandContext {
            dir: Some(dir.clone()),
            selected: vec![f.clone()],
        };
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let r = run_workflow(&w, &ctx, &AtomicBool::new(false), move |line| {
            sink.lock().unwrap().push(line.to_string());
        });
        assert!(r.succeeded(), "{r:?}");
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("a.txt"), "占位符应已展开：{}", got[0]);
        assert!(!got[0].contains("{file}"));
        fs::remove_dir_all(&dir).ok();
    }

    /// 一批里坏的被丢掉，好的留下。
    #[test]
    fn sanitize_drops_bad_ones() {
        let list = vec![
            Workflow {
                name: "好".into(),
                steps: vec!["echo a".into()],
                source: None,
            },
            Workflow {
                name: "".into(),
                steps: vec!["echo b".into()],
                source: None,
            },
            Workflow {
                name: "空".into(),
                steps: vec![],
                source: None,
            },
        ];
        let kept = sanitize(list);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "好");
    }
}
