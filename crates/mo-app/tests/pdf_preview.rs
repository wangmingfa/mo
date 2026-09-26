//! PDF 首页预览的整条链路：平台渲染 → 还原 alpha → PNG 落盘 → 再读回来。
//!
//! `mo-platform` 那边已经逐像素验过「渲染出来的是一张对的图」（见
//! `windows::tests::renders_the_first_page_of_a_real_pdf`），这里钉的是**它上面那一半**：
//! `AppState::preview_pdf_page` 的落盘位置、缓存命中与失效，以及「编码成 PNG 之后 UI
//! 还读得回来」。这一段是跨平台共用的，所以尺寸按 DPI 宽容差断言（macOS 按 72 DPI 出
//! 图，Windows 的 `PdfPage::Size` 是 96 DPI），只要求形状与内容还在。

#![cfg(any(target_os = "macos", target_os = "windows"))]

use mo_app::AppState;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// 手搓一份最小可解析的 PDF：一页 100×200 点，左下角一个 60×60 的纯蓝方块。
///
/// 与 `mo-platform` 那份同源：不引依赖、不下载样本就能验货。xref 的字节偏移按拼装
/// 过程现算——算错了会让解析器走「修复」路径，能不能救回来全看运气。
fn sample_pdf() -> Vec<u8> {
    let content = "0 0 1 rg\n20 20 60 60 re\nf\n";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] /Contents 4 0 R >>".to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// 把缓存目录钉进临时目录，返回它（渲染产物与搜索索引都落到这里，不碰真实缓存）。
///
/// ⚠️ 调用方必须先拿 [`env_lock`]：`MO_CACHE_DIR` 是**进程级**变量，而同二进制的测试
/// 默认并行——A 刚指到自己的目录，B 又改指过去，A 的「第二次调用命中同一个文件」就
/// 会随调度时机随机翻车。
fn temp_cache(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("临时目录建得出来");
    std::env::set_var("MO_CACHE_DIR", &dir);
    dir
}

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 渲染产物落盘、UI 读得回来，而且第二次调用命中缓存、改过 mtime 就失效。
#[test]
fn pdf_first_page_is_rendered_and_cached() {
    let _guard = env_lock();
    let dir = temp_cache("pdf-render");
    let pdf = dir.join("sample.pdf");
    std::fs::write(&pdf, sample_pdf()).expect("示例 PDF 写得出去");

    let app = AppState::new();
    let png = app.preview_pdf_page(&pdf).expect("首页要渲染得出来");
    assert!(png.exists(), "返回的必须是落盘了的文件：{}", png.display());
    assert_eq!(
        png.parent().and_then(|p| p.file_name()),
        Some("pdf-preview".as_ref()),
        "渲染产物落在 <缓存>/pdf-preview 下"
    );

    let bm = mo_thumbnails::decode_bitmap(&png).expect("UI 读得回这张 PNG");
    let (w, h) = (bm.width(), bm.height());
    // 100×200 点的一页：72 DPI 是 100×200，96 DPI 是 133×267。
    assert!((90..=140).contains(&w), "宽该在 100~133 之间，实际 {w}");
    assert!(
        (w as usize * 19 / 10..=w as usize * 21 / 10).contains(&(h as usize)),
        "高约等于宽的 2 倍（100×200 的一页），实际 {w}×{h}"
    );
    assert_eq!(bm.bgra().len(), w as usize * h as usize * 4);
    let px = bm.bgra().as_chunks::<4>().0;
    assert!(px.iter().all(|p| p[3] == 255), "铺过白纸的首页处处不透明");
    // BGRA：蓝方块是 `(255, 0, 0)`。一个都没有说明「渲染 → PNG」中间把内容弄丢了。
    let blue = px.iter().filter(|p| p[0] > 200 && p[2] < 60).count();
    assert!(
        blue > (w * h / 20) as usize,
        "方块得占到画面一成以上，实际 {blue} 个像素"
    );

    // 第二次：同一份 PDF 没改过 → 命中同一个文件，不再渲染。
    assert_eq!(
        app.preview_pdf_page(&pdf),
        Some(png.clone()),
        "同一份 PDF 命中缓存"
    );

    // 改过 mtime → 键变了，得重渲染成**另一个**文件（否则改完 PDF 预览还是旧图）。
    touch(&pdf);
    let again = app
        .preview_pdf_page(&pdf)
        .expect("改过的 PDF 照样渲染得出来");
    assert_ne!(
        again, png,
        "mtime 变了就不能再吃旧缓存：{again:?} vs {png:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 把 mtime 推到现在：键里带的是 `SystemTime`（纳秒精度），一次 `set_modified` 就够。
fn touch(path: &Path) {
    let f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("文件在");
    f.set_modified(std::time::SystemTime::now())
        .expect("改得动 mtime");
}

/// 扩展名骗人 → `None`：调用方退回占位文案，绝不拿这个 PDF 的路径去喂 `img()`。
#[test]
fn a_file_that_isnt_a_pdf_renders_nothing() {
    let _guard = env_lock();
    let dir = temp_cache("pdf-junk");
    let liar = dir.join("liar.pdf");
    std::fs::write(&liar, b"plain text with a pdf suffix").expect("写得动");
    assert!(AppState::new().preview_pdf_page(&liar).is_none());
    std::fs::remove_dir_all(&dir).ok();
}
