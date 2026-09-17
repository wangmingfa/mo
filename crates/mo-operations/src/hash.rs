use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use md5::Md5;
use mo_core::MoError;
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// 支持的哈希算法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
}

impl HashAlgo {
    /// 人类可读名称。
    pub fn name(self) -> &'static str {
        match self {
            HashAlgo::Md5 => "MD5",
            HashAlgo::Sha1 => "SHA-1",
            HashAlgo::Sha256 => "SHA-256",
        }
    }
}

/// 流式计算单个文件的多种哈希。
///
/// 分块读取（64KB/块），**不把整文件读进内存**——大文件（GB 级）也能算。
/// 只计算被请求的算法，避免无谓开销。
pub fn compute_hashes(
    path: &Path,
    algos: &[HashAlgo],
) -> Result<HashMap<HashAlgo, String>, MoError> {
    let mut file = std::fs::File::open(path).map_err(MoError::Io)?;
    let mut buf = [0u8; 64 * 1024];

    let mut m = if algos.iter().any(|a| matches!(a, HashAlgo::Md5)) {
        Some(Md5::new())
    } else {
        None
    };
    let mut s1 = if algos.iter().any(|a| matches!(a, HashAlgo::Sha1)) {
        Some(Sha1::new())
    } else {
        None
    };
    let mut s2 = if algos.iter().any(|a| matches!(a, HashAlgo::Sha256)) {
        Some(Sha256::new())
    } else {
        None
    };

    loop {
        let n = file.read(&mut buf).map_err(MoError::Io)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        if let Some(h) = &mut m {
            h.update(chunk);
        }
        if let Some(h) = &mut s1 {
            h.update(chunk);
        }
        if let Some(h) = &mut s2 {
            h.update(chunk);
        }
    }

    let mut out = HashMap::new();
    if let Some(h) = m {
        out.insert(HashAlgo::Md5, hex(h.finalize()));
    }
    if let Some(h) = s1 {
        out.insert(HashAlgo::Sha1, hex(h.finalize()));
    }
    if let Some(h) = s2 {
        out.insert(HashAlgo::Sha256, hex(h.finalize()));
    }
    Ok(out)
}

fn hex(d: impl AsRef<[u8]>) -> String {
    let mut s = String::with_capacity(d.as_ref().len() * 2);
    for b in d.as_ref() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hashes_of_abc() {
        let d = std::env::temp_dir().join(format!("mo-hash-{}", std::process::id()));
        std::fs::write(&d, b"abc").unwrap();

        let all = [HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256];
        let out = compute_hashes(&d, &all).unwrap();
        assert_eq!(out[&HashAlgo::Md5], "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            out[&HashAlgo::Sha1],
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            out[&HashAlgo::Sha256],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        // 只算 MD5 时不返回其他算法。
        let only = compute_hashes(&d, &[HashAlgo::Md5]).unwrap();
        assert_eq!(only.len(), 1);

        let _ = std::fs::remove_file(&d);
    }
}
