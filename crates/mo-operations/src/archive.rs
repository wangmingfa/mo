//! archive：压缩与解压。
//!
//! 支持 zip（deflate）、tar、tar.gz；7z / rar 等由外部工具负责，
//! 这里只做**不引入额外 native 依赖**就能覆盖的格式。

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
    let options = zip::write::FileOptions::default()
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

/// 解压 `archive` 到 `dest`（自动识别 zip / tar / tar.gz）。
pub fn extract_archive(archive: &Path, dest: &Path) -> Result<usize, MoError> {
    std::fs::create_dir_all(dest).map_err(MoError::Io)?;
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.ends_with(".zip") {
        return extract_zip(archive, dest);
    }
    extract_tar(
        archive,
        dest,
        name.ends_with(".tar.gz") || name.ends_with(".tgz"),
    )
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
