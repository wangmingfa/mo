//! mo-diff：文件 / 文件夹比较引擎（纯逻辑，无 UI、无第三方依赖）。
//!
//! * [`text`]：Myers O(ND) 行级 diff，输出 [`text::DiffOp`] 区块序列；
//! * [`files`]：文件 vs 文件（大小捷径 → 流式字节比对 → 文本行级 diff / 二进制标记）；
//! * [`tree`]：文件夹 vs 文件夹递归比较，输出按相对路径排序的扁平条目列表。
//!
//! 跟 mo-core 一样不依赖 GPUI，全部逻辑可单独测试。

pub mod files;
pub mod text;
pub mod tree;

pub use files::{compare_files, FileComparison, FileStatus};
pub use text::{text_diff, DiffOp, TextDiff};
pub use tree::{compare_trees, TreeComparison, TreeEntry, TreeStatus};

use std::path::Path;

/// 顶层比较入口：根据两侧条目类型自动选择文件比较或树比较。
pub fn compare(a: &Path, b: &Path) -> Result<Comparison, String> {
    let (da, db) = (a.is_dir(), b.is_dir());
    match (da, db) {
        (true, true) => Ok(Comparison::Trees(compare_trees(a, b)?)),
        (false, false) => Ok(Comparison::Files(compare_files(a, b)?)),
        _ => Err("目录只能与目录比较，文件只能与文件比较".to_string()),
    }
}

/// 一次比较的完整结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comparison {
    /// 文件 vs 文件。
    Files(FileComparison),
    /// 文件夹 vs 文件夹。
    Trees(TreeComparison),
}

impl Comparison {
    /// 两侧路径（用于模态标题展示）。
    pub fn paths(&self) -> (&Path, &Path) {
        match self {
            Comparison::Files(f) => (&f.a, &f.b),
            Comparison::Trees(t) => (&t.left, &t.right),
        }
    }

    /// 是否完全一致。
    pub fn is_identical(&self) -> bool {
        match self {
            Comparison::Files(f) => f.status.is_identical(),
            Comparison::Trees(t) => t.is_identical(),
        }
    }
}
