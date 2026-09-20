//! 重复文件查找：三级漏斗，避免「一上来就把整个盘读一遍」。
//!
//! 判定顺序从便宜到贵，每一级只对**上一级没排除掉**的候选做：
//!
//! 1. **大小**：字节数不同的文件内容必然不同，一次 `metadata` 就能把绝大多数
//!    文件筛掉；
//! 2. **部分哈希**（首 4 KiB + 尾 4 KiB）：同大小但内容不同的文件（比如同样
//!    长度的日志、文本、镜像文件）绝大多数在这一级就被分开，而读取量只有
//!    8 KiB 而不是整个文件；
//! 3. **整体哈希**：只有前两级都撞上的才做全量读取，这一步才是真判定。
//!
//! 空文件（0 字节）不参与：它们数量极多、又不占空间，报出来只会淹没真结果。
//! 符号链接也不参与：链接本身没有内容，读它会跟随到目标，等于把「同一个文件」
//! 报成两个副本。

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use md5::{Digest, Md5};

/// 部分哈希读的首 / 尾字节数。
const PART: u64 = 4096;

/// 结果排序：可回收空间大的在前，同大小按第一条路径排（保证顺序稳定可断言）。
fn sort_groups(groups: &mut [DupGroup]) {
    groups.sort_by(|a, b| {
        b.wasted()
            .cmp(&a.wasted())
            .then_with(|| a.paths[0].cmp(&b.paths[0]))
    });
}

/// 一组内容完全相同的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupGroup {
    /// 单个文件的字节数。
    pub size: u64,
    /// 组内文件（按路径排序，第一个是「建议保留」的那份）。
    pub paths: Vec<PathBuf>,
}

impl DupGroup {
    /// 删掉其余副本后可以回收的字节数（保留第一份）。
    pub fn wasted(&self) -> u64 {
        self.paths.len().saturating_sub(1) as u64 * self.size
    }
}

/// 一次查找的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DedupReport {
    /// 重复组（按可回收空间从大到小排序）。
    pub groups: Vec<DupGroup>,
    /// 参与比较的普通文件数。
    pub scanned: usize,
    /// 因读不了而跳过的文件数（权限、占用中、消失）。
    pub skipped: usize,
    /// 是否在跑完前被取消。
    pub cancelled: bool,
}

impl DedupReport {
    /// 全部重复组一共能回收多少字节。
    pub fn total_wasted(&self) -> u64 {
        self.groups.iter().map(DupGroup::wasted).sum()
    }
}

/// 递归收集普通文件（不跟随符号链接，链接本身也排除）。
fn walk(root: &std::path::Path, out: &mut Vec<PathBuf>, skipped: &mut usize) {
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        // 符号链接整个剪掉：链接没有自己的内容，读它会跟随到目标，
        // 等于把同一个文件报成两个副本。
        .into_iter()
        .filter_entry(|e| !e.file_type().is_symlink())
    {
        match entry {
            Ok(e) if e.file_type().is_file() => {
                // 0 字节文件直接不进候选：数量多、又不占空间，只会淹没真结果。
                if e.metadata().ok().is_some_and(|m| m.len() > 0) {
                    out.push(e.path().to_path_buf());
                }
            }
            // 读不了的目录（权限不足、正在被删）只计数，不让整次扫描失败。
            Ok(_) => {}
            Err(_) => {
                *skipped += 1;
            }
        }
    }
}

/// 读首尾各 [`PART`] 字节做部分哈希；文件不超过 2×PART 时等价于整体哈希。
fn partial_hash(path: &std::path::Path) -> std::io::Result<Option<String>> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut h = Md5::new();
    if len <= PART * 2 {
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        h.update(&buf);
    } else {
        let mut head = vec![0u8; PART as usize];
        f.read_exact(&mut head)?;
        h.update(&head);
        use std::io::Seek;
        f.seek(std::io::SeekFrom::End(-(PART as i64)))?;
        let mut tail = vec![0u8; PART as usize];
        f.read_exact(&mut tail)?;
        h.update(&tail);
    }
    Ok(Some(format!("{:x}", h.finalize())))
}

/// 整体哈希（流式，不把文件整个读进内存）。
fn full_hash(path: &std::path::Path) -> std::io::Result<Option<String>> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Md5::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(Some(format!("{:x}", h.finalize())))
}

/// 在若干根目录下查找重复文件。
///
/// `cancel` 在每读一个文件前检查一次：整体哈希是这里最耗时的动作，
/// 取消粒度到文件级足够（用户按 Esc 到真正停下来最多差一个文件）。
pub fn find_duplicates(roots: &[PathBuf], cancel: &AtomicBool) -> DedupReport {
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for r in roots {
        walk(r, &mut files, &mut skipped);
    }

    // 1) 按大小分组：只保留「同大小至少两个」的候选。
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for p in &files {
        let Ok(meta) = std::fs::metadata(p) else {
            skipped += 1;
            continue;
        };
        if meta.len() == 0 {
            continue;
        }
        by_size.entry(meta.len()).or_default().push(p.clone());
    }
    let mut stage2: Vec<(u64, Vec<PathBuf>)> =
        by_size.into_iter().filter(|(_, v)| v.len() > 1).collect();
    stage2.sort();

    let mut report = DedupReport {
        groups: Vec::new(),
        scanned: files.len(),
        skipped,
        cancelled: false,
    };

    // 2) 部分哈希；3) 整体哈希。两级都只在「同组候选」内部做。
    for (size, cands) in stage2 {
        let mut by_partial: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for p in cands {
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                sort_groups(&mut report.groups);
                return report;
            }
            match partial_hash(&p) {
                Ok(Some(h)) => by_partial.entry(h).or_default().push(p),
                _ => report.skipped += 1,
            }
        }
        for (_, group) in by_partial {
            if group.len() < 2 {
                continue;
            }
            let mut by_full: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for p in group {
                if cancel.load(Ordering::Relaxed) {
                    report.cancelled = true;
                    sort_groups(&mut report.groups);
                    return report;
                }
                match full_hash(&p) {
                    Ok(Some(h)) => by_full.entry(h).or_default().push(p),
                    _ => report.skipped += 1,
                }
            }
            for (_, mut paths) in by_full {
                if paths.len() < 2 {
                    continue;
                }
                // 稳定顺序：第一个作为「建议保留」，也方便 UI 断言。
                paths.sort();
                report.groups.push(DupGroup { size, paths });
            }
        }
    }
    report.groups.sort_by(|a, b| {
        b.wasted()
            .cmp(&a.wasted())
            .then_with(|| a.paths[0].cmp(&b.paths[0]))
    });
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "mo-dedup-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &PathBuf, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&p, bytes).unwrap();
        p
    }

    fn run(roots: &[PathBuf]) -> DedupReport {
        find_duplicates(roots, &AtomicBool::new(false))
    }

    /// 完全相同的两个文件成组，不同的文件不进组。
    #[test]
    fn groups_identical_files_only() {
        let dir = tmp("basic");
        let body = vec![7u8; 1024];
        write(&dir, "a.bin", &body);
        write(&dir, "sub/b.bin", &body);
        write(&dir, "other.bin", &vec![9u8; 1024]);

        let r = run(std::slice::from_ref(&dir));
        assert_eq!(r.groups.len(), 1, "应只有一个重复组：{:?}", r.groups);
        assert_eq!(r.groups[0].paths.len(), 2);
        assert_eq!(r.groups[0].size, 1024);
        assert_eq!(r.groups[0].wasted(), 1024, "保留一份后可回收一份");
        fs::remove_dir_all(&dir).ok();
    }

    /// 大小相同但内容不同：不能因为「没比字节」就误判为重复。
    #[test]
    fn same_size_different_content_is_not_duplicate() {
        let dir = tmp("samesize");
        write(&dir, "a.bin", &vec![1u8; 4096]);
        write(&dir, "b.bin", &vec![2u8; 4096]);
        // 只有尾部不同的长文件：必须靠整体哈希才分得开。
        let mut x = vec![3u8; 20000];
        let mut y = x.clone();
        *x.last_mut().unwrap() = 1;
        *y.last_mut().unwrap() = 2;
        write(&dir, "tail-x.bin", &x);
        write(&dir, "tail-y.bin", &y);

        let r = run(std::slice::from_ref(&dir));
        assert!(
            r.groups.is_empty(),
            "内容都不同，不该报重复：{:?}",
            r.groups
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// 首尾相同、中间不同的大文件：部分哈希会留下候选，整体哈希必须否掉。
    #[test]
    fn partial_match_resolved_by_full_hash() {
        let dir = tmp("partial");
        let mut a = vec![0u8; 40000];
        let mut b = vec![0u8; 40000];
        for i in 15000..16000 {
            a[i] = 1;
            b[i] = 2;
        }
        write(&dir, "a.bin", &a);
        write(&dir, "b.bin", &b);

        let r = run(std::slice::from_ref(&dir));
        assert!(r.groups.is_empty(), "中间不同不是重复：{:?}", r.groups);
        fs::remove_dir_all(&dir).ok();
    }

    /// 多组结果按可回收空间从大到小排，空文件不参与。
    #[test]
    fn sorts_by_wasted_and_ignores_empty() {
        let dir = tmp("order");
        write(&dir, "big1.bin", &vec![5u8; 9000]);
        write(&dir, "big2.bin", &vec![5u8; 9000]);
        write(&dir, "small1.bin", &vec![6u8; 100]);
        write(&dir, "small2.bin", &vec![6u8; 100]);
        write(&dir, "empty1.bin", b"");
        write(&dir, "empty2.bin", b"");

        let r = run(std::slice::from_ref(&dir));
        assert_eq!(r.groups.len(), 2, "{:?}", r.groups);
        assert_eq!(r.groups[0].size, 9000, "大的那组应排在前面");
        assert_eq!(r.total_wasted(), 9000 + 100);
        assert!(r.groups.iter().all(|g| g.size > 0), "0 字节文件不该进结果");
        fs::remove_dir_all(&dir).ok();
    }

    /// 取消：置位后立刻停下，并如实标记 cancelled。
    #[test]
    fn cancel_stops_the_scan() {
        let dir = tmp("cancel");
        for i in 0..8 {
            write(&dir, &format!("a{i}.bin"), &vec![i as u8; 5000]);
            write(&dir, &format!("b{i}.bin"), &vec![i as u8; 5000]);
        }
        let cancel = AtomicBool::new(true);
        let r = find_duplicates(std::slice::from_ref(&dir), &cancel);
        assert!(r.cancelled, "置了取消位就必须报告被取消");
        assert!(r.groups.is_empty(), "被取消时不该给出结果");
        fs::remove_dir_all(&dir).ok();
    }

    /// 多个根目录一起扫（跨目录副本是最常见的重复来源）。
    #[test]
    fn scans_multiple_roots() {
        let a = tmp("roots-a");
        let b = tmp("roots-b");
        write(&a, "same.bin", b"hello hello hello");
        write(&b, "copy.bin", b"hello hello hello");

        let r = run(&[a.clone(), b.clone()]);
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.groups[0].paths.len(), 2);
        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }
}
