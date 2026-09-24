//! content：按**文件内容**搜索（grep）。
//!
//! 与 [`crate::index`] 的分工：索引只存文件名，回答「哪个文件叫这个」；这里回答
//! 「哪个文件里写着这个」——后者没法靠索引，只能真的去读。所以它天生更贵，
//! 设计上全部围绕「把钱花在刀刃上」：
//!
//! * **先窄后读**：先用文件名 glob / 隐藏 / 体积三道筛子挡掉不相关的文件，
//!   再决定要不要读字节（`max_file_bytes` 之外的文件连打开都不打开）；
//! * **二进制嗅探**：读了前 [`SNIFF_BYTES`] 字节发现有 NUL 或不是合法 UTF-8，
//!   立刻放弃——在 `node_modules` 里 grep 不该把 `.png` 也逐字节扫一遍；
//! * **预算**：命中数撞到 `max_hits` 就停并置 `truncated`，宁可少给几条也不
//!   让一次搜索跑几分钟（与索引爬取同一个取舍，见 [`crate::crawl`]）；
//! * **可中断**：`stop` 一置位就在下一个文件边界退出，结果照常返回。
//!
//! ⚠️ 只读**本地**文件（`std::fs`）：远程后端要 grep 意味着把每个文件都下载
//! 一遍，那是另一种成本模型，得走别的入口（届时把「读字节」抽成一个参数）。
//! 调用方（mo-app）会在远程目录上直接拒绝这个命令。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 嗅探窗口：只读前这么多字节判断是不是二进制。
///
/// 8 KiB 足够抓到几乎所有二进制头（PNG / PDF / Mach-O / SQLite 前几字节就有
/// NUL），又不至于为了判断而把整个大文件读进来。
const SNIFF_BYTES: usize = 8 * 1024;

/// 单文件体积上限的默认：8 MiB。
///
/// 日志 / 转储 / 单文件数据库动辄几百 MB，逐行扫它们会吃掉整次搜索的预算，
/// 而用户几乎不会想在里面找东西——「太大了，跳过」比「卡住」可接受得多。
pub const DEFAULT_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// 一次搜索的命中行预算（不含上下文行）。
pub const DEFAULT_MAX_HITS: usize = 500;

/// 单个文件最多报多少条命中（防止一个巨型 minified js 吃满整屏）。
pub const DEFAULT_MAX_PER_FILE: usize = 20;

/// 内容搜索的查询条件。
///
/// 全是可复制的纯数据：`search_content` 是同步阻塞函数，调用方把它丢进
/// blocking 池（mo-app 侧已保证），这里不需要任何共享状态。
#[derive(Debug, Clone)]
pub struct ContentQuery {
    /// 搜索词：正则模式下是正则源码，否则是普通子串。
    pub pattern: String,
    /// `true` = 按正则解释 `pattern`。
    pub regex: bool,
    /// `true` = 区分大小写（默认不区分：找 `TODO` 时不该漏掉 `todo`）。
    pub case_sensitive: bool,
    /// `true` = 全词匹配（`foo` 不命中 `foobar`）。
    pub whole_word: bool,
    /// 只搜文件名匹配这些模式的文件（空 = 不限制）。支持 `*` 与 `?`。
    pub include: Vec<String>,
    /// 跳过文件名匹配这些模式的文件。
    pub exclude: Vec<String>,
    /// 超过这个体积的文件直接跳过（`0` = 不限制）。
    pub max_file_bytes: u64,
    /// 递归深度（`0` = 不限）。
    pub max_depth: usize,
    /// 跳过隐藏条目（与列表「显示隐藏文件」同一判据）。
    pub skip_hidden: bool,
    /// 命中行预算。
    pub max_hits: usize,
    /// 单文件命中上限。
    pub max_per_file: usize,
    /// 命中行上下各带多少行上下文（`0` = 只给命中行）。
    pub context: usize,
}

impl ContentQuery {
    /// 只带搜索词的查询，其余取默认（不区分大小写的子串搜索）。
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_depth: 0,
            skip_hidden: true,
            max_hits: DEFAULT_MAX_HITS,
            max_per_file: DEFAULT_MAX_PER_FILE,
            context: 0,
        }
    }
}

/// 一行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineHit {
    /// 行号（**1 开始**，与编辑器一致）。
    pub line: usize,
    /// 行内容（已经去掉换行符；超长行不截断，UI 自己负责省略）。
    pub text: String,
    /// 命中区间，按 **字符** 下标（不是字节）——UI 要按它切三段上色，
    /// 字节下标在多字节字符上会把一个汉字切坏。
    pub spans: Vec<(usize, usize)>,
    /// `true` = 这一行只是上下文（跟着命中行带出来的），本身没有命中。
    pub context: bool,
}

/// 一个文件里的所有命中。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    pub path: PathBuf,
    pub name: String,
    pub lines: Vec<LineHit>,
}

/// 一次内容搜索的结果汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentReport {
    /// 命中的文件（按路径排序）。
    pub files: Vec<FileHit>,
    /// 真正读了内容的文件数。
    pub scanned: usize,
    /// 有命中的文件数（= `files.len()`，留着给 UI 少算一次）。
    pub files_matched: usize,
    /// 命中行数（不含上下文行）。
    pub hits: usize,
    /// 撞到预算 / 深度上限而提前收工。
    pub truncated: bool,
    /// 因二进制（或不是 UTF-8）而跳过的文件数。
    pub skipped_binary: usize,
}

/// 编译后的匹配器：字面量走手写的字符扫描，正则交给 `regex`。
enum Matcher {
    Literal {
        /// 搜索词的字符序列（已按需折叠大小写）。
        needle: Vec<char>,
        case_sensitive: bool,
        whole_word: bool,
    },
    Regex(regex::Regex),
}

impl Matcher {
    fn compile(q: &ContentQuery) -> Result<Self, String> {
        if q.pattern.is_empty() {
            return Err("搜索词是空的".to_string());
        }
        if q.regex {
            let mut b = regex::RegexBuilder::new(&q.pattern);
            b.case_insensitive(!q.case_sensitive);
            // 单行里找就行：`search_content` 本来就是逐行喂进来的。
            b.multi_line(false);
            b.size_limit(1 << 20);
            return b.build().map(Matcher::Regex).map_err(|e| e.to_string());
        }
        let needle: Vec<char> = if q.case_sensitive {
            q.pattern.chars().collect()
        } else {
            // 折叠到小写再存：比较时两侧都折叠，大小写就无关了。
            q.pattern.chars().flat_map(char::to_lowercase).collect()
        };
        if needle.is_empty() {
            return Err("搜索词是空的".to_string());
        }
        Ok(Matcher::Literal {
            needle,
            case_sensitive: q.case_sensitive,
            whole_word: q.whole_word,
        })
    }

    /// 返回一行里所有命中区间（**字符**下标，左闭右开，互不重叠）。
    fn spans_in(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            Matcher::Regex(re) => re
                .find_iter(line)
                .map(|m| {
                    // 字节下标 → 字符下标：直接按字节切会破坏汉字。
                    let a = line[..m.start()].chars().count();
                    let b = a + line[m.start()..m.end()].chars().count();
                    (a, b)
                })
                .filter(|(a, b)| b > a)
                .collect(),
            Matcher::Literal {
                needle,
                case_sensitive,
                whole_word,
            } => {
                let chars: Vec<char> = line.chars().collect();
                let mut out = Vec::new();
                let mut i = 0usize;
                while i + needle.len() <= chars.len() {
                    let hit = if *case_sensitive {
                        chars[i..i + needle.len()] == needle[..]
                    } else {
                        // 逐字符折叠后比：不改下标，所以下标始终指向原串，
                        // 不会出现「折叠后长度变了、高亮错位」。
                        chars[i..i + needle.len()]
                            .iter()
                            .zip(needle.iter())
                            .all(|(c, n)| (*c).to_lowercase().eq((*n).to_lowercase()))
                    };
                    if hit && (!*whole_word || word_at(&chars, i, needle.len())) {
                        out.push((i, i + needle.len()));
                        i += needle.len();
                    } else {
                        i += 1;
                    }
                }
                out
            }
        }
    }
}

/// `[i, i+len)` 是不是一个「整词」：两边都不是词字符（字母 / 数字 / `_`）。
///
/// 用 `is_alphanumeric` 而不是 `is_ascii_alphanumeric`：搜中文词时「的」左右
/// 是汉字也算词内，`whole_word` 才不会形同虚设。
fn word_at(chars: &[char], i: usize, len: usize) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let before = i.checked_sub(1).and_then(|p| chars.get(p).copied());
    let after = chars.get(i + len).copied();
    !before.is_some_and(is_word) && !after.is_some_and(is_word)
}

/// 文件名 glob：只支持 `*` 与 `?`，且**不区分大小写**。
///
/// 不引第三方 glob 库是有意的：这里要的只是「`*.rs`」「`test_*`」这种量级，
/// 一个 20 行的函数就够，而真 glob 语义（`**`、`[a-z]`、路径分段）在这个
/// 场景里只会让人写出意料之外的模式。
pub fn name_matches(name: &str, pattern: &str) -> bool {
    let n: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    // 经典双指针 + 星号回溯：O(len(n) × 星号数)，名字长度是个位数级，够用。
    let (mut ni, mut pi) = (0usize, 0usize);
    let (mut star, mut star_n) = (None::<usize>, 0usize);
    while ni < n.len() {
        match p.get(pi).copied() {
            Some('*') => {
                star = Some(pi);
                star_n = ni;
                pi += 1;
            }
            Some(c) if c == '?' || c == n[ni] => {
                ni += 1;
                pi += 1;
            }
            // 配不上：退回最近的 `*` 让它多吃一个字符，再往后试。
            _ => match star {
                Some(s) => {
                    star_n += 1;
                    ni = star_n;
                    pi = s + 1;
                }
                None => return false,
            },
        }
    }
    // 名字吃完了：剩下的模式必须全是 `*`。
    p[pi..].iter().all(|c| *c == '*')
}

/// 这个文件要不要读（三道筛子：隐藏 → 体积 → glob）。
fn wanted(path: &Path, name: &str, q: &ContentQuery) -> bool {
    if q.skip_hidden && name.starts_with('.') {
        return false;
    }
    if !q.include.is_empty() && !q.include.iter().any(|p| name_matches(name, p)) {
        return false;
    }
    if q.exclude.iter().any(|p| name_matches(name, p)) {
        return false;
    }
    if q.max_file_bytes > 0 {
        // 拿不到体积（权限）就放过去：读的时候还有一层保护，不至于出错。
        if let Ok(m) = std::fs::metadata(path) {
            if m.len() > q.max_file_bytes {
                return false;
            }
        }
    }
    true
}

/// 在 `root` 下按内容搜索。返回 [`ContentReport`]。
///
/// **阻塞**：全程同步 IO，调用方必须在 blocking 池里跑。
/// `stop` 置位后在下一个文件边界停下，已找到的结果照常返回。
pub fn search_content(
    root: &Path,
    q: &ContentQuery,
    stop: &AtomicBool,
) -> anyhow::Result<ContentReport> {
    let matcher = Matcher::compile(q).map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(root, 0, q, stop, &mut files);

    let mut report = ContentReport::default();
    for path in files {
        if stop.load(Ordering::Relaxed) {
            report.truncated = true;
            break;
        }
        if report.hits >= q.max_hits {
            report.truncated = true;
            break;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            // 权限 / 竞态消失的文件：跳过，不让一颗老鼠屎坏一锅。
            Err(_) => continue,
        };
        if looks_binary(&bytes) {
            report.skipped_binary += 1;
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            report.skipped_binary += 1;
            continue;
        };
        report.scanned += 1;

        let mut hit_lines: BTreeSet<usize> = BTreeSet::new();
        let mut spans_by_line: Vec<(usize, Vec<(usize, usize)>)> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let spans = matcher.spans_in(line);
            if !spans.is_empty() {
                hit_lines.insert(i);
                spans_by_line.push((i, spans));
            }
            if spans_by_line.len() >= q.max_per_file {
                break;
            }
        }
        if spans_by_line.is_empty() {
            continue;
        }
        // 命中行 ± context，用有序集合合并重叠区间（相邻命中不会重复给行）。
        let all: Vec<String> = text.lines().map(|s| s.to_string()).collect();
        let mut wanted_lines: BTreeSet<usize> = BTreeSet::new();
        for &i in &hit_lines {
            let from = i.saturating_sub(q.context);
            let to = (i + q.context + 1).min(all.len());
            for k in from..to {
                wanted_lines.insert(k);
            }
        }
        let mut lines = Vec::new();
        for k in wanted_lines {
            let spans = spans_by_line
                .iter()
                .find(|(i, _)| *i == k)
                .map(|(_, s)| s.clone())
                .unwrap_or_default();
            let text = all.get(k).cloned().unwrap_or_default();
            lines.push(LineHit {
                line: k + 1,
                text,
                spans,
                context: !hit_lines.contains(&k),
            });
        }
        report.hits += spans_by_line.len();
        report.files.push(FileHit {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            path,
            lines,
        });
    }
    report.files.sort_by(|a, b| a.path.cmp(&b.path));
    report.files_matched = report.files.len();
    Ok(report)
}

/// 递归收集待搜的文件（**不读内容**），深度 / 隐藏 / 中断都在这里拦。
fn collect_files(
    dir: &Path,
    depth: usize,
    q: &ContentQuery,
    stop: &AtomicBool,
    out: &mut Vec<PathBuf>,
) {
    if stop.load(Ordering::Relaxed) {
        return;
    }
    if q.max_depth > 0 && depth >= q.max_depth {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if q.skip_hidden && name.starts_with('.') {
            continue;
        }
        let path = e.path();
        // ⚠️ `symlink_metadata`：软链本身不是目录，跟进去既可能成环（`/tmp/self -> .`）
        // 也会把「同一个文件」扫两遍。内容搜索不跟随软链。
        let Ok(m) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if m.is_dir() {
            collect_files(&path, depth + 1, q, stop, out);
        } else if m.is_file() && wanted(&path, &name, q) {
            out.push(path);
        }
    }
}

/// 二进制嗅探：前 [`SNIFF_BYTES`] 里有 NUL 就当作二进制。
///
/// 判 NUL 而不是「非打印字符占比」：前者对文本几乎零误判（正常文本不含 NUL），
/// 后者会把 UTF-8 的中文 / emoji 統統判成二进制。UTF-8 合法性由调用方另判。
fn looks_binary(bytes: &[u8]) -> bool {
    let end = bytes.len().min(SNIFF_BYTES);
    bytes[..end].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::{name_matches, search_content, ContentQuery, ContentReport, DEFAULT_MAX_PER_FILE};
    use std::sync::atomic::AtomicBool;

    /// 在临时目录下搭一棵固定的小树，返回根目录。
    ///
    /// 用 pid + 纳秒拼唯一名（`tempfile` 没进依赖），测试完自己清掉。
    fn tree(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mo-content-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(dir.join("sub/deep"));
        std::fs::write(dir.join("a.txt"), "hello world\nsecond TODO line\n").unwrap();
        std::fs::write(dir.join("b.rs"), "fn main() { /* TODO */ }\n").unwrap();
        std::fs::write(dir.join("sub/c.txt"), "nothing here\n").unwrap();
        std::fs::write(dir.join("sub/deep/d.txt"), "todo: hidden deep\n").unwrap();
        // 二进制：前几字节就有 NUL，必须被跳过。
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3, b'T', b'O', b'D', b'O']).unwrap();
        dir
    }

    fn run(root: &std::path::Path, q: &ContentQuery) -> ContentReport {
        search_content(root, q, &AtomicBool::new(false)).expect("搜索不该失败")
    }

    /// 最核心的一条：真的按内容找到了，且行号 / 行文本 / 高亮区间都对。
    #[test]
    fn finds_the_lines_and_reports_highlight_spans() {
        let dir = tree("basic");
        let q = ContentQuery::new("TODO");
        let r = run(&dir, &q);
        // 大小写不敏感是默认，所以 sub/deep/d.txt 的 `todo` 也算。
        assert_eq!(r.files_matched, 3, "a.txt / b.rs / sub/deep/d.txt 各有一处");
        assert_eq!(r.hits, 3);
        // 结果按路径排序：a.txt 在前。
        let a = &r.files[0];
        assert_eq!(a.name, "a.txt");
        assert_eq!(a.lines.len(), 1);
        assert_eq!(a.lines[0].line, 2, "行号从 1 开始");
        assert_eq!(a.lines[0].text, "second TODO line");
        // 字符下标：'s','e','c','o','n','d',' ' 之后才是 TODO。
        assert_eq!(a.lines[0].spans, vec![(7, 11)]);
        let b = &r.files[1];
        assert_eq!(b.lines[0].spans, vec![(15, 19)], "/* TODO */ 里的 TODO");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 不区分大小写是默认：`todo` 要能找到 `TODO`。
    #[test]
    fn case_insensitive_by_default_and_strict_when_asked() {
        let dir = tree("case");
        let loose = run(&dir, &ContentQuery::new("todo"));
        assert_eq!(loose.hits, 3, "a / b / deep 三条");
        let mut strict = ContentQuery::new("todo");
        strict.case_sensitive = true;
        let r = run(&dir, &strict);
        assert_eq!(r.hits, 1, "只有 deep 那条是小写");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 二进制文件要跳过，且**不**被当成命中（否则搜出来的行是乱码）。
    #[test]
    fn binary_files_are_skipped_not_scanned() {
        let dir = tree("binary");
        let r = run(&dir, &ContentQuery::new("TODO"));
        assert_eq!(r.skipped_binary, 1, "blob.bin 应被判成二进制");
        assert!(
            !r.files.iter().any(|f| f.name == "blob.bin"),
            "二进制不能进结果：它里面的 TODO 是字节巧合，不是文本"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 递归进子目录；`max_depth` 生效时只搜前若干层。
    #[test]
    fn recursion_honours_max_depth() {
        let dir = tree("depth");
        assert_eq!(
            run(&dir, &ContentQuery::new("todo")).hits,
            3,
            "默认不限深度"
        );
        let mut shallow = ContentQuery::new("todo");
        shallow.max_depth = 1;
        let r = run(&dir, &shallow);
        assert_eq!(r.hits, 2, "深度 1：只到 sub/ 那一层，deep/d.txt 不进");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// glob 收窄：只看 `*.rs`。
    #[test]
    fn include_globs_narrow_the_scan() {
        let dir = tree("glob");
        let mut q = ContentQuery::new("todo");
        q.include = vec!["*.rs".to_string()];
        let r = run(&dir, &q);
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].name, "b.rs");

        let mut q2 = ContentQuery::new("todo");
        q2.exclude = vec!["*.rs".to_string()];
        let r2 = run(&dir, &q2);
        assert!(!r2.files.iter().any(|f| f.name == "b.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预算：命中撞上限就停并置 `truncated`，绝不为了「搜全」跑到底。
    #[test]
    fn stops_at_the_hit_budget_and_says_so() {
        let dir = tree("budget");
        let mut q = ContentQuery::new("todo");
        q.max_hits = 1;
        let r = run(&dir, &q);
        assert_eq!(r.hits, 1);
        assert!(r.truncated, "撞到预算必须置位，UI 才好提示「结果被截断」");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 上下文行：跟着命中行带出来，且**不带高亮区间**。
    #[test]
    fn context_lines_ride_along_without_spans() {
        let dir = tree("ctx");
        let mut q = ContentQuery::new("TODO");
        q.context = 1;
        let r = run(&dir, &q);
        let a = r.files.iter().find(|f| f.name == "a.txt").unwrap();
        assert_eq!(a.lines.len(), 2, "命中行 + 上面一行");
        assert!(a.lines[0].context && a.lines[0].spans.is_empty());
        assert!(!a.lines[1].context && !a.lines[1].spans.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 正则与整词：两条都走另一条匹配分支，串味就全错。
    #[test]
    fn regex_and_whole_word_take_their_own_paths() {
        let dir = tree("mode");
        let mut q = ContentQuery::new(r"fn \w+\(");
        q.regex = true;
        let r = run(&dir, &q);
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].name, "b.rs");
        assert_eq!(r.files[0].lines[0].spans, vec![(0, 8)], "fn main( 整段");

        // 整词：`hello` 不该命中 `hello`，但 `hello` 左右是空格才算。
        let mut w = ContentQuery::new("hello");
        w.whole_word = true;
        assert_eq!(run(&dir, &w).hits, 1, "hello world 里 hello 是整词");
        let mut w2 = ContentQuery::new("hell");
        w2.whole_word = true;
        assert_eq!(run(&dir, &w2).hits, 0, "hell 不是整词");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 多字节字符：区间按**字符**给，不能按字节（否则高亮会把汉字切坏）。
    #[test]
    fn spans_are_char_offsets_not_bytes() {
        let dir = std::env::temp_dir().join(format!("mo-content-cjk-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("c.txt"), "你好世界，再见世界\n").unwrap();
        let r = run(&dir, &ContentQuery::new("世界"));
        let spans = &r.files[0].lines[0].spans;
        assert_eq!(spans.len(), 2, "两个「世界」");
        assert_eq!(spans[0], (2, 4), "前两个字是「你好」");
        assert_eq!(spans[1], (7, 9));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `stop` 一置位就收工：这是「关掉搜索框 / 切目录」能立刻生效的前提。
    #[test]
    fn stop_flag_ends_the_scan() {
        let dir = tree("stop");
        let stop = AtomicBool::new(true);
        let r = search_content(&dir, &ContentQuery::new("todo"), &stop).unwrap();
        assert_eq!(r.hits, 0, "一开始就叫停 → 一条都不该扫");
        assert_eq!(r.scanned, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空搜索词 / 坏正则：给的是能直接显示给人看的错误，不是 panic。
    #[test]
    fn bad_queries_are_errors_not_panics() {
        let dir = tree("bad");
        assert!(search_content(&dir, &ContentQuery::new(""), &AtomicBool::new(false)).is_err());
        let mut q = ContentQuery::new("([");
        q.regex = true;
        assert!(search_content(&dir, &q, &AtomicBool::new(false)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 单文件上限：一个文件里几十处命中也只报前 N 条。
    #[test]
    fn per_file_cap_limits_one_noisy_file() {
        let dir = std::env::temp_dir().join(format!("mo-content-noisy-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let many = "todo\n".repeat(50);
        std::fs::write(dir.join("n.txt"), many).unwrap();
        let r = run(&dir, &ContentQuery::new("todo"));
        assert_eq!(r.hits, DEFAULT_MAX_PER_FILE, "单文件命中被截断到上限");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_matcher_handles_star_and_question() {
        assert!(name_matches("main.rs", "*.rs"));
        assert!(name_matches("MAIN.RS", "*.rs"), "glob 不区分大小写");
        assert!(!name_matches("main.ts", "*.rs"));
        assert!(name_matches("a.rs", "?.rs"));
        assert!(!name_matches("ab.rs", "?.rs"));
        assert!(name_matches("test_foo.py", "test_*"));
        assert!(!name_matches("foo_test.py", "test_*"), "约束的是开头");
    }
}
