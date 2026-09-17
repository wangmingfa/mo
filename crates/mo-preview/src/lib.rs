//! mo-preview：文件预览提取。
//!
//! 给定一个路径，判断其类型并提取「足够预览」的内容：
//!
//! * 文本 / Markdown / JSON / 代码：读入文本（大文件只读前 [`MAX_TEXT`] 字节）；
//! * 图片：不解码，只回传路径让 UI 用 `img()` 直接加载；
//! * 目录：列出前若干条目作为摘要；
//! * 二进制 / 空文件：给一句说明。
//!
//! 本 crate **不依赖任何 UI 框架**，纯文件系统读取，可单独单元测试。

use std::path::Path;

use mo_core::MoError;

/// 预览类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    /// 普通文本。
    Text,
    /// Markdown 文档。
    Markdown,
    /// JSON 数据。
    Json,
    /// 源代码（仅用于语法着色提示，本阶段仍是纯文本）。
    Code,
    /// 图片（内容由 UI 直接加载路径，不在此解码）。
    Image,
    /// 目录（内容为条目摘要）。
    Directory,
    /// 二进制（非文本）。
    Binary,
    /// 空文件。
    Empty,
}

/// 一个文件的预览内容。
#[derive(Debug, Clone)]
pub struct Preview {
    pub kind: PreviewKind,
    /// 展示标题（文件名）。
    pub title: String,
    /// 文本内容（文本 / markdown / json / code / 目录摘要 / 二进制说明）。
    pub text: Option<String>,
    /// 若是图片，给出原始路径（UI 用 `img()` 加载）。
    pub image: Option<std::path::PathBuf>,
    /// 文件大小（字节）。
    pub size: u64,
}

/// 预览最多读取的字节数（再大也只显示头部）。
const MAX_TEXT: usize = 512 * 1024;
/// 探测是否文本时读取的前缀字节数。
const HEAD: usize = 64 * 1024;
/// 目录预览最多列出的条目数。
const DIR_PREVIEW: usize = 200;

/// 预览单个文件 / 目录。
pub fn preview_path(path: &Path) -> Result<Preview, MoError> {
    let meta = std::fs::symlink_metadata(path).map_err(MoError::Io)?;
    let size = meta.len();
    let title = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    if meta.is_dir() {
        return preview_directory(path, title, size);
    }
    if size == 0 {
        return Ok(Preview {
            kind: PreviewKind::Empty,
            title,
            text: Some("(空文件)".to_string()),
            image: None,
            size,
        });
    }

    // 图片：按扩展名判断，直接给路径（不解码，留给 UI 的缩略图 / 原图加载）。
    if is_image(path) {
        return Ok(Preview {
            kind: PreviewKind::Image,
            title,
            text: None,
            image: Some(path.to_path_buf()),
            size,
        });
    }

    // 文本类：先读前缀判断是否合法 UTF-8。
    let head = read_head(path, HEAD)?;
    if !is_utf8(&head) {
        return Ok(Preview {
            kind: PreviewKind::Binary,
            title,
            text: Some(format!("二进制文件，{size} 字节")),
            image: None,
            size,
        });
    }

    let kind = kind_by_ext(path);
    let text = if size as usize <= MAX_TEXT {
        std::fs::read_to_string(path).map_err(MoError::Io)?
    } else {
        let mut s = String::from_utf8_lossy(&head).to_string();
        s.push_str(&format!("\n\n…（文件较大，仅显示前 {} KB）", HEAD / 1024));
        s
    };

    Ok(Preview {
        kind,
        title,
        text: Some(text),
        image: None,
        size,
    })
}

/// 目录预览：列出前若干条目（目录在前）。
fn preview_directory(path: &Path, title: String, size: u64) -> Result<Preview, MoError> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Ok(rd) = std::fs::read_dir(path) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Ok(m) = entry.metadata() {
                if m.is_dir() {
                    dirs.push(name);
                } else {
                    files.push(name);
                }
            } else {
                files.push(name);
            }
            if dirs.len() + files.len() >= DIR_PREVIEW {
                break;
            }
        }
    }
    dirs.sort();
    files.sort();
    let mut text = String::from("目录内容（最多显示前 200 项）：\n");
    for d in &dirs {
        text.push_str(&format!("📁 {d}\n"));
    }
    for f in &files {
        text.push_str(&format!("📄 {f}\n"));
    }
    if dirs.is_empty() && files.is_empty() {
        text.push_str("（空目录）\n");
    }
    Ok(Preview {
        kind: PreviewKind::Directory,
        title,
        text: Some(text),
        image: None,
        size,
    })
}

/// 读文件前 `n` 字节（不足则读全部）。
fn read_head(path: &Path, n: usize) -> Result<Vec<u8>, MoError> {
    use std::io::Read;
    let len = std::fs::metadata(path).map_err(MoError::Io)?.len() as usize;
    let take = n.min(len);
    let mut f = std::fs::File::open(path).map_err(MoError::Io)?;
    let mut buf = vec![0u8; take];
    f.read_exact(&mut buf).map_err(MoError::Io)?;
    Ok(buf)
}

fn is_utf8(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
}

fn is_image(path: &Path) -> bool {
    matches!(
        ext(path).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff" | "avif" | "heic")
    )
}

fn kind_by_ext(path: &Path) -> PreviewKind {
    match ext(path).as_deref() {
        Some("md" | "markdown") => PreviewKind::Markdown,
        Some("json") => PreviewKind::Json,
        Some("rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "c" | "cpp" | "h" | "hpp" | "cc" | "java"
             | "go" | "sh" | "bash" | "zsh" | "toml" | "yaml" | "yml" | "cfg" | "conf" | "ini"
             | "css" | "html" | "htm" | "xml" | "lua" | "rb" | "php" | "sql") => PreviewKind::Code,
        _ => PreviewKind::Text,
    }
}

fn ext(path: &Path) -> Option<String> {
    let e = path.extension()?.to_string_lossy().to_lowercase();
    if e.is_empty() { None } else { Some(e) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("mo-preview-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn text_and_markdown_and_json() {
        let d = tmp("kinds");
        let mut t = std::fs::File::create(d.join("a.txt")).unwrap();
        t.write_all(b"hello world").unwrap();
        let mut m = std::fs::File::create(d.join("doc.md")).unwrap();
        m.write_all(b"# Title\nbody").unwrap();
        let mut j = std::fs::File::create(d.join("data.json")).unwrap();
        j.write_all(b"{\"k\":1}").unwrap();

        assert_eq!(preview_path(&d.join("a.txt")).unwrap().kind, PreviewKind::Text);
        let md = preview_path(&d.join("doc.md")).unwrap();
        assert_eq!(md.kind, PreviewKind::Markdown);
        assert!(md.text.unwrap().contains("# Title"));
        let js = preview_path(&d.join("data.json")).unwrap();
        assert_eq!(js.kind, PreviewKind::Json);
        assert!(js.text.unwrap().contains("\"k\":1"));
    }

    #[test]
    fn directory_lists_entries() {
        let d = tmp("dir");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("file.txt"), b"x").unwrap();
        let pv = preview_path(&d).unwrap();
        assert_eq!(pv.kind, PreviewKind::Directory);
        let text = pv.text.unwrap();
        assert!(text.contains("📁 sub"));
        assert!(text.contains("📄 file.txt"));
    }

    #[test]
    fn empty_and_binary() {
        let d = tmp("misc");
        std::fs::write(d.join("empty.bin"), b"").unwrap();
        let mut b = std::fs::File::create(d.join("blob.bin")).unwrap();
        // 写入非法 UTF-8 字节。
        b.write_all(&[0xff, 0xfe, 0xfd]).unwrap();

        assert_eq!(preview_path(&d.join("empty.bin")).unwrap().kind, PreviewKind::Empty);
        let bin = preview_path(&d.join("blob.bin")).unwrap();
        assert_eq!(bin.kind, PreviewKind::Binary);
    }

    #[test]
    fn image_returns_path_without_decoding() {
        let d = tmp("img");
        // 内容是否合法图片不影响「类型判定」：按扩展名识别。
        std::fs::write(d.join("pic.png"), b"not really png").unwrap();
        let pv = preview_path(&d.join("pic.png")).unwrap();
        assert_eq!(pv.kind, PreviewKind::Image);
        assert_eq!(pv.image, Some(d.join("pic.png")));
        assert!(pv.text.is_none());
    }
}
