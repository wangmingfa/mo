use std::path::{Path, PathBuf};

use mo_core::FileId;
use mo_thumbnails::{preview_scaled_in, ThumbnailCache, DEFAULT_SIZE, PREVIEW_MAX_EDGE};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-thumb-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

/// 写一张彩色测试图，返回路径。
fn write_test_image(dir: &Path, w: u32, h: u32) -> PathBuf {
    let path = dir.join("source.png");
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 255) as u8, (y % 255) as u8, 96])
    });
    img.save(&path).expect("写入测试图失败");
    path
}

#[test]
fn generates_thumbnail_within_requested_size() {
    let dir = tmp("basic");
    let src = write_test_image(&dir, 640, 480);
    let cache = ThumbnailCache::with_root(dir.join("thumbs"));
    let id = FileId::new(1, 7);

    let out = cache
        .get_or_create(&id, &src, DEFAULT_SIZE)
        .expect("生成缩略图失败");

    assert!(out.exists(), "缩略图文件应存在");
    let thumb = image::open(&out).expect("缩略图无法解码");
    assert!(
        thumb.width() <= DEFAULT_SIZE && thumb.height() <= DEFAULT_SIZE,
        "缩略图不应超过请求尺寸，实际 {}x{}",
        thumb.width(),
        thumb.height()
    );
    // 等比缩放：长边贴到 128。
    assert_eq!(thumb.width(), DEFAULT_SIZE);
}

#[test]
fn second_call_hits_disk_cache() {
    let dir = tmp("cached");
    let src = write_test_image(&dir, 800, 600);
    let cache = ThumbnailCache::with_root(dir.join("thumbs"));
    let id = FileId::new(1, 9);

    let first = cache.get_or_create(&id, &src, DEFAULT_SIZE).unwrap();
    let mtime = std::fs::metadata(&first).unwrap().modified().unwrap();

    let second = cache.get_or_create(&id, &src, DEFAULT_SIZE).unwrap();
    assert_eq!(first, second, "应命中同一个缓存文件");

    // 命中缓存不应重写文件。
    let mtime2 = std::fs::metadata(&second).unwrap().modified().unwrap();
    assert_eq!(mtime, mtime2, "命中缓存时不应重新生成");
}

#[test]
fn cache_key_is_file_id_not_path() {
    let dir = tmp("by-id");
    let src = write_test_image(&dir, 400, 300);
    let cache = ThumbnailCache::with_root(dir.join("thumbs"));
    let id = FileId::new(1, 11);

    let first = cache.get_or_create(&id, &src, DEFAULT_SIZE).unwrap();

    // 文件改名后（路径变了，id 不变）依然命中同一份缓存。
    let renamed = dir.join("renamed.png");
    std::fs::rename(&src, &renamed).unwrap();
    let second = cache.get_or_create(&id, &renamed, DEFAULT_SIZE).unwrap();

    assert_eq!(first, second);
}

/// 缓存文件名必须能直接当文件名用。
///
/// 这条是 Windows 上那个 bug 的回归：键原来取 `FileId` 的 `Display`（`volume:id`），
/// 冒号在 Windows 是非法文件名字符，`CreateFile` 回 `ERROR_INVALID_PARAMETER`
/// （os error 87），整棵缩略图缓存与预览降采样副本都写不下去。
#[test]
fn cached_path_file_name_is_legal_on_every_platform() {
    const ILLEGAL: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let cache = ThumbnailCache::with_root(tmp("legal-name").join("thumbs"));
    let ids = [
        FileId::new(1, 7),
        FileId::new(u64::MAX, u128::MAX),
        FileId::synthetic(Path::new("/tmp/照片.png")),
    ];
    for id in ids {
        let p = cache.cached_path(&id, DEFAULT_SIZE);
        let name = p
            .file_name()
            .unwrap_or_else(|| panic!("{id} 的缓存路径没有文件名：{p:?}"))
            .to_string_lossy()
            .to_string();
        for c in ILLEGAL {
            assert!(
                !name.contains(c),
                "缓存文件名 {name:?} 含非法字符 {c:?}（id={id}）——Windows 上会 os error 87"
            );
        }
    }
}

#[test]
fn non_image_files_are_rejected() {
    let dir = tmp("not-image");
    let cache = ThumbnailCache::with_root(dir.join("thumbs"));
    let txt = dir.join("note.txt");
    std::fs::write(&txt, b"hello").unwrap();

    assert!(cache
        .get_or_create(&FileId::new(1, 1), &txt, DEFAULT_SIZE)
        .is_err());
}

// ---- 快速预览的降采样副本 ----

#[test]
fn oversized_image_is_downscaled_for_preview() {
    let dir = tmp("preview-big");
    let root = dir.join("preview");
    // 长边 3000 > PREVIEW_MAX_EDGE(2560)，应当生成副本。
    let src = write_test_image(&dir, 3000, 2000);

    let out = preview_scaled_in(&root, &src, PREVIEW_MAX_EDGE).expect("超尺寸图应有降采样副本");

    assert!(out.exists(), "降采样副本应存在");
    let scaled = image::open(&out).expect("降采样副本无法解码");
    assert_eq!(
        scaled.width(),
        PREVIEW_MAX_EDGE,
        "长边应贴到上限，实际 {}x{}",
        scaled.width(),
        scaled.height()
    );
    // 3000×2000 → 长边贴到 2560，短边按比例留给有向下的取整误差。
    assert!(
        (1706..=1708).contains(&scaled.height()),
        "短边应按比例缩放，实际 {}x{}",
        scaled.width(),
        scaled.height()
    );
}

#[test]
fn small_image_has_no_preview_copy() {
    let dir = tmp("preview-small");
    let root = dir.join("preview");
    let src = write_test_image(&dir, 800, 600);

    // 本来就够小：不该多生成一份磁盘副本，直接加载原图。
    assert_eq!(preview_scaled_in(&root, &src, PREVIEW_MAX_EDGE), None);
    assert!(!root.exists(), "没有超尺寸时不该创建缓存目录");
}

#[test]
fn preview_copy_is_reused_from_disk_cache() {
    let dir = tmp("preview-cached");
    let root = dir.join("preview");
    let src = write_test_image(&dir, 3000, 1000);

    let first = preview_scaled_in(&root, &src, PREVIEW_MAX_EDGE).unwrap();
    let mtime = std::fs::metadata(&first).unwrap().modified().unwrap();
    let second = preview_scaled_in(&root, &src, PREVIEW_MAX_EDGE).unwrap();

    assert_eq!(first, second, "应命中同一份缓存");
    let mtime2 = std::fs::metadata(&second).unwrap().modified().unwrap();
    assert_eq!(mtime, mtime2, "命中缓存时不应重新解码生成");
}

/// 写一张大 JPEG（照片场景）。
fn write_test_jpeg(dir: &Path, w: u32, h: u32) -> PathBuf {
    let path = dir.join("photo.jpg");
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 255) as u8, (y % 255) as u8, 128])
    });
    img.save_with_format(&path, image::ImageFormat::Jpeg)
        .expect("写入测试 JPEG 失败");
    path
}

/// 写一张带 alpha 的大 PNG（截图场景）。
fn write_test_rgba_png(dir: &Path, w: u32, h: u32) -> PathBuf {
    let path = dir.join("shot.png");
    let img = image::RgbaImage::from_fn(w, h, |x, _y| image::Rgba([(x % 255) as u8, 64, 200, 128]));
    img.save(&path).expect("写入测试 PNG 失败");
    path
}

/// 预览产物的编码格式必须跟源图语义匹配。
///
/// 照片（JPEG）走 JPEG：debug 构建下 PNG 编码一张 2560 的副本要 1.28s、产物 5MB，
/// 换 JPEG q85 后 0.32s / 1.1MB（见 `mo_thumbnails::Encoded`）。
/// 带 alpha 的必须留在 PNG——转 JPEG 会把透明区压成黑底。
///
/// 扩展名不只是好看：上层用 gpui 的 `img(path)` 加载，它**按扩展名**挑解码器
/// （`gpui::Img::extensions()`），扩展名与真实内容对不上会直接解码失败。
#[test]
fn preview_copy_encoding_follows_the_source() {
    let dir = tmp("preview-encoding");
    let root = dir.join("preview");

    // 照片 → JPEG，且内容真的是 JPEG（SOI 标记），缓存键也认得出来。
    let jpg = write_test_jpeg(&dir, 3000, 2000);
    let from_jpeg = preview_scaled_in(&root, &jpg, PREVIEW_MAX_EDGE).expect("JPEG 应有降采样副本");
    assert_eq!(
        from_jpeg.extension().and_then(|e| e.to_str()),
        Some("jpg"),
        "照片的副本应为 JPEG（PNG 在 debug 下编码要 1.28s、产物 5MB）"
    );
    let bytes = std::fs::read(&from_jpeg).expect("读副本失败");
    assert_eq!(
        &bytes[..2],
        &[0xFF, 0xD8],
        "后缀写了 jpg，内容也必须是 JPEG"
    );
    assert_eq!(
        preview_scaled_in(&root, &jpg, PREVIEW_MAX_EDGE),
        Some(from_jpeg.clone()),
        "同格式应命中同一份缓存"
    );

    // 带 alpha 的 PNG → 留在 PNG。
    let png = write_test_rgba_png(&dir, 3000, 800);
    let from_png = preview_scaled_in(&root, &png, PREVIEW_MAX_EDGE).expect("PNG 应有降采样副本");
    assert_eq!(
        from_png.extension().and_then(|e| e.to_str()),
        Some("png"),
        "带 alpha 的源必须留在 PNG 里，否则透明区会被压成黑底"
    );
    let bytes = std::fs::read(&from_png).expect("读副本失败");
    assert_eq!(
        &bytes[..4],
        &[0x89, b'P', b'N', b'G'],
        "后缀写了 png，内容也必须是 PNG"
    );
    assert_ne!(from_jpeg, from_png, "两种编码的缓存文件不该撞名");
}

#[test]
fn preview_downscale_failures_fall_back_to_original() {
    let dir = tmp("preview-fallback");
    let root = dir.join("preview");
    let txt = dir.join("note.txt");
    std::fs::write(&txt, b"hello").unwrap();

    // 非图片：指标撑不起尺寸探测，返回 None 让调用方用原图，而不是让预览失败。
    assert_eq!(preview_scaled_in(&root, &txt, PREVIEW_MAX_EDGE), None);
}

/// 平台层交出来的图标像素要能原样编成 PNG（尺寸、通道序、alpha 都不许走样）。
#[test]
fn rgba_pixels_round_trip_through_png() {
    // 2×2：左上不透明红、右上全透明、左下半透明绿、右下不透明蓝。
    let rgba: Vec<u8> = vec![
        255, 0, 0, 255, //
        0, 0, 0, 0, //
        0, 128, 0, 128, //
        0, 0, 255, 255,
    ];
    let png = mo_thumbnails::encode_rgba_png(2, 2, &rgba).expect("应能编码");
    assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G'], "产物必须是 PNG");

    let back = image::load_from_memory(&png).expect("解码失败").to_rgba8();
    assert_eq!((back.width(), back.height()), (2, 2));
    assert_eq!(back.as_raw(), &rgba, "像素（含 alpha）必须原样往返");
}

/// 长度与尺寸对不上时返回 `None`，别 panic——调用方按「这张图标没取到」处理。
#[test]
fn rgba_encoding_rejects_mismatched_lengths() {
    assert!(mo_thumbnails::encode_rgba_png(2, 2, &[0u8; 15]).is_none());
    assert!(mo_thumbnails::encode_rgba_png(0, 0, &[]).is_none());
}

/// AppKit 的位图是**预乘**的，编码前还原成直通 alpha——否则半透明边缘发暗。
#[test]
fn premultiplied_pixels_are_restored() {
    // alpha=128 的纯红，预乘后红通道是 128；还原后应回到 255。
    let mut px = vec![128u8, 64, 0, 128];
    mo_thumbnails::unpremultiply_rgba(&mut px);
    assert_eq!(px, vec![255u8, 128, 0, 128]);

    // 全透明像素保持全 0（除法要绕开，别变成 NaN/除零）。
    let mut clear = vec![0u8, 0, 0, 0];
    mo_thumbnails::unpremultiply_rgba(&mut clear);
    assert_eq!(clear, vec![0u8, 0, 0, 0]);

    // 不透明的像素本来就等同直通 alpha，不该被改动。
    let mut opaque = vec![10u8, 20, 30, 255];
    mo_thumbnails::unpremultiply_rgba(&mut opaque);
    assert_eq!(opaque, vec![10u8, 20, 30, 255]);
}
