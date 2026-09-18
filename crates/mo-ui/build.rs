//! 构建脚本：把 `assets/icon.png` 转成多尺寸 `.ico`，并以整数资源 ID 1 内嵌进
//! Windows 可执行文件。
//!
//! 为什么这么做：gpui 的 Windows 后端在注册窗口类时用
//! `LoadImageW(module, MAKEINTRESOURCE(1), IMAGE_ICON, …)` 取图标
//! （见 `gpui-pre-windows/src/platform.rs::load_icon`）。exe 里没有 ID=1 的图标
//! 资源就回退成通用图标，任务栏 / 标题栏 / 资源管理器里都看不到 Mo 自己的图标。
//! 这里补上该资源即可，无需改动运行期代码。
//!
//! 非 Windows 平台本脚本是 no-op（图标资源段也不作为依赖参与编译）。

fn main() {
    // 源图或本脚本变化时重跑。
    println!("cargo:rerun-if-changed=../../assets/icon.png");
    println!("cargo:rerun-if-changed=build.rs");
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    use image::{
        codecs::ico::{IcoEncoder, IcoFrame},
        ExtendedColorType,
    };

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let png = manifest_dir.join("../../assets/icon.png");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ico_path = out_dir.join("mo-icon.ico");

    // 1024² 源图 → 常用尺寸，逐帧 Lanczos 重采样后以 PNG 压缩存入 ICO。
    let base = image::open(&png)
        .unwrap_or_else(|e| panic!("读取图标源 {} 失败: {e}", png.display()))
        .to_rgba8();
    // icon.png 是 macOS 风格：图形四周留了一圈透明边（内容约占 82%）。macOS Dock
    // 需要这圈留白，但 Windows 任务栏图标是满格的，带着它就显得比邻居小一圈。
    // 这里按不透明像素的包围盒裁掉透明边，让图形填满 ICO 画框。
    let base = crop_to_content(&base);
    let mut frames = Vec::new();
    for &s in &[16u32, 24, 32, 48, 64, 128, 256] {
        let img = image::imageops::resize(&base, s, s, image::imageops::FilterType::Lanczos3);
        frames.push(
            IcoFrame::as_png(img.as_raw(), s, s, ExtendedColorType::Rgba8)
                .unwrap_or_else(|e| panic!("构造 ICO 帧 {s}px 失败: {e}")),
        );
    }
    let file = fs::File::create(&ico_path).expect("创建 mo-icon.ico 失败");
    IcoEncoder::new(file)
        .encode_images(&frames)
        .expect("写入 ICO 失败");

    // .rc：整数 ID 1 的 ICON 资源，正好匹配 gpui 的 MAKEINTRESOURCE(1)。
    // 用正斜杠路径，rc.exe 两者都接受。
    let rc_path = out_dir.join("mo-icon.rc");
    let ico_fwd = ico_path.display().to_string().replace('\\', "/");
    fs::write(&rc_path, format!("1 ICON \"{ico_fwd}\"\n")).expect("写入 .rc 失败");

    match embed_resource::compile(&rc_path, embed_resource::NONE) {
        embed_resource::CompilationResult::Ok | embed_resource::CompilationResult::NotWindows => {}
        embed_resource::CompilationResult::NotAttempted(reason) => {
            // 找不到 rc.exe / Windows SDK：不阻断构建，但明确提示图标未内嵌。
            println!("cargo:warning=未内嵌 Windows 图标资源（任务栏将显示默认图标）：{reason}");
        }
        embed_resource::CompilationResult::Failed(reason) => {
            panic!("内嵌 Windows 图标资源失败：{reason}");
        }
    }
}

/// 按不透明像素的包围盒裁剪，去掉 macOS 图标四周的透明留白。
///
/// alpha 阈值取 8：忽略抗锯齿最外缘的近零透明像素，避免包围盒被淡边撑大。
/// 全透明（理论上不会发生）时原样返回。
#[cfg(windows)]
fn crop_to_content(img: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let (mut min_x, mut min_y) = (w, h);
    let (mut max_x, mut max_y) = (0u32, 0u32);
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            if img.get_pixel(x, y)[3] > 8 {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if !found {
        return img.clone();
    }
    image::imageops::crop_imm(img, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1).to_image()
}

#[cfg(not(windows))]
fn embed_icon() {}