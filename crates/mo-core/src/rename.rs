//! rename：批量重命名的**纯规则**（不碰文件系统，便于单测）。
//!
//! UI 只负责把「选中的文件名 + 用户填的规则」交给 [`plan_batch_rename`]，
//! 得到目标名后再交给 `mo-app` 逐个提交 `RenameOperation`——
//! 规则部分与磁盘完全解耦，因此可以在这里做充分的边界测试。

/// 批量重命名规则。
#[derive(Debug, Clone, Default)]
pub struct RenameSpec {
    /// 查找（空串表示不替换）。
    pub find: String,
    /// 替换为。
    pub replace: String,
    /// 统一前缀。
    pub prefix: String,
    /// 统一后缀（在扩展名之前）。
    pub suffix: String,
    /// 用序号代替原文件名。
    pub use_index: bool,
    /// 起始序号。
    pub index_start: usize,
    /// 序号宽度（不足补 0）。
    pub index_width: usize,
    /// 保留原扩展名（序号 / 后缀都加在扩展名之前）。
    pub keep_extension: bool,
}

/// 目标名是否为空（全空规则会产生空名，UI 需要挡住）。
pub fn is_noop(spec: &RenameSpec) -> bool {
    spec.find.is_empty()
        && spec.replace.is_empty()
        && spec.prefix.is_empty()
        && spec.suffix.is_empty()
        && !spec.use_index
}

/// 按规则生成目标名；与 `names` 等长。
///
/// 顺序号按传入顺序递增，起始值与宽度取自 `spec`。
pub fn plan_batch_rename(names: &[String], spec: &RenameSpec) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| rename_one(name, spec, i))
        .collect()
}

fn rename_one(name: &str, spec: &RenameSpec, i: usize) -> String {
    let (stem, ext) = split_ext(name);
    // 保留扩展名时，替换 / 序号 / 后缀都只作用于主名，扩展名原样保留；
    // 否则按整个文件名处理（后缀会加在 .pdf 之后）。
    let keep = spec.keep_extension && !ext.is_empty();
    let base = if keep { stem } else { name };
    let body = if spec.use_index {
        let n = spec.index_start + i;
        let width = spec.index_width.max(1);
        format!("{n:0width$}", width = width)
    } else if !spec.find.is_empty() {
        base.replace(&spec.find, &spec.replace)
    } else {
        base.to_string()
    };
    let mut out = format!("{}{}{}", spec.prefix, body, spec.suffix);
    if keep {
        out.push('.');
        out.push_str(ext);
    }
    out
}

/// 拆出「主名 / 扩展名」：点号在首位（如 `.gitignore`）视为无扩展名。
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(pos) if pos > 0 => (&name[..pos], &name[pos + 1..]),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> RenameSpec {
        RenameSpec::default()
    }

    #[test]
    fn replaces_substring_in_stem_only() {
        let s = RenameSpec {
            find: "img".into(),
            replace: "photo".into(),
            keep_extension: true,
            ..spec()
        };
        assert_eq!(
            plan_batch_rename(&["img.photo.png".into(), "img.png".into()], &s),
            vec!["photo.photo.png".to_string(), "photo.png".to_string()]
        );
    }

    #[test]
    fn numbering_pads_to_width_and_keeps_extension() {
        let s = RenameSpec {
            use_index: true,
            index_start: 1,
            index_width: 3,
            prefix: "shot-".into(),
            keep_extension: true,
            ..spec()
        };
        assert_eq!(
            plan_batch_rename(&["a.png".into(), "b.jpg".into()], &s),
            vec!["shot-001.png".to_string(), "shot-002.jpg".to_string()]
        );
    }

    #[test]
    fn suffix_goes_before_extension_when_kept() {
        let s = RenameSpec {
            suffix: "-old".into(),
            keep_extension: true,
            ..spec()
        };
        assert_eq!(
            plan_batch_rename(&["report.pdf".into()], &s),
            vec!["report-old.pdf".to_string()]
        );
        let s2 = RenameSpec {
            suffix: "-old".into(),
            ..spec()
        };
        // 不保留扩展名时按整名处理：后缀直接加在末尾。
        assert_eq!(
            plan_batch_rename(&["report.pdf".into()], &s2),
            vec!["report.pdf-old".to_string()]
        );
    }

    #[test]
    fn dotfiles_have_no_extension() {
        let s = RenameSpec {
            prefix: "new-".into(),
            keep_extension: true,
            ..spec()
        };
        assert_eq!(
            plan_batch_rename(&[".gitignore".into()], &s),
            vec!["new-.gitignore".to_string()]
        );
    }

    #[test]
    fn empty_spec_is_a_noop() {
        assert!(is_noop(&RenameSpec::default()));
        assert!(!is_noop(&RenameSpec {
            prefix: "x".into(),
            ..spec()
        }));
    }
}
