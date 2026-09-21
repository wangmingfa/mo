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

#[test]
fn preview_downscale_failures_fall_back_to_original() {
    let dir = tmp("preview-fallback");
    let root = dir.join("preview");
    let txt = dir.join("note.txt");
    std::fs::write(&txt, b"hello").unwrap();

    // 非图片：指标撑不起尺寸探测，返回 None 让调用方用原图，而不是让预览失败。
    assert_eq!(preview_scaled_in(&root, &txt, PREVIEW_MAX_EDGE), None);
}
