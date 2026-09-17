//! 文件 vs 文件比较：先走快速路径，再尝试行级 diff。
//!
//! * 大小不同 → 直接判 Different（仍可对文本做行级 diff）；
//! * 内容逐块流式比对 → 完全一致判 Identical；
//! * 二者都失败时，若两文件都是合法 UTF-8 且不超过 4 MiB → 行级 diff；
//!   否则标记为二进制差异。

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::text::TextDiff;

/// 单文件比较结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileComparison {
    /// 左侧路径。
    pub a: PathBuf,
    /// 右侧路径。
    pub b: PathBuf,
    /// 左侧大小（字节）。
    pub a_size: u64,
    /// 右侧大小（字节）。
    pub b_size: u64,
    /// 差异详情。
    pub status: FileStatus,
}

/// 差异详情：完全相同 / 文本差异 / 二进制差异。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    /// 字节级完全一致。
    Identical,
    /// 不同，且两侧都是文本：携带行级 diff。
    TextDiff(TextDiff),
    /// 不同，且无法按文本呈现（二进制或超出文本大小上限）。
    BinaryDiff,
}

impl FileStatus {
    /// 是否完全相同。
    pub fn is_identical(&self) -> bool {
        matches!(self, FileStatus::Identical)
    }
}

/// 文本行级 diff 的大小上限：超过按二进制处理（避免把几百 MB 的日志读进内存）。
const TEXT_LIMIT: u64 = 4 * 1024 * 1024;

/// 比较两个普通文件。
pub fn compare_files(a: &Path, b: &Path) -> Result<FileComparison, String> {
    for p in [a, b] {
        if !p.is_file() {
            return Err(format!("{} 不是普通文件", p.display()));
        }
    }
    let (a_size, b_size) = match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => (ma.len(), mb.len()),
        (Err(e), _) | (_, Err(e)) => return Err(format!("读取元数据失败：{e}")),
    };

    if a_size == b_size && same_bytes(a, b)? {
        return Ok(FileComparison {
            a: a.to_path_buf(),
            b: b.to_path_buf(),
            a_size,
            b_size,
            status: FileStatus::Identical,
        });
    }

    // 尝试文本 diff：两侧都读得动且是合法 UTF-8。
    if a_size <= TEXT_LIMIT && b_size <= TEXT_LIMIT {
        if let (Ok(ta), Ok(tb)) = (std::fs::read_to_string(a), std::fs::read_to_string(b)) {
            return Ok(FileComparison {
                a: a.to_path_buf(),
                b: b.to_path_buf(),
                a_size,
                b_size,
                status: FileStatus::TextDiff(crate::text::text_diff(&ta, &tb)),
            });
        }
    }

    Ok(FileComparison {
        a: a.to_path_buf(),
        b: b.to_path_buf(),
        a_size,
        b_size,
        status: FileStatus::BinaryDiff,
    })
}

/// 供树比较复用的公开入口：两文件内容是否一致（大小不同直接判否）。
pub fn same_content(a: &Path, b: &Path) -> Result<bool, String> {
    same_bytes(a, b)
}

/// 流式逐块比较两个文件的字节内容（大小不同直接判否）。
fn same_bytes(a: &Path, b: &Path) -> Result<bool, String> {
    let mut fa = File::open(a).map_err(|e| format!("打开 {} 失败：{e}", a.display()))?;
    let mut fb = File::open(b).map_err(|e| format!("打开 {} 失败：{e}", b.display()))?;
    let mut ba = [0u8; 64 * 1024];
    let mut bb = [0u8; 64 * 1024];
    loop {
        let na = fa.read(&mut ba).map_err(|e| format!("读取失败：{e}"))?;
        let nb = fb.read(&mut bb).map_err(|e| format!("读取失败：{e}"))?;
        if na != nb {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
        if ba[..na] != bb[..nb] {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DiffOp;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 独立的临时目录（不引第三方依赖，用进程时间戳隔离）。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!(
                "mo-diff-{}-{}-{}",
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
    fn identical_files() {
        let t = TempDir::new("same");
        let a = t.path("a.txt");
        let b = t.path("b.txt");
        fs::write(&a, b"hello\nworld\n").unwrap();
        fs::write(&b, b"hello\nworld\n").unwrap();
        let r = compare_files(&a, &b).unwrap();
        assert_eq!(r.status, FileStatus::Identical);
        assert_eq!(r.a_size, 12);
    }

    #[test]
    fn different_text_files_get_line_diff() {
        let t = TempDir::new("text");
        let a = t.path("a.txt");
        let b = t.path("b.txt");
        fs::write(&a, "one\ntwo\nthree\n").unwrap();
        fs::write(&b, "one\nTWO\nthree\n").unwrap();
        let r = compare_files(&a, &b).unwrap();
        let FileStatus::TextDiff(td) = r.status else {
            panic!("应为文本差异");
        };
        assert_eq!(td.a_lines.len(), 3);
        assert_eq!(td.b_lines.len(), 3);
        // 恰好中间一行不同。
        let changes = td
            .ops
            .iter()
            .filter(|op| !matches!(op, DiffOp::Equal { .. }))
            .count();
        assert_eq!(changes, 2); // Delete + Insert
    }

    #[test]
    fn different_sizes_same_text_prefix() {
        let t = TempDir::new("size");
        let a = t.path("a.txt");
        let b = t.path("b.txt");
        fs::write(&a, "a\nb\n").unwrap();
        fs::write(&b, "a\nb\nc\n").unwrap();
        let r = compare_files(&a, &b).unwrap();
        let FileStatus::TextDiff(td) = r.status else {
            panic!("应为文本差异");
        };
        assert!(td.ops.contains(&DiffOp::Insert { new: 2, count: 1 }));
    }

    #[test]
    fn binary_files_report_binary_diff() {
        let t = TempDir::new("bin");
        let a = t.path("a.bin");
        let b = t.path("b.bin");
        // 非法 UTF-8 → 走二进制路径。
        fs::write(&a, [0xff, 0xfe, 0x00, 0x01]).unwrap();
        fs::write(&b, [0xff, 0xfe, 0x00, 0x02]).unwrap();
        let r = compare_files(&a, &b).unwrap();
        assert_eq!(r.status, FileStatus::BinaryDiff);
    }

    #[test]
    fn non_file_rejected() {
        let t = TempDir::new("dir");
        let a = t.path("subdir");
        fs::create_dir_all(&a).unwrap();
        let b = t.path("file.txt");
        fs::write(&b, b"x").unwrap();
        assert!(compare_files(&a, &b).is_err());
    }
}
