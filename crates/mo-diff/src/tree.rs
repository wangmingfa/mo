//! 文件夹 vs 文件夹递归比较。
//!
//! 输出一个扁平的条目列表（相对路径 + 状态），渲染层可直接按顺序展示。
//! * 两侧同名目录：递归下钻；子树完全一致则**不列出**（只计数），
//!   有差异才作为一条 `is_dir: true, Different` 记录，作为「进入有差异」的导航标记。
//! * 一侧独有：整棵子树只记一条，不逐个罗列内部文件。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// 树比较结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeComparison {
    /// 左侧根目录。
    pub left: PathBuf,
    /// 右侧根目录。
    pub right: PathBuf,
    /// 按相对路径排序的条目（相同目录不列出）。
    pub entries: Vec<TreeEntry>,
    /// 内容完全一致的文件数。
    pub identical_files: usize,
    /// 内容完全一致的目录数（未列出）。
    pub identical_dirs: usize,
    /// 有差异的条目数（含差异目录）。
    pub different: usize,
    /// 仅左侧存在的条目数。
    pub left_only: usize,
    /// 仅右侧存在的条目数。
    pub right_only: usize,
}

impl TreeComparison {
    /// 是否完全一致（无任何差异）。
    pub fn is_identical(&self) -> bool {
        self.different == 0 && self.left_only == 0 && self.right_only == 0
    }
}

/// 一条树比较记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// 相对根目录的路径。
    pub rel: PathBuf,
    /// 状态。
    pub status: TreeStatus,
    /// 是否目录。
    pub is_dir: bool,
}

/// 条目状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeStatus {
    /// 两侧都有且内容一致。
    Identical,
    /// 两侧都有但内容不同。
    Different,
    /// 仅左侧存在。
    LeftOnly,
    /// 仅右侧存在。
    RightOnly,
}

/// 比较两棵目录树（递归，不限制深度）。
pub fn compare_trees(left: &Path, right: &Path) -> Result<TreeComparison, String> {
    for p in [left, right] {
        if !p.is_dir() {
            return Err(format!("{} 不是目录", p.display()));
        }
    }
    let mut out = TreeComparison {
        left: left.to_path_buf(),
        right: right.to_path_buf(),
        entries: Vec::new(),
        identical_files: 0,
        identical_dirs: 0,
        different: 0,
        left_only: 0,
        right_only: 0,
    };
    walk(left, right, Path::new(""), &mut out)?;
    Ok(out)
}

/// 递归下钻一层；返回该子树是否有差异。
fn walk(left: &Path, right: &Path, rel: &Path, out: &mut TreeComparison) -> Result<bool, String> {
    let ls = read_names(left)?;
    let rs = read_names(right)?;

    let mut dirty = false;
    // 并集（BTreeSet 保证有序去重 → 输出确定性）。
    let names: std::collections::BTreeSet<&String> = ls.keys().chain(rs.keys()).collect();
    for name in names {
        let child_rel = rel.join(name);
        let lp = left.join(name);
        let rp = right.join(name);
        let in_left = ls.contains_key(name);
        let in_right = rs.contains_key(name);

        match (in_left, in_right) {
            (true, true) => {
                let l_dir = lp.is_dir();
                let r_dir = rp.is_dir();
                if l_dir != r_dir {
                    // 类型都不同（一侧目录一侧文件），必然不同。
                    push(
                        out,
                        &child_rel,
                        TreeStatus::Different,
                        l_dir || r_dir,
                        &mut dirty,
                    );
                } else if l_dir {
                    // 两侧都是目录：下钻；子树干净则只计数不列出。
                    let sub_dirty = walk(&lp, &rp, &child_rel, out)?;
                    if sub_dirty {
                        push(out, &child_rel, TreeStatus::Different, true, &mut dirty);
                    } else {
                        out.identical_dirs += 1;
                    }
                } else {
                    // 两侧都是文件：逐字节比较。
                    let same = super::files::same_content(&lp, &rp)?;
                    let status = if same {
                        TreeStatus::Identical
                    } else {
                        TreeStatus::Different
                    };
                    push(out, &child_rel, status, false, &mut dirty);
                }
            }
            (true, false) => {
                push(
                    out,
                    &child_rel,
                    TreeStatus::LeftOnly,
                    lp.is_dir(),
                    &mut dirty,
                );
            }
            (false, true) => {
                push(
                    out,
                    &child_rel,
                    TreeStatus::RightOnly,
                    rp.is_dir(),
                    &mut dirty,
                );
            }
            (false, false) => unreachable!(),
        }
    }
    Ok(dirty)
}

/// 记录一条结果并累计计数；任何非 Identical 记录都让子树变「脏」。
fn push(out: &mut TreeComparison, rel: &Path, status: TreeStatus, is_dir: bool, dirty: &mut bool) {
    match status {
        TreeStatus::Identical => out.identical_files += 1,
        TreeStatus::Different => out.different += 1,
        TreeStatus::LeftOnly => out.left_only += 1,
        TreeStatus::RightOnly => out.right_only += 1,
    }
    if status != TreeStatus::Identical {
        *dirty = true;
    }
    out.entries.push(TreeEntry {
        rel: rel.to_path_buf(),
        status,
        is_dir,
    });
}

/// 读一层目录的文件名集合（有序，保证输出确定性）。
fn read_names(dir: &Path) -> Result<BTreeMap<String, ()>, String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("读取 {} 失败：{e}", dir.display()))?;
    let mut map = BTreeMap::new();
    for e in rd.flatten() {
        map.insert(e.file_name().to_string_lossy().to_string(), ());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 独立的临时目录。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!(
                "mo-diff-tree-{}-{}-{}",
                tag,
                std::process::id(),
                nanos
            ));
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn identical_trees() {
        let (l, r) = (TempDir::new("tl"), TempDir::new("tr"));
        for t in [&l, &r] {
            fs::write(t.path("a.txt"), "same").unwrap();
            fs::create_dir_all(t.path("sub")).unwrap();
            fs::write(t.path("sub/b.txt"), "same").unwrap();
        }
        let c = compare_trees(&l.0, &r.0).unwrap();
        assert!(c.is_identical());
        assert_eq!(c.identical_files, 2);
        assert_eq!(c.identical_dirs, 1);
        // 相同目录不列出：只有两个文件记录。
        assert_eq!(c.entries.len(), 2);
    }

    #[test]
    fn mixed_differences() {
        let (l, r) = (TempDir::new("ml"), TempDir::new("mr"));
        // 相同文件。
        fs::write(l.path("same.txt"), "x").unwrap();
        fs::write(r.path("same.txt"), "x").unwrap();
        // 不同文件。
        fs::write(l.path("diff.txt"), "old").unwrap();
        fs::write(r.path("diff.txt"), "new").unwrap();
        // 仅左 / 仅右。
        fs::write(l.path("only-left.txt"), "1").unwrap();
        fs::write(r.path("only-right.txt"), "2").unwrap();

        let c = compare_trees(&l.0, &r.0).unwrap();
        assert_eq!(c.identical_files, 1);
        assert_eq!(c.different, 1);
        assert_eq!(c.left_only, 1);
        assert_eq!(c.right_only, 1);
        assert!(!c.is_identical());
        // 输出按相对路径排序。
        let rels: Vec<&str> = c.entries.iter().map(|e| e.rel.to_str().unwrap()).collect();
        let mut sorted = rels.clone();
        sorted.sort();
        assert_eq!(rels, sorted);
    }

    #[test]
    fn differing_directory_is_listed_clean_one_is_not() {
        let (l, r) = (TempDir::new("dl"), TempDir::new("dr"));
        fs::create_dir_all(l.path("same-dir")).unwrap();
        fs::create_dir_all(r.path("same-dir")).unwrap();
        fs::write(l.path("same-dir/x.txt"), "1").unwrap();
        fs::write(r.path("same-dir/x.txt"), "1").unwrap();

        fs::create_dir_all(l.path("dirty-dir")).unwrap();
        fs::create_dir_all(r.path("dirty-dir")).unwrap();
        fs::write(l.path("dirty-dir/a.txt"), "1").unwrap();
        fs::write(r.path("dirty-dir/a.txt"), "2").unwrap();

        let c = compare_trees(&l.0, &r.0).unwrap();
        let dirs: Vec<&TreeEntry> = c.entries.iter().filter(|e| e.is_dir).collect();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].rel, PathBuf::from("dirty-dir"));
        assert_eq!(dirs[0].status, TreeStatus::Different);
        assert_eq!(c.identical_dirs, 1);
    }

    #[test]
    fn left_only_directory_counts_as_one_entry() {
        let (l, r) = (TempDir::new("ol"), TempDir::new("or"));
        fs::create_dir_all(l.path("whole/sub")).unwrap();
        fs::write(l.path("whole/sub/f.txt"), "1").unwrap();
        let c = compare_trees(&l.0, &r.0).unwrap();
        assert_eq!(c.left_only, 1);
        assert_eq!(c.entries.len(), 1);
        assert!(c.entries[0].is_dir);
    }

    #[test]
    fn dir_vs_file_with_same_name_is_different() {
        let (l, r) = (TempDir::new("xf"), TempDir::new("xr"));
        fs::create_dir_all(l.path("thing")).unwrap();
        fs::write(r.path("thing"), "file content").unwrap();
        let c = compare_trees(&l.0, &r.0).unwrap();
        assert_eq!(c.different, 1);
        assert_eq!(c.entries[0].status, TreeStatus::Different);
    }

    #[test]
    fn non_directory_rejected() {
        let t = TempDir::new("nd");
        let f = t.path("f.txt");
        fs::write(&f, "x").unwrap();
        assert!(compare_trees(&f, &t.0).is_err());
    }
}
