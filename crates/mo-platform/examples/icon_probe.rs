//! 临时探针：对给定路径逐个调 `file_icon_raster`，报告成功与否并保存像素。
//!
//! 用法：`cargo run -p mo-platform --example icon_probe -- /path/a /path/b ...`
//! 输出写在 `/tmp/mo-icon-test/probe-<序号>.raw`（RGBA，40x40）。

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::fs::create_dir_all("/tmp/mo-icon-test").unwrap();
    // 先验通用文件夹占位图（NSFolder 资产）：连取三次，应稳定 Some 且同一张图。
    // 先验通用文件夹占位图（NSFolder 资产）：连取三次，应稳定 Some 且同一张图。
    for i in 0..3 {
        let t0 = std::time::Instant::now();
        match mo_platform::folder_icon_raster(40) {
            Some(r) => {
                let opaque = r
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|c| c[3] > 128)
                    .count();
                std::fs::write(format!("/tmp/mo-icon-test/nsfolder-{i}.raw"), &r.rgba).unwrap();
                println!(
                    "nsfolder#{i} -> Some {}x{} opaque={opaque} ({:?})",
                    r.width,
                    r.height,
                    t0.elapsed()
                );
            }
            None => println!("nsfolder#{i} -> None ({:?})", t0.elapsed()),
        }
    }
    for (i, p) in args.iter().enumerate() {
        let path = std::path::Path::new(p);
        let exists = path.exists();
        let is_dir = path.is_dir();
        let t0 = std::time::Instant::now();
        let raster = mo_platform::file_icon_raster(path, 40);
        let dt = t0.elapsed();
        match raster {
            Some(r) => {
                // 简单统计：非透明像素数 + 平均 RGB，用来区分「文件夹」与「空白文档」。
                let px4 = r.rgba.as_chunks::<4>().0;
                let opaque = px4.iter().filter(|c| c[3] > 128).count();
                let sum_rgb: (u64, u64, u64) = px4
                    .iter()
                    .filter(|c| c[3] > 128)
                    .fold((0u64, 0u64, 0u64), |(a, b, c), px| {
                        (a + px[0] as u64, b + px[1] as u64, c + px[2] as u64)
                    });
                let out = format!("/tmp/mo-icon-test/probe-{i}.raw");
                std::fs::write(&out, &r.rgba).unwrap();
                let opaque64 = opaque as u64;
                println!(
                    "#{i} {p} exists={exists} is_dir={is_dir} -> Some {}x{} opaque={opaque} avg=({},{},{}) ({dt:?}) saved {out}",
                    r.width,
                    r.height,
                    sum_rgb.0 / opaque64.max(1),
                    sum_rgb.1 / opaque64.max(1),
                    sum_rgb.2 / opaque64.max(1),
                );
            }
            None => {
                println!("#{i} {p} exists={exists} is_dir={is_dir} -> None ({dt:?})");
            }
        }
    }
}
