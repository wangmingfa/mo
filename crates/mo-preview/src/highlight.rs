//! 预览里的**轻量语法着色**。
//!
//! ## 为什么自己写
//!
//! 预览此前对 `Code` / `Json` 一律按纯文本显示（`PreviewKind::Code` 的注释里就
//! 写着「本阶段仍是纯文本」）。要着色通常的答案是 `syntect` / `tree-sitter`，
//! 但它们带一张几 MB 的语法与主题表、编译期要跑构建脚本——为一个「瞄一眼文件」
//! 的浮窗付这个代价不划算，而且预览只读 512KB 的头部、还要跟着主题走。
//!
//! 这里的做法是一台**手写扫描器**：只认五类记号（注释 / 字符串 / 数字 / 关键字 /
//! 标点），关键字表是几门常见语言的并集。认不出的单词一律 `Plain`——着色是装饰，
//! 认错了比不认更糟（把变量涂成关键字色会让人读错代码）。
//!
//! ## 契约
//!
//! * **纯函数**：输入源码，输出按行分组的记号。无 IO、无全局状态，可以在
//!   headless 里直接断言。
//! * **有上限**：超过 [`MAX_HIGHLIGHT_BYTES`] / [`MAX_HIGHLIGHT_LINES`] 直接返回
//!   `None`（调用方退回纯文本）。一个大文件逐字符扫一遍再摊成几万个元素，
//!   光建元素就能把浮窗卡住——「瞄一眼」要的是快，不是全。

/// 着色的字节上限：超过就不着色（退回纯文本）。
pub const MAX_HIGHLIGHT_BYTES: usize = 128 * 1024;
/// 着色的行数上限。
///
/// 定在 300 不是拍脑袋：渲染侧**一行一个容器、一段记号一个元素**，一行代码通常
/// 4~6 段，300 行就是一千多个元素——再往上，光建元素就能让浮窗滚动变顿。
/// 「瞄一眼」要的是快，超过这个长度退回纯文本（整段一个元素，再长也不卡）。
pub const MAX_HIGHLIGHT_LINES: usize = 300;

/// 一类记号。种类刻意少：颜色只要几种，多了反而读不出层次。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// 普通文本（含空白、标识符）。
    Plain,
    /// 关键字 / 字面量 `true` `false` `null` 那几个。
    Keyword,
    /// 字符串字面量（含引号）。
    String,
    /// 注释（行注释与块注释都算）。
    Comment,
    /// 数字字面量。
    Number,
    /// 标点与运算符。
    Punct,
    // ---- 以下是 Markdown 渲染用的块级 / 行内记号（见 [`crate::markdown`]）----
    /// 标题（级别 1~6，决定字号）。
    Heading(u8),
    /// 行内代码 / 围栏代码块。
    InlineCode,
    /// 链接（`[文字](地址)` 里的文字）。
    Link,
    /// 引用行。
    Quote,
    /// 列表项的标记（渲染时换成 `•`）。
    ListMarker,
    /// 分隔线（渲染时画成一横）。
    Rule,
    /// **加粗**。
    Bold,
}

/// 一段同色的文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub kind: TokenKind,
}

/// 给 `src` 着色，返回**按行分组**的记号；超上限或不值得着色时返回 `None`。
///
/// 按行分组是因为渲染侧要一行一个容器（行内多段不同色，行间仍要能滚动与换行），
/// 而「行」正好也是块注释跨行状态的自然边界。
pub fn highlight(src: &str) -> Option<Vec<Vec<Token>>> {
    if src.len() > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let mut lines: Vec<Vec<Token>> = Vec::new();
    let mut in_block = false;
    for line in src.split('\n') {
        if lines.len() >= MAX_HIGHLIGHT_LINES {
            return None;
        }
        lines.push(highlight_line(line, &mut in_block));
    }
    Some(lines)
}

/// 扫一行。
///
/// * `in_block`：块注释是否还开着——跨行的 `/**/` 靠它在行之间传状态，否则
///   注释块后面每一行都被当成代码（一片关键字色，非常难看）。
fn highlight_line(line: &str, in_block: &mut bool) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    // 相邻同类记号合并：一个标识符逐字符 push 会产出几十个只含一个字符的
    // Token，渲染侧就是几十个元素——合并之后一行通常只有个位数的段。
    let push = |out: &mut Vec<Token>, text: String, kind: TokenKind| {
        if text.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.kind == kind => last.text.push_str(&text),
            _ => out.push(Token { text, kind }),
        }
    };

    while i < chars.len() {
        let c = chars[i];

        // 块注释内部：一直吃到 `*/`（吃不到就整行都是注释）。
        if *in_block {
            let rest: String = chars[i..].iter().collect();
            match rest.find("*/") {
                Some(end) => {
                    // ⚠️ `find` 给的是**字节**偏移，而 `i` 走的是字符下标——中文
                    // 注释里直接加会把下标推到错位置（后面整行都认错）。
                    let taken = rest[..end + 2].chars().count();
                    push(&mut out, rest[..end + 2].to_string(), TokenKind::Comment);
                    *in_block = false;
                    i += taken;
                }
                None => {
                    push(&mut out, rest, TokenKind::Comment);
                    i = chars.len();
                }
            }
            continue;
        }

        // 行注释。
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            push(&mut out, chars[i..].iter().collect(), TokenKind::Comment);
            break;
        }
        // 块注释开头。
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let rest: String = chars[i..].iter().collect();
            match rest.find("*/") {
                Some(end) => {
                    let taken = rest[..end + 2].chars().count();
                    push(&mut out, rest[..end + 2].to_string(), TokenKind::Comment);
                    i += taken;
                }
                None => {
                    push(&mut out, rest, TokenKind::Comment);
                    *in_block = true;
                    i = chars.len();
                }
            }
            continue;
        }
        // 字符串：`"` / `'` 都认，反斜杠转义跳过下一个字符。
        if c == '"' || c == '\'' {
            let mut j = i + 1;
            while j < chars.len() {
                match chars[j] {
                    '\\' => j += 2,
                    q if q == c => {
                        j += 1;
                        break;
                    }
                    // 引号没闭合就换行：当作字符串到行尾（不跨行吞掉后面所有内容，
                    // 那会让整个文件变成一片字符串色）。
                    '\n' => break,
                    _ => j += 1,
                }
            }
            let end = j.min(chars.len());
            push(&mut out, chars[i..end].iter().collect(), TokenKind::String);
            i = end;
            continue;
        }
        // 数字：开头是数字，或 `.` 后面紧跟数字（`.5`）。
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()))
        {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_')
            {
                j += 1;
            }
            push(&mut out, chars[i..j].iter().collect(), TokenKind::Number);
            i = j;
            continue;
        }
        // 标识符 / 关键字。
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let kind = if is_keyword(&word) {
                TokenKind::Keyword
            } else {
                TokenKind::Plain
            };
            push(&mut out, word, kind);
            i = j;
            continue;
        }
        // 标点 / 运算符。
        if is_punct(c) {
            push(&mut out, c.to_string(), TokenKind::Punct);
            i += 1;
            continue;
        }
        push(&mut out, c.to_string(), TokenKind::Plain);
        i += 1;
    }

    if out.is_empty() {
        out.push(Token {
            text: String::new(),
            kind: TokenKind::Plain,
        });
    }
    out
}

fn is_punct(c: char) -> bool {
    matches!(
        c,
        '{' | '}'
            | '('
            | ')'
            | '['
            | ']'
            | ';'
            | ':'
            | ','
            | '.'
            | '<'
            | '>'
            | '='
            | '+'
            | '-'
            | '*'
            | '/'
            | '%'
            | '&'
            | '|'
            | '!'
            | '?'
            | '^'
            | '~'
    )
}

/// 关键字表：**几门常见语言的并集**。
///
/// 并集而不是按语言分表，是因为预览不值得为「这是 .rs 还是 .ts」再判一次——
/// 认错了最坏的结果是把一个叫 `match` 的变量涂成关键字色，代价远小于维护
/// 一整套按扩展名分派的表。
fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        // Rust
        "as" | "async" | "await" | "break" | "const" | "continue" | "crate" | "dyn" | "else"
        | "enum" | "extern" | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" | "match"
        | "mod" | "move" | "mut" | "pub" | "ref" | "return" | "self" | "Self" | "static"
        | "struct" | "super" | "trait" | "type" | "unsafe" | "use" | "where" | "while"
        // C / C++ / Java / C#
        | "auto" | "bool" | "byte" | "case" | "catch" | "char" | "class" | "default" | "delete"
        | "do" | "double" | "extends" | "final" | "finally" | "float" | "goto" | "implements"
        | "import" | "instanceof" | "int" | "interface" | "long" | "native" | "new" | "nullptr"
        | "package" | "private" | "protected" | "public" | "short" | "signed" | "sizeof"
        | "static_cast" | "switch" | "synchronized" | "template" | "this" | "throw" | "throws"
        | "transient" | "try" | "typedef" | "typename" | "union" | "unsigned" | "using"
        | "virtual" | "void" | "volatile"
        // JS / TS / Python / Go（`await` 已在上一段里）
        | "abstract" | "console" | "debugger" | "def" | "defer" | "del" | "elif"
        | "except" | "export" | "False" | "from" | "func" | "global" | "go" | "is" | "lambda"
        | "None" | "nonlocal" | "not" | "pass" | "raise" | "range" | "select" | "True"
        | "undefined" | "var" | "with" | "yield"
        // 通用字面量（JSON 的 true/false/null 也走这条）
        | "false" | "null" | "true"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &[Token]) -> Vec<TokenKind> {
        line.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn rust_keywords_strings_and_comments_are_tagged() {
        let src = "fn main() { // 入口\n    let s = \"hi\";\n}\n";
        let lines = highlight(src).expect("小文件应当着色");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0][0].text, "fn");
        assert_eq!(lines[0][0].kind, TokenKind::Keyword);
        // 行注释整段一个 Comment 记号。
        assert!(lines[0].iter().any(|t| t.kind == TokenKind::Comment));
        let second = kinds(&lines[1]);
        assert!(second.contains(&TokenKind::Keyword), "{second:?}");
        assert!(second.contains(&TokenKind::String), "{second:?}");
    }

    /// 块注释跨行：靠 `in_block` 在行之间传状态，否则注释后面每一行都被当代码。
    #[test]
    fn block_comment_carries_across_lines() {
        let src = "/* 开头\n还在注释里 */ let x = 1;\n";
        let lines = highlight(src).expect("小文件应当着色");
        assert!(
            lines[1]
                .iter()
                .take_while(|t| t.kind == TokenKind::Comment)
                .count()
                >= 1,
            "第二行开头仍是注释：{:?}",
            lines[1]
        );
        assert!(lines[1].iter().any(|t| t.kind == TokenKind::Keyword));
        assert!(lines[1].iter().any(|t| t.kind == TokenKind::Number));
    }

    /// 相邻同类记号合并：一行不该产出几十个单字符 Token（那等于几十个 UI 元素）。
    #[test]
    fn adjacent_same_kind_tokens_are_merged() {
        let lines = highlight("let variable_name = 1;").unwrap();
        assert_eq!(lines[0].len(), 6, "{:?}", lines[0]);
        // 空白与标识符同为 Plain，合并成一段（含两侧空格）。
        assert_eq!(lines[0][1].text.trim(), "variable_name");
        assert_eq!(lines[0][1].kind, TokenKind::Plain);
    }

    /// 没闭合的引号不跨行吞掉后面所有内容。
    #[test]
    fn unterminated_string_stays_on_its_line() {
        let lines = highlight("\"abc\nx = 1\n").unwrap();
        assert!(!lines[1].iter().any(|t| t.kind == TokenKind::String));
        assert!(lines[1].iter().any(|t| t.kind == TokenKind::Number));
    }

    /// 超上限 = 不着色（`None`），调用方退回纯文本。
    #[test]
    fn oversized_input_is_not_highlighted() {
        let big = "a".repeat(MAX_HIGHLIGHT_BYTES + 1);
        assert!(highlight(&big).is_none());
        let many = "\n".repeat(MAX_HIGHLIGHT_LINES + 5);
        assert!(highlight(&many).is_none());
    }

    /// JSON：键与值是字符串、数字、`true/false/null` 走关键字色。
    #[test]
    fn json_literals_are_tagged() {
        let lines = highlight("{\"name\": \"mo\", \"n\": 1, \"ok\": true}").unwrap();
        let k = kinds(&lines[0]);
        assert!(k.contains(&TokenKind::String));
        assert!(k.contains(&TokenKind::Number));
        assert!(k.contains(&TokenKind::Keyword));
    }
}
