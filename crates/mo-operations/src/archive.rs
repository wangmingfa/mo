//! archive：压缩与解压。
//!
//! 内置（无额外 native 依赖）覆盖 zip（deflate）、tar、tar.gz；
//! 7z / rar / tar.bz2 / tar.xz 等交给外部工具（7-Zip / 系统 tar），
//! 见 [`extract_external`]。`is_extractable` 对这两类都返回 `true`，
//! 工具不存在时由 `extract_archive` 当场说人话（指明装哪个）。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use mo_core::MoError;
use walkdir::WalkDir;

/// 可创建的归档格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
}

impl ArchiveFormat {
    /// 按目标文件名推断格式（默认 zip）。
    pub fn from_path(path: &Path) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase());
        match name.as_deref() {
            Some(n) if n.ends_with(".tar.gz") || n.ends_with(".tgz") => ArchiveFormat::TarGz,
            Some(n) if n.ends_with(".tar") => ArchiveFormat::Tar,
            _ => ArchiveFormat::Zip,
        }
    }

    pub fn extension_hint(&self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::TarGz => "tar.gz",
        }
    }
}

/// 需要**外部工具**才能解的格式（Rust 原生库覆盖不到）。
///
/// 与 [`ArchiveFormat`]（内置）是两条判据，按扩展名分派，不探测工具是否存在——
/// 探测放在真正解压时（`extract_external`），这样菜单该不该给「解压」不随用户
/// 有没有装 7z 而闪烁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalFormat {
    /// 7z / rar 等：交给 7-Zip（`7z` / `7za`）。
    SevenZip,
    /// tar.bz2 / tar.xz 等：交给系统 `tar`（libarchive 通常已支持 bz2/xz）。
    Bsdtar,
}

impl ExternalFormat {
    /// 该格式在哪些候选二进制名下能解（按优先级试）。
    pub fn binaries(&self) -> &'static [&'static str] {
        match self {
            ExternalFormat::SevenZip => &["7z", "7za"],
            ExternalFormat::Bsdtar => &["tar"],
        }
    }

    /// 没装工具时，提示用户去装哪一个（说人话，别只报退出码）。
    pub fn tool_hint(&self) -> &'static str {
        match self {
            ExternalFormat::SevenZip => "7-Zip（命令 `7z` / `7za`）",
            ExternalFormat::Bsdtar => "系统 `tar`（libarchive 版）",
        }
    }
}

/// 按扩展名判断一个归档是否**需要外部工具**解（不看内容、不探测工具是否存在）。
pub fn external_extract_format(path: &Path) -> Option<ExternalFormat> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())?;
    if name.ends_with(".7z") || name.ends_with(".rar") {
        return Some(ExternalFormat::SevenZip);
    }
    if name.ends_with(".tar.bz2")
        || name.ends_with(".tbz2")
        || name.ends_with(".tar.xz")
        || name.ends_with(".txz")
    {
        return Some(ExternalFormat::Bsdtar);
    }
    None
}

/// 把 `sources` 打包进 `dest`。
///
/// `sources` 里每个条目归档为「自身的相对名」：目录递归打包。
pub fn create_archive(dest: &Path, sources: &[PathBuf]) -> Result<(), MoError> {
    match ArchiveFormat::from_path(dest) {
        ArchiveFormat::Zip => write_zip(dest, sources),
        ArchiveFormat::Tar => write_tar(dest, sources, false),
        ArchiveFormat::TarGz => write_tar(dest, sources, true),
    }
}

fn write_zip(dest: &Path, sources: &[PathBuf]) -> Result<(), MoError> {
    let file = File::create(dest).map_err(MoError::Io)?;
    let mut zip = zip::ZipWriter::new(file);
    // zip 8：FileOptions 带生命周期/压缩泛型参数，default() 推不出具体类型；
    // SimpleFileOptions 是「无注释 + 静态生命周期」的现成别名。
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for src in sources {
        let base = src
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        if src.is_dir() {
            for entry in WalkDir::new(src).follow_links(false) {
                let entry = entry.map_err(|e| MoError::Other(e.to_string()))?;
                let path = entry.path();
                let name = relative_name(path, &base);
                if entry.file_type().is_dir() {
                    zip.add_directory(name, options)
                        .map_err(|e| MoError::Other(e.to_string()))?;
                } else {
                    zip.start_file(name, options)
                        .map_err(|e| MoError::Other(e.to_string()))?;
                    let mut f = File::open(path).map_err(MoError::Io)?;
                    std::io::copy(&mut f, &mut zip).map_err(MoError::Io)?;
                }
            }
        } else {
            zip.start_file(relative_name(src, &base), options)
                .map_err(|e| MoError::Other(e.to_string()))?;
            let mut f = File::open(src).map_err(MoError::Io)?;
            std::io::copy(&mut f, &mut zip).map_err(MoError::Io)?;
        }
    }
    zip.finish()
        .map_err(|e| MoError::Other(format!("写入 zip 收尾失败：{e}")))?;
    Ok(())
}

fn write_tar(dest: &Path, sources: &[PathBuf], gzip: bool) -> Result<(), MoError> {
    let file = File::create(dest).map_err(MoError::Io)?;
    let mut builder = tar::Builder::new(if gzip {
        Box::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::default(),
        )) as Box<dyn Write>
    } else {
        Box::new(file) as Box<dyn Write>
    });
    for src in sources {
        let base = src
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        if src.is_dir() {
            builder
                .append_dir_all(relative_name(src, &base), src)
                .map_err(|e| MoError::Other(e.to_string()))?;
        } else {
            let mut f = File::open(src).map_err(MoError::Io)?;
            builder
                .append_file(relative_name(src, &base), &mut f)
                .map_err(|e| MoError::Other(e.to_string()))?;
        }
    }
    builder
        .finish()
        .map_err(|e| MoError::Other(format!("写入 tar 收尾失败：{e}")))?;
    Ok(())
}

/// 这个压缩包我们**解得开**吗（按扩展名判，不看内容）。
///
/// 与 [`ArchiveFormat::from_path`]（**打包**用）是两条判据，别合并：打包时认不出
/// 的后缀一律按 zip 处理（给个能用的结果），解压时认不出就必须直说——把 `.7z`
/// 丢给 tar 只会得到一句看不懂的「不是 tar」（2026-09-29 用户报的胡话）。
pub fn extract_format(path: &Path) -> Option<ArchiveFormat> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())?;
    if name.ends_with(".zip") {
        return Some(ArchiveFormat::Zip);
    }
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return Some(ArchiveFormat::TarGz);
    }
    if name.ends_with(".tar") {
        return Some(ArchiveFormat::Tar);
    }
    None
}

/// 右键菜单 / 命令面板「要不要给解压这一项」用同一条判据（[`extract_format`] +
/// [`external_extract_format`]）：内置解得开、或外部工具解得开的格式都给这一项。
pub fn is_extractable(path: &Path) -> bool {
    extract_format(path).is_some() || external_extract_format(path).is_some()
}

/// 报错里要指名道姓「是哪个格式」——只说「不支持」等于没说。
fn archive_suffix(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    // 复合后缀整体报（`a.tar.bz2` 报「tar.bz2」而不是「bz2」）。
    for s in [".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".tbz2", ".txz"] {
        if name.ends_with(s) {
            return s.trim_start_matches('.').to_string();
        }
    }
    name.rsplit('.').next().unwrap_or(&name).to_string()
}

/// 解压 `archive` 到 `dest`。
///
/// 先认内置格式（zip / tar / tar.gz），再认外部工具格式（7z / rar / tar.bz2 /
/// tar.xz），都不中才**当场报错**（列出支持的那些）。不往下试内置库：把 `.7z`
/// 丢给 tar 只会得到一句看不懂的「不是 tar」（2026-09-29 用户报的胡话）。
pub fn extract_archive(archive: &Path, dest: &Path) -> Result<usize, MoError> {
    if let Some(fmt) = extract_format(archive) {
        std::fs::create_dir_all(dest).map_err(MoError::Io)?;
        return match fmt {
            ArchiveFormat::Zip => extract_zip(archive, dest),
            ArchiveFormat::Tar => extract_tar(archive, dest, false),
            ArchiveFormat::TarGz => extract_tar(archive, dest, true),
        };
    }
    if let Some(fmt) = external_extract_format(archive) {
        return extract_external(archive, dest, fmt);
    }
    Err(MoError::Other(format!(
        "暂不支持解压 {}（目前支持 zip / tar / tar.gz / tgz，以及 7z / rar / tar.bz2 / tar.xz（需安装外部工具））",
        archive_suffix(archive)
    )))
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<usize, MoError> {
    let file = File::open(archive).map_err(MoError::Io)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| MoError::Other(e.to_string()))?;
    let mut count = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| MoError::Other(e.to_string()))?;
        let out = dest.join(entry.name());
        // 目录项直接建目录；文件项自动创建父目录后写盘。
        if entry.name().ends_with('/') {
            std::fs::create_dir_all(&out).map_err(MoError::Io)?;
            count += 1;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(MoError::Io)?;
        }
        let mut f = File::create(&out).map_err(MoError::Io)?;
        std::io::copy(&mut entry, &mut f).map_err(MoError::Io)?;
        count += 1;
    }
    Ok(count)
}

fn extract_tar(archive: &Path, dest: &Path, gzip: bool) -> Result<usize, MoError> {
    let file = File::open(archive).map_err(MoError::Io)?;
    let mut ar = tar::Archive::new(if gzip {
        Box::new(flate2::read::GzDecoder::new(file)) as Box<dyn Read>
    } else {
        Box::new(file) as Box<dyn Read>
    });
    let entries = ar.entries().map_err(|e| MoError::Other(e.to_string()))?;
    let mut count = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|e| MoError::Other(e.to_string()))?;
        entry
            .unpack_in(dest)
            .map_err(|e| MoError::Other(e.to_string()))?;
        count += 1;
    }
    Ok(count)
}

/// 借外部工具解压（7z / rar / tar.bz2 / tar.xz 等 Rust 库覆盖不到的格式）。
///
/// 安全：归档路径与目标目录都是 `Command` 的**独立参数**（OsString），绝不拼进
/// shell——路径含空格、中文也安全，且无命令注入风险。调用方在 `spawn_blocking`
/// 里跑，不阻塞 GPUI 执行器。Windows 上加 `CREATE_NO_WINDOW` 避免 GUI 应用弹出
/// 黑框（见 `windows-port.md` §11）。
fn extract_external(archive: &Path, dest: &Path, fmt: ExternalFormat) -> Result<usize, MoError> {
    std::fs::create_dir_all(dest).map_err(MoError::Io)?;
    for bin in fmt.binaries() {
        let mut cmd = std::process::Command::new(bin);
        match fmt {
            ExternalFormat::SevenZip => {
                // `x` = 按归档内结构解压（不套一层根目录）；`-y` = 全部确认；
                // `-o` 与路径**紧挨无空格**（7z 语法）。
                cmd.arg("x")
                    .arg(archive)
                    .arg(format!("-o{}", dest.display()))
                    .arg("-y");
            }
            ExternalFormat::Bsdtar => {
                cmd.arg("-xf").arg(archive).arg("-C").arg(dest);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        match cmd.output() {
            Ok(out) if out.status.success() => return Ok(1),
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr);
                return Err(MoError::Other(format!(
                    "用 {} 解压 {} 失败（退出码 {}）：{}",
                    bin,
                    archive.display(),
                    out.status,
                    first_lines(&msg, 3)
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                continue; // 试下一个候选二进制（如 7z 不在就试 7za）
            }
            Err(e) => return Err(MoError::Other(format!("启动 {} 失败：{e}", bin))),
        }
    }
    Err(MoError::Other(format!(
        "找不到能解压 {} 的工具 {}。请先安装 {}，再重试这一操作（内置只覆盖 zip/tar/tar.gz/tgz）。",
        archive_suffix(archive),
        fmt.tool_hint(),
        fmt.tool_hint()
    )))
}

/// 取一段报错文本的前几行，避免把外部工具的一大坨 stderr 全塞进界面提示。
fn first_lines(s: &str, n: usize) -> String {
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 归档内的相对名（相对于打包条目的父目录）。
fn relative_name(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个用例用独立目录：并行测试共享同一临时目录会互相删掉素材。
    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mo-archive-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"aaa").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), b"bbbb").unwrap();
        dir
    }

    #[test]
    fn zip_roundtrip() {
        let src = fixture("zip");
        let out = src.join("pack.zip");
        create_archive(&out, &[src.join("a.txt"), src.join("sub")]).unwrap();
        let dest = src.join("unzipped");
        let n = extract_archive(&out, &dest).unwrap();
        assert!(n >= 2, "解压出的条目太少：{n}");
        assert_eq!(
            std::fs::read_to_string(dest.join("a.txt")).unwrap(),
            "aaa",
            "文件内容未还原"
        );
        let _ = std::fs::remove_dir_all(&src);
    }

    /// 外部工具格式（7z / rar / tar.bz2 / tar.xz）现在归可解家族：菜单该给
    /// 「解压」，且按扩展名分派到正确的工具（不探测可用性，避免菜单闪烁）。
    #[test]
    fn external_formats_are_now_recognized() {
        assert_eq!(
            external_extract_format(Path::new("a.7z")),
            Some(ExternalFormat::SevenZip)
        );
        assert_eq!(
            external_extract_format(Path::new("a.rar")),
            Some(ExternalFormat::SevenZip)
        );
        assert_eq!(
            external_extract_format(Path::new("a.tar.bz2")),
            Some(ExternalFormat::Bsdtar)
        );
        assert_eq!(
            external_extract_format(Path::new("a.tbz2")),
            Some(ExternalFormat::Bsdtar)
        );
        assert_eq!(
            external_extract_format(Path::new("a.tar.xz")),
            Some(ExternalFormat::Bsdtar)
        );
        assert_eq!(
            external_extract_format(Path::new("a.txz")),
            Some(ExternalFormat::Bsdtar)
        );
        assert_eq!(
            external_extract_format(Path::new("a.zip")),
            None,
            "内置格式不走外部分支"
        );
        for name in ["a.7z", "a.rar", "a.tar.bz2", "a.tar.xz", "a.tbz2", "a.txz"] {
            assert!(
                is_extractable(Path::new(name)),
                "{name} 现在该经外部工具解压"
            );
        }
    }

    /// 真正不认识的格式仍要**指名道姓**地说（§33 的反向验证精神保留）：
    /// `.7z` 等已经改走外部工具，这里用 `.zzz` 守「完全不认识」这一支。
    #[test]
    fn truly_unknown_format_still_says_which_one() {
        let dir = fixture("unknown");
        let p = dir.join("pack.zzz");
        std::fs::write(&p, b"not really an archive").unwrap();
        assert!(!is_extractable(&p), "完全不认识的格式不该给解压");
        let err = extract_archive(&p, &dir.join("out"))
            .expect_err("该报错")
            .to_string();
        assert!(err.contains("zzz"), "报错要写明是哪个格式：{err}");
        assert!(
            err.contains("zip / tar / tar.gz / tgz"),
            "报错要列出支持的格式：{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 解得开的四种照旧（别因为加了判据把能用的砍掉）。
    #[test]
    fn supported_formats_are_recognized() {
        for name in ["a.zip", "a.tar", "a.tar.gz", "a.tgz"] {
            assert!(is_extractable(Path::new(name)), "{name} 该解得开");
        }
    }

    #[test]
    fn tar_gz_roundtrip() {
        let src = fixture("targz");
        let out = src.join("pack.tar.gz");
        create_archive(&out, &[src.join("a.txt")]).unwrap();
        let dest = src.join("untar");
        extract_archive(&out, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "aaa");
        let _ = std::fs::remove_dir_all(&src);
    }
}
