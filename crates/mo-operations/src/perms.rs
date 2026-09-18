//! perms：权限读写。
//!
//! 权限改动是「瞬间完成」的小操作，因此不进操作队列，由 `mo-app`
//! 丢到 blocking 池里直接执行（UI 只等一次结果）。

use std::path::Path;

use mo_core::MoError;

/// 设置 unix 权限位（低 9 位）。非 unix 平台落到只读标记。
pub fn set_permissions(path: &Path, mode: u32) -> Result<(), MoError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode & 0o777);
        std::fs::set_permissions(path, perms).map_err(MoError::Io)
    }
    #[cfg(not(unix))]
    {
        let meta = std::fs::metadata(path).map_err(MoError::Io)?;
        let mut perms = meta.permissions();
        perms.set_readonly(mode & 0o222 == 0);
        std::fs::set_permissions(path, perms).map_err(MoError::Io)
    }
}

/// 把权限位格式化为 `rwxr-xr--` 形式（供属性面板展示）。
pub fn mode_string(mode: u32) -> String {
    const BITS: [(u32, char); 9] = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    BITS.iter()
        .map(|(bit, ch)| if mode & bit != 0 { *ch } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::mode_string;

    #[test]
    fn renders_rwx_triples() {
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o644), "rw-r--r--");
        assert_eq!(mode_string(0o000), "---------");
    }
}
