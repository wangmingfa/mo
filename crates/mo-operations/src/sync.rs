//! 文件夹同步：扫描两侧 → 生成计划 → 过目 → 执行。
//!
//! ## 为什么先出计划再执行
//!
//! 同步是本项目里**唯一会成批改动大量文件**的功能，一步算错就是几百个文件。
//! 所以引擎分成 `plan`（只读，算出要做什么）与 `apply`（照计划做）两步，UI
//! 必须先展示计划让用户过目，点了执行才动手。
//!
//! ## 判定规则
//!
//! 两侧都按「相对路径 → (字节数, 修改时间)」建表，然后逐个相对路径比较：
//!
//! * 两边都没有 → 不存在这种情况（表就是从两边来的）；
//! * 只有一边有 → 按模式决定复制方向；
//! * 两边都有且**字节数与修改时间都相同** → 视为已同步，跳过（不读文件内容，
//!   这是同步能在大目录上跑得起的前提）；
//! * 两边都有但不相同 → 冲突，交给 [`ConflictPolicy`]。
//!
//! ## 删除永不落地
//!
//! 镜像模式确实需要「删掉目标端多出来的文件」，但删的东西是用户数据。所以
//! 引擎把删除只写成计划项，执行时一律走调用方注入的 `deleter` 回调（应用层
//! 接回收站），本模块不出现任何永久删除调用。

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::time::SystemTime;

/// 同步方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
    /// 双向合并：两边独有的都补到对面，谁都不删谁。
    #[default]
    TwoWay,
    /// 单向镜像：把 `src` 复制成 `dst` 的样子（`dst` 多出来的可删）。
    Mirror,
}

/// 两边都有、内容不同时的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// 较新的一方覆盖较旧的一方。
    #[default]
    NewerWins,
    /// 两边都留着：把较旧的改名成 `*.冲突副本.<侧>.<原名>` 再放入较新的。
    KeepBoth,
    /// 冲突项一律不动，只在报告里列出，交给人判断。
    Skip,
}

/// 一次同步的选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncOptions {
    pub mode: SyncMode,
    pub conflict: ConflictPolicy,
    /// 镜像模式下是否删除目标端多出来的文件（走回收站，非永久删除）。
    pub delete_extras: bool,
}

/// 一个文件的指纹：字节数 + 修改时间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    size: u64,
    mtime: SystemTime,
}

/// 计划里的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 把 `rel` 从 src 复制到 dst。
    CopyToTarget { rel: String, size: u64 },
    /// 把 `rel` 从 dst 复制到 src（双向模式才有）。
    CopyToSource { rel: String, size: u64 },
    /// 冲突：较新的一方覆盖较旧的一方。
    Overwrite {
        rel: String,
        size: u64,
        /// `true` = 用 src 覆盖 dst；`false` = 用 dst 覆盖 src。
        to_target: bool,
    },
    /// 冲突（KeepBoth）：把较旧的改名保留，再放入较新的。
    KeepBoth {
        rel: String,
        size: u64,
        to_target: bool,
        /// 保留下来的旧文件在目标端的新名字。
        backup: String,
    },
    /// 冲突（Skip）：不动，只报告。
    ConflictSkipped { rel: String },
    /// 镜像模式：目标端多出来的文件送回收站。
    TrashInTarget { rel: String, size: u64 },
}

impl Action {
    /// 这一项要搬动的字节数（用于「预计传输量」）。
    pub fn bytes(&self) -> u64 {
        match self {
            Action::CopyToTarget { size, .. }
            | Action::CopyToSource { size, .. }
            | Action::Overwrite { size, .. }
            | Action::KeepBoth { size, .. } => *size,
            Action::ConflictSkipped { .. } | Action::TrashInTarget { .. } => 0,
        }
    }

    /// 人类可读的一行描述（计划列表直接显示它）。
    pub fn describe(&self) -> String {
        match self {
            Action::CopyToTarget { rel, .. } => format!("→ 复制到目标：{rel}"),
            Action::CopyToSource { rel, .. } => format!("← 复制到源端：{rel}"),
            Action::Overwrite { rel, to_target, .. } => format!(
                "≠ 覆盖{}：{rel}",
                if *to_target { "目标端" } else { "源端" }
            ),
            Action::KeepBoth {
                rel,
                backup,
                to_target,
                ..
            } => format!(
                "≠ 两边都留：{} 旧版存为 {backup}",
                if *to_target { "目标端" } else { "源端" }
            )
            .replacen("≠ 两边都留：", "≠ 冲突：", 1)
            .replacen(" 旧版存为", &format!(" {rel} 旧版存为"), 1),
            Action::ConflictSkipped { rel } => format!("= 冲突跳过：{rel}"),
            Action::TrashInTarget { rel, .. } => format!("✗ 目标端多余，移入回收站：{rel}"),
        }
    }
}

/// 一次扫描 + 比对得到的执行计划。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// 两侧都相同、直接跳过的文件数。
    pub identical: usize,
    /// 扫描到的文件总数（两侧去重后的相对路径数）。
    pub considered: usize,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// 预计要传输的字节数。
    pub fn total_bytes(&self) -> u64 {
        self.actions.iter().map(Action::bytes).sum()
    }
}

/// 递归收集一棵树：相对路径（`/` 分隔）→ 指纹。
///
/// 读不了的条目（权限、正在被删）只跳过并计数，不让整次扫描失败。
pub(crate) fn scan(root: &Path) -> (BTreeMap<String, Stamp>, usize) {
    let mut out = BTreeMap::new();
    let mut skipped = 0usize;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        // 符号链接整个剪掉：链接指向的内容不在这一侧，同步它只会把两边的
        // 目录结构搅乱。
        .filter_entry(|e| !e.file_type().is_symlink())
    {
        let Ok(e) = entry else {
            skipped += 1;
            continue;
        };
        if !e.file_type().is_file() {
            continue;
        }
        let Ok(meta) = e.metadata() else {
            skipped += 1;
            continue;
        };
        let Ok(rel) = e.path().strip_prefix(root) else {
            skipped += 1;
            continue;
        };
        // 统一成 `/` 分隔的相对路径：Map 的键要跨平台稳定，也要能拼回两侧。
        let key = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        out.insert(
            key,
            Stamp {
                size: meta.len(),
                mtime,
            },
        );
    }
    (out, skipped)
}

/// 冲突时给旧版留的底稿名：`a/b.txt` → `a/b.冲突副本.target.txt`。
fn backup_name(rel: &str, side: &str) -> String {
    let (dir, file) = match rel.rsplit_once('/') {
        Some((d, f)) => (format!("{d}/"), f.to_string()),
        None => (String::new(), rel.to_string()),
    };
    match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            format!("{dir}{stem}.冲突副本.{side}.{ext}")
        }
        _ => format!("{dir}{file}.冲突副本.{side}"),
    }
}

/// 比对两侧，生成执行计划。
pub fn plan(src: &Path, dst: &Path, opts: SyncOptions) -> Plan {
    let (a, _) = scan(src);
    let (b, _) = scan(dst);
    let mut out = Plan::default();
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    out.considered = keys.len();

    for rel in keys {
        match (a.get(rel), b.get(rel)) {
            (Some(sa), Some(sb)) => {
                if sa.size == sb.size && sa.mtime == sb.mtime {
                    out.identical += 1;
                    continue;
                }
                // 冲突：谁新谁赢。相等时按路径稳定地选一侧，避免同一对文件
                // 两次扫描给出不同计划。
                let src_newer = sa.mtime >= sb.mtime;
                match opts.conflict {
                    ConflictPolicy::Skip => out
                        .actions
                        .push(Action::ConflictSkipped { rel: rel.clone() }),
                    ConflictPolicy::NewerWins => out.actions.push(Action::Overwrite {
                        rel: rel.clone(),
                        size: (if src_newer { sa } else { sb }).size,
                        to_target: src_newer,
                    }),
                    ConflictPolicy::KeepBoth => out.actions.push(Action::KeepBoth {
                        rel: rel.clone(),
                        size: (if src_newer { sa } else { sb }).size,
                        to_target: src_newer,
                        backup: backup_name(rel, if src_newer { "target" } else { "source" }),
                    }),
                }
            }
            (Some(sa), None) => {
                out.actions.push(Action::CopyToTarget {
                    rel: rel.clone(),
                    size: sa.size,
                });
            }
            (None, Some(sb)) => match opts.mode {
                SyncMode::TwoWay => out.actions.push(Action::CopyToSource {
                    rel: rel.clone(),
                    size: sb.size,
                }),
                // 镜像：目标端多出来的要清掉；没开删除开关就只在计划里缺席，
                // 绝不擅自删用户文件。
                SyncMode::Mirror if opts.delete_extras => out.actions.push(Action::TrashInTarget {
                    rel: rel.clone(),
                    size: sb.size,
                }),
                SyncMode::Mirror => {}
            },
            (None, None) => {}
        }
    }
    out
}

/// 执行结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub copied: usize,
    pub trashed: usize,
    pub skipped: usize,
    /// 失败的项目（相对路径 + 原因）。计划里其余项照常执行。
    pub errors: Vec<(String, String)>,
}

/// 按计划执行。
///
/// `trash` 由调用方提供（应用层接回收站）：本模块不做任何永久删除。
pub fn apply(
    src: &Path,
    dst: &Path,
    p: &Plan,
    mut trash: impl FnMut(&Path) -> Result<(), String>,
) -> SyncReport {
    let mut rep = SyncReport {
        skipped: p.identical,
        ..Default::default()
    };
    for act in &p.actions {
        let r = match act {
            Action::CopyToTarget { rel, .. } => copy_over(src, dst, rel).map(|_| ()),
            Action::CopyToSource { rel, .. }
            | Action::Overwrite {
                rel,
                to_target: false,
                ..
            } => copy_over(dst, src, rel).map(|_| ()),
            Action::Overwrite {
                rel,
                to_target: true,
                ..
            } => copy_over(src, dst, rel).map(|_| ()),
            Action::KeepBoth {
                rel,
                to_target,
                backup,
                ..
            } => {
                // 先把要被覆盖的那一侧的旧版改名留下，再放新版。
                let (from_root, to_root) = if *to_target { (src, dst) } else { (dst, src) };
                let old = to_root.join(rel);
                let kept = to_root.join(backup);
                let step = (|| -> io::Result<()> {
                    if old.exists() {
                        if let Some(parent) = kept.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::rename(&old, &kept)?;
                    }
                    copy_over(from_root, to_root, rel)?;
                    Ok(())
                })();
                step.map(|_| ())
            }
            Action::ConflictSkipped { .. } => {
                rep.skipped += 1;
                continue;
            }
            Action::TrashInTarget { rel, .. } => {
                let victim = dst.join(rel);
                match trash(&victim) {
                    Ok(()) => {
                        rep.trashed += 1;
                        continue;
                    }
                    Err(e) => Err(io::Error::other(e)),
                }
            }
        };
        match r {
            Ok(()) => rep.copied += 1,
            Err(e) => {
                let name = match act {
                    Action::CopyToTarget { rel, .. }
                    | Action::CopyToSource { rel, .. }
                    | Action::Overwrite { rel, .. }
                    | Action::KeepBoth { rel, .. }
                    | Action::ConflictSkipped { rel }
                    | Action::TrashInTarget { rel, .. } => rel.clone(),
                };
                rep.errors.push((name, e.to_string()));
            }
        }
    }
    rep
}

/// 把 `from_root/rel` 复制到 `to_root/rel`，顺带补齐父目录。
fn copy_over(from_root: &Path, to_root: &Path, rel: &str) -> io::Result<u64> {
    let from = from_root.join(rel);
    let to = to_root.join(rel);
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&from, &to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "mo-sync-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&p, body).unwrap();
    }

    /// 把两侧 mtime 钉死，让「谁更新」在测试里是确定的。
    fn touch(root: &Path, rel: &str, at: SystemTime) {
        let f = fs::OpenOptions::new()
            .write(true)
            .open(root.join(rel))
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(at))
            .unwrap();
    }

    fn epoch(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    /// 只有一边有的文件：双向模式两边都补。
    #[test]
    fn two_way_copies_both_directions() {
        let a = tmp("2w-a");
        let b = tmp("2w-b");
        put(&a, "only-in-a.txt", "aaa");
        put(&b, "sub/only-in-b.txt", "bbb");

        let p = plan(&a, &b, SyncOptions::default());
        assert_eq!(p.actions.len(), 2, "{:?}", p.actions);
        assert!(p
            .actions
            .iter()
            .any(|x| matches!(x, Action::CopyToTarget { rel, .. } if rel == "only-in-a.txt")));
        assert!(p
            .actions
            .iter()
            .any(|x| matches!(x, Action::CopyToSource { rel, .. } if rel == "sub/only-in-b.txt")));
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    /// 字节数与修改时间都相同 → 视为已同步，一个动作都不该有。
    #[test]
    fn identical_files_are_skipped() {
        let a = tmp("same-a");
        let b = tmp("same-b");
        put(&a, "x.txt", "hello");
        put(&b, "x.txt", "hello");
        let mt = fs::metadata(a.join("x.txt")).unwrap().modified().unwrap();
        touch(&b, "x.txt", mt);

        let p = plan(&a, &b, SyncOptions::default());
        assert!(p.is_empty(), "已同步的文件不该产生动作：{:?}", p.actions);
        assert_eq!(p.identical, 1);
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    /// 冲突三种处置都要算对，且 KeepBoth 的底稿名保留扩展名。
    #[test]
    fn conflict_policies() {
        let a = tmp("cf-a");
        let b = tmp("cf-b");
        put(&a, "doc.md", "newer from a");
        put(&b, "doc.md", "older from b");
        touch(&a, "doc.md", epoch(2000));
        touch(&b, "doc.md", epoch(1000));

        let opts = |c: ConflictPolicy| SyncOptions {
            mode: SyncMode::TwoWay,
            conflict: c,
            delete_extras: false,
        };
        let p = plan(&a, &b, opts(ConflictPolicy::NewerWins));
        assert!(
            matches!(
                &p.actions[0],
                Action::Overwrite { to_target: true, size, .. } if *size == 12
            ),
            "{:?}",
            p.actions
        );

        let p = plan(&a, &b, opts(ConflictPolicy::Skip));
        assert!(matches!(&p.actions[0], Action::ConflictSkipped { rel } if rel == "doc.md"));

        let p = plan(&a, &b, opts(ConflictPolicy::KeepBoth));
        match &p.actions[0] {
            Action::KeepBoth {
                backup, to_target, ..
            } => {
                assert!(backup.ends_with(".冲突副本.target.md"), "{backup}");
                assert!(backup.starts_with("doc."), "底稿应与原文件同目录：{backup}");
                assert!(*to_target, "a 侧更新，应写入目标端");
            }
            other => panic!("意外动作：{other:?}"),
        }
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    /// 镜像模式：目标端多余的默认不动，开了开关才进回收站。
    #[test]
    fn mirror_deletes_only_when_opted_in() {
        let a = tmp("mi-a");
        let b = tmp("mi-b");
        put(&a, "keep.txt", "k");
        put(&b, "extra.txt", "e");

        let p = plan(
            &a,
            &b,
            SyncOptions {
                mode: SyncMode::Mirror,
                conflict: ConflictPolicy::NewerWins,
                delete_extras: false,
            },
        );
        assert!(p
            .actions
            .iter()
            .all(|x| !matches!(x, Action::TrashInTarget { .. })));

        let p = plan(
            &a,
            &b,
            SyncOptions {
                mode: SyncMode::Mirror,
                conflict: ConflictPolicy::NewerWins,
                delete_extras: true,
            },
        );
        assert!(p
            .actions
            .iter()
            .any(|x| matches!(x, Action::TrashInTarget { rel, .. } if rel == "extra.txt")));
        // 镜像不该反向复制：a 缺的文件不会从 b 补过来。
        assert!(!p
            .actions
            .iter()
            .any(|x| matches!(x, Action::CopyToSource { .. })));
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    /// 执行：按计划搬文件、建子目录、冲突覆盖，并如实报告。
    #[test]
    fn apply_executes_the_plan() {
        let a = tmp("ap-a");
        let b = tmp("ap-b");
        put(&a, "deep/nested/new.txt", "payload");
        put(&b, "old.txt", "stale");
        put(&a, "old.txt", "fresh");
        touch(&a, "old.txt", epoch(5000));
        touch(&b, "old.txt", epoch(1000));

        let p = plan(&a, &b, SyncOptions::default());
        let rep = apply(&a, &b, &p, |_| Ok(()));
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert_eq!(rep.copied, 2, "{rep:?}");
        assert_eq!(
            fs::read_to_string(b.join("deep/nested/new.txt")).unwrap(),
            "payload"
        );
        assert_eq!(fs::read_to_string(b.join("old.txt")).unwrap(), "fresh");
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    /// KeepBoth 执行后：旧版真的留下了，新版也到位。
    #[test]
    fn apply_keep_both_preserves_old_copy() {
        let a = tmp("kb-a");
        let b = tmp("kb-b");
        put(&a, "note.txt", "NEW");
        put(&b, "note.txt", "OLD");
        touch(&a, "note.txt", epoch(9000));
        touch(&b, "note.txt", epoch(1000));

        let p = plan(
            &a,
            &b,
            SyncOptions {
                mode: SyncMode::TwoWay,
                conflict: ConflictPolicy::KeepBoth,
                delete_extras: false,
            },
        );
        let rep = apply(&a, &b, &p, |_| Ok(()));
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert_eq!(fs::read_to_string(b.join("note.txt")).unwrap(), "NEW");
        let kept = b.join("note.冲突副本.target.txt");
        assert_eq!(fs::read_to_string(&kept).unwrap(), "OLD", "旧版必须还在");
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    /// 删除只走注入的回收站回调，引擎自己不删文件；回调失败要进 errors。
    #[test]
    fn deletions_go_through_the_injected_trash() {
        let a = tmp("tr-a");
        let b = tmp("tr-b");
        put(&a, "keep.txt", "k");
        put(&b, "extra.txt", "e");

        let opts = SyncOptions {
            mode: SyncMode::Mirror,
            conflict: ConflictPolicy::NewerWins,
            delete_extras: true,
        };
        let p = plan(&a, &b, opts);

        // 回调只是记下来，不真删：证明引擎没有自己 unlink。
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
        let sink = seen.clone();
        let rep = apply(&a, &b, &p, move |victim| {
            sink.lock().unwrap().push(victim.to_path_buf());
            Ok(())
        });
        assert_eq!(rep.trashed, 1, "{rep:?}");
        assert!(b.join("extra.txt").exists(), "引擎不该自己把文件删掉");
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(seen.lock().unwrap()[0].ends_with("extra.txt"));

        // 回调报错 → 计入 errors，不影响其它项。
        let rep = apply(&a, &b, &p, |_| Err("回收站不可用".to_string()));
        assert_eq!(rep.errors.len(), 1);
        assert!(rep.errors[0].1.contains("回收站不可用"));
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    /// 空目录、只有一边有目录：目录本身不进计划（只同步文件）。
    #[test]
    fn empty_dirs_do_not_produce_actions() {
        let a = tmp("ed-a");
        let b = tmp("ed-b");
        fs::create_dir_all(a.join("just/a/dir")).unwrap();
        fs::create_dir_all(b.join("empty")).unwrap();

        let p = plan(&a, &b, SyncOptions::default());
        assert!(p.is_empty(), "空目录不该产生同步动作：{:?}", p.actions);
        assert_eq!(p.considered, 0);
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }

    /// 底稿名：无扩展名 / 隐藏文件 / 多级路径都不能把目录部分吃掉。
    #[test]
    fn backup_name_handles_awkward_paths() {
        assert_eq!(backup_name("a/b.txt", "target"), "a/b.冲突副本.target.txt");
        assert_eq!(backup_name("plain", "source"), "plain.冲突副本.source");
        assert_eq!(
            backup_name("dir/.hidden", "target"),
            "dir/.hidden.冲突副本.target"
        );
        assert_eq!(
            backup_name("x.tar.gz", "target"),
            "x.tar.冲突副本.target.gz"
        );
    }
}
