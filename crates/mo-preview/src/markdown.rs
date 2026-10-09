//! Markdown 预览的**结构化渲染**。
//!
//! 此前 `.md` 在预览里显示的是**源码**：`# 标题`、`**加粗**`、`` `代码` `` 这些
//! 记号原样糊在屏幕上。记号的本意是给渲染器看的，给人看的是它渲染之后的样子。
//!
//! ## 为什么做到「记号」这一层就停
//!
//! 完整的 Markdown（嵌套列表、表格、脚注、HTML 内联）是一整个排版引擎的事，
//! 为一个「瞄一眼文件」的浮窗做全套不划算。这里只认最常见的块级结构（标题 /
//! 围栏代码块 / 引用 / 列表 / 分隔线）与行内记号（行内代码 / 链接 / 加粗），
//! 认不出的行一律按纯文本原样显示——**宁可少渲染，不可渲染错**：把一段正文
//! 误判成标题，比不渲染它更糟。
//!
//! 输出与 `highlight` 同一套 [`Token`]（按行分组），所以渲染侧只多几个颜色分支，
//! 不用另写一套绘制。

use crate::highlight::{Token, TokenKind, MAX_HIGHLIGHT_BYTES, MAX_HIGHLIGHT_LINES};

/// 把 Markdown 渲染成按行分组的记号；超上限时返回 `None`（调用方退回纯文本）。
pub fn markdown(src: &str) -> Option<Vec<Vec<Token>>> {
    if src.len() > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let mut out: Vec<Vec<Token>> = Vec::new();
    let mut in_fence = false;
    for line in src.split('\n') {
        if out.len() >= MAX_HIGHLIGHT_LINES {
            return None;
        }
        out.push(render_line(line, &mut in_fence));
    }
    Some(out)
}

fn render_line(line: &str, in_fence: &mut bool) -> Vec<Token> {
    // 围栏代码块：整行原样（缩进要保留），只换颜色。
    if *in_fence {
        if line.trim_start().starts_with("```") {
            *in_fence = false;
            return vec![tok(line, TokenKind::InlineCode)];
        }
        return vec![tok(line, TokenKind::InlineCode)];
    }
    if line.trim_start().starts_with("```") {
        *in_fence = true;
        return vec![tok(line, TokenKind::InlineCode)];
    }

    let trimmed = line.trim_end();

    // 分隔线：`---` / `***` / `___`（三个以上、只有这些字符）。
    if is_rule(trimmed) {
        return vec![tok(&"─".repeat(40), TokenKind::Rule)];
    }

    // 标题：`#` ~ `######` + 空格。整行一个记号（**不做行内解析**）：标题靠
    // 字号与颜色立起来就够了，再拆行内记号只会让同一行出现两种字号，反而乱。
    if let Some(rest) = heading_of(trimmed) {
        let level = trimmed.chars().take_while(|c| *c == '#').count() as u8;
        return vec![tok(rest.trim(), TokenKind::Heading(level.min(6)))];
    }

    // 引用：`> …`（只剥一层，嵌套引用按纯文本处理）。
    if let Some(rest) = trimmed.strip_prefix('>') {
        let body = rest.strip_prefix(' ').unwrap_or(rest);
        return vec![tok(body, TokenKind::Quote)];
    }

    // 列表项：`- ` / `* ` / `+ ` / `1. `。
    if let Some((marker, rest)) = list_item_of(trimmed) {
        let mut out = vec![Token {
            text: marker,
            kind: TokenKind::ListMarker,
        }];
        out.extend(inline(rest));
        return out;
    }

    inline(trimmed)
}

fn tok(text: &str, kind: TokenKind) -> Token {
    Token {
        text: text.to_string(),
        kind,
    }
}

fn is_rule(line: &str) -> bool {
    let line = line.trim();
    if line.len() < 3 {
        return false;
    }
    let c = line.chars().next().unwrap();
    matches!(c, '-' | '*' | '_') && line.chars().all(|x| x == c || x == ' ')
}

fn heading_of(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // `#######`（七个）不是标题；`#标题`（没空格）也不是——那是话题标记不是标题。
    let rest = &line[hashes..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some(rest)
}

fn list_item_of(line: &str) -> Option<(String, &str)> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(("• ".to_string(), rest));
        }
    }
    // 有序列表：`1. ` —— 保留原序号（换成圆点就看不出是第几项了）。
    let digits = line.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && line[digits..].starts_with(". ") {
        return Some((line[..digits + 1].to_string(), &line[digits + 2..]));
    }
    None
}

/// 行内记号：`` `代码` `` / `[文字](地址)` / `**加粗**`。
///
/// 只认这三种——它们占日常 Markdown 行内用法的绝大多数，且**都能无歧义地就地
/// 判定**（有明确的起止标记）。`*斜体*` 不处理：`*` 同时是列表标记与强调标记，
/// 一段里出现单个 `*` 就猜错，而猜错的代价是把正文涂成奇怪的颜色。
fn inline(line: &str) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut plain = String::new();
    let flush = |out: &mut Vec<Token>, plain: &mut String| {
        if !plain.is_empty() {
            out.push(Token {
                text: std::mem::take(plain),
                kind: TokenKind::Plain,
            });
        }
    };

    while i < chars.len() {
        let c = chars[i];
        // 行内代码：反引号到下一个反引号。
        if c == '`' {
            let rest: String = chars[i + 1..].iter().collect();
            match rest.find('`') {
                Some(end) => {
                    flush(&mut out, &mut plain);
                    out.push(tok(&rest[..end], TokenKind::InlineCode));
                    i += 1 + rest[..end].chars().count() + 1;
                    continue;
                }
                None => {
                    plain.push(c);
                    i += 1;
                    continue;
                }
            }
        }
        // 链接：`[文字](地址)`。显示**文字**，地址丢弃——预览里留一长串 URL
        // 会把版式撑坏，而点不了（纯文本预览）留着也没用。
        if c == '[' {
            let rest: String = chars[i..].iter().collect();
            if let Some(close) = rest.find(']') {
                let after = &rest[close + 1..];
                if after.starts_with('(') {
                    if let Some(url_end) = after.find(')') {
                        flush(&mut out, &mut plain);
                        let text = &rest[1..close];
                        let url = &after[1..url_end];
                        // 文字为空（`[](url)`）时退而显示地址本身。
                        out.push(tok(
                            if text.is_empty() { url } else { text },
                            TokenKind::Link,
                        ));
                        i += rest[..close + 1].chars().count()
                            + after[..url_end + 1].chars().count();
                        continue;
                    }
                }
            }
        }
        // 加粗：`**文字**`。
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            let rest: String = chars[i + 2..].iter().collect();
            if let Some(end) = rest.find("**") {
                flush(&mut out, &mut plain);
                out.push(tok(&rest[..end], TokenKind::Bold));
                i += 2 + rest[..end].chars().count() + 2;
                continue;
            }
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut out, &mut plain);
    if out.is_empty() {
        out.push(tok("", TokenKind::Plain));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(l: &[Token]) -> String {
        l.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn headings_lose_their_hashes_and_keep_the_level() {
        let lines = markdown("# 标题\n\n正文\n").unwrap();
        assert_eq!(lines[0][0].text, "标题");
        assert_eq!(lines[0][0].kind, TokenKind::Heading(1));
        assert_eq!(lines[2][0].kind, TokenKind::Plain);
    }

    /// `#######`（七个 #）不是标题——Markdown 只到六级。
    #[test]
    fn seven_hashes_is_not_a_heading() {
        let lines = markdown("####### nope\n").unwrap();
        assert!(!matches!(lines[0][0].kind, TokenKind::Heading(_)));
    }

    #[test]
    fn fenced_code_is_passed_through_verbatim() {
        let src = "正文\n```\nlet x = 1;\n```\n后\n";
        let lines = markdown(src).unwrap();
        assert_eq!(lines[2][0].text, "let x = 1;");
        assert_eq!(lines[2][0].kind, TokenKind::InlineCode);
        assert_eq!(lines[4][0].kind, TokenKind::Plain);
    }

    #[test]
    fn lists_quotes_and_rules_are_structured() {
        let lines = markdown("- 一项\n> 引用\n---\n").unwrap();
        assert_eq!(lines[0][0].kind, TokenKind::ListMarker);
        assert_eq!(lines[0][0].text, "• ");
        assert_eq!(line_text(&lines[0]), "• 一项");
        assert_eq!(lines[1][0].kind, TokenKind::Quote);
        assert_eq!(lines[1][0].text, "引用");
        assert_eq!(lines[2][0].kind, TokenKind::Rule);
    }

    #[test]
    fn ordered_lists_keep_their_number() {
        let lines = markdown("1. 第一\n2. 第二\n").unwrap();
        assert_eq!(lines[0][0].text, "1.");
        assert_eq!(lines[1][0].text, "2.");
    }

    #[test]
    fn inline_code_links_and_bold_are_tagged() {
        let lines = markdown("用 `mo` 打开 [文档](https://x.y) 与 **重点**").unwrap();
        let kinds: Vec<TokenKind> = lines[0].iter().map(|t| t.kind).collect();
        assert!(kinds.contains(&TokenKind::InlineCode), "{kinds:?}");
        assert!(kinds.contains(&TokenKind::Link), "{kinds:?}");
        assert!(kinds.contains(&TokenKind::Bold), "{kinds:?}");
        // 地址不留在正文里。
        assert!(!line_text(&lines[0]).contains("https://"));
        assert_eq!(line_text(&lines[0]), "用 mo 打开 文档 与 重点");
    }

    /// 没闭合的反引号 / 括号：按纯文本原样显示，不吞掉后面整行。
    #[test]
    fn unbalanced_markers_stay_plain() {
        let lines = markdown("a `b c\n[未闭合](x\n").unwrap();
        assert!(lines[0].iter().all(|t| t.kind == TokenKind::Plain));
        assert!(
            lines[1].iter().all(|t| t.kind == TokenKind::Plain),
            "{:?}",
            lines[1]
        );
    }
}
