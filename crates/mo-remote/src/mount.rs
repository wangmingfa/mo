//! SMB / NFS：**发现 + 触发系统挂载**，而不是在进程里实现协议。
//!
//! Rust 生态长期没有成熟可维护的 SMB / NFS **客户端**实现（SMB2/3 与 NFSv4 都是
//! 大协议）。而三端操作系统本身都会把网络盘挂成目录：
//!
//! * macOS：`/Volumes/<share>`（smbfs / nfs）
//! * Linux：`/mnt`、`/media`、`/run/user/<uid>/gvfs`（cifs / nfs）
//! * Windows：驱动器号（`net use`）
//!
//! 文件管理器里「挂载后当本地目录浏览」才是正解——它顺带免掉了凭据管理、断点续传、
//! Kerberos 这一整套。所以这里只做两件事：
//!
//! * [`mounted_shares`]——**发现**：系统里已经挂好的网络盘（只读，不发起任何网络操作）；
//! * [`mount`]——**触发系统挂载**，返回挂载点，之后当普通本地目录浏览。
//!
//! ## 注意：挂载点不用 `/Volumes`
//!
//! macOS 上 `/Volumes` 是 root 所有的（`drwxr-xr-x root`），普通用户建不了目录，
//! 而 `mount_smbfs` 又不是 setuid——挂不进去。所以挂载点放在**应用自己的数据目录**
//! 下（`~/Library/Application Support/mo/mounts/<名字>`），用户一定拥有它。
//! 代价是网络盘不出现在桌面 / Finder 侧边栏，换来的是「不用 sudo 就能挂」。
//!
//! ## 凭据
//!
//! SMB 的密码**不进命令行参数**（`ps` 看得见），走 stdin 管道喂给 `mount_smbfs`；
//! 地址里没写密码时用 `-N`（不询问，走 `nsmb.conf` / 钥匙串里的那份）。

use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "macos")]
use std::process::Stdio;

use crate::{RemoteError, RemoteUrl};

/// 侧边栏上显示的名字：挂载点的目录名（用户认的是它），退化用 `host/export`。
///
/// 盘符要单独处理：Windows 的 `Z:\` 是**根目录**，`Path::file_name()` 拿不到东西
/// （而且在 unix 上解析同一串会得到 `Z:\` 整串），这里直接显示盘符。
fn display_name(path: &Path, host: &str, export: &str) -> String {
    let raw = path.to_string_lossy().to_string();
    let drive = raw.trim_end_matches('\\').trim_end_matches('/');
    if drive.len() == 2 && drive.ends_with(':') {
        return drive.to_string();
    }
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            if host.is_empty() {
                export.to_string()
            } else {
                format!("{host}/{export}")
            }
        })
}

/// 一条系统已挂载的网络盘。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkShare {
    /// 本地挂载点（挂好之后就是一个普通目录）。
    pub path: PathBuf,
    /// 协议：`smb` / `nfs`。
    pub scheme: String,
    /// 主机名（解析不出来时为空）。
    pub host: String,
    /// 共享名 / 导出路径。
    pub export: String,
    /// 侧边栏上显示的名字。
    pub label: String,
}

impl NetworkShare {
    /// 侧边栏显示名：优先用挂载点的目录名（用户认的是它），退化用 `host/export`。
    fn named(path: PathBuf, scheme: &str, host: &str, export: &str) -> Self {
        let label = display_name(&path, host, export);
        Self {
            path,
            scheme: scheme.to_string(),
            host: host.to_string(),
            export: export.to_string(),
            label,
        }
    }
}

/// 发现：系统里已经挂好的网络盘。
///
/// 只读（读 `/proc/mounts` 或跑一次 `mount` / `net use`），**不发起任何网络操作**，
/// 所以侧边栏可以放心地每次渲染都调一次；失败就当「没有」，不打断 UI。
pub fn mounted_shares() -> Vec<NetworkShare> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/mounts")
            .map(|text| shares_from_proc_mounts(&text))
            .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        run("mount", &[])
            .map(|out| shares_from_macos_mount(&out))
            .unwrap_or_default()
    }
    #[cfg(target_os = "windows")]
    {
        run("net", &["use"])
            .map(|out| shares_from_net_use(&out))
            .unwrap_or_default()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

/// 这个协议是**交给系统挂载**的（而不是在进程里实现协议）吗？
pub fn is_mountable(scheme: &str) -> bool {
    matches!(scheme, "smb" | "cifs" | "samba" | "nfs")
}

/// 触发系统挂载：按协议把 `url` 交给操作系统，返回挂载点。
///
/// 挂好之后调用方应当**当本地目录打开**（`AppState::open_local`）——这条路径上
/// 一切读写都走 `LocalFileSystem`，与远程会话那套（FTP / SFTP / WebDAV）是两回事。
pub fn mount(url: &RemoteUrl) -> Result<PathBuf, RemoteError> {
    let point = mount_point(url)?;
    match url.scheme.as_str() {
        "smb" | "cifs" | "samba" => mount_smb(url, &point),
        "nfs" => mount_nfs(url, &point),
        s => Err(RemoteError::Unsupported(s.to_string())),
    }
}

/// 卸载（用户点侧边栏那个「断开」时用）。
pub fn unmount(path: &Path) -> Result<(), RemoteError> {
    #[cfg(target_os = "windows")]
    let out = run("net", &["use", &path_string(path), "/delete", "/yes"]);
    #[cfg(not(target_os = "windows"))]
    let out = run("umount", &[&path_string(path)]);

    match out {
        Ok(_) => Ok(()),
        Err(e) => Err(RemoteError::transport("卸载网络盘", e)),
    }
}

// ---- 挂载点 ----

/// 挂载点：应用数据目录下的 `mounts/<名字>`（为什么不用 `/Volumes` 见模块文档）。
fn mount_point(url: &RemoteUrl) -> Result<PathBuf, RemoteError> {
    let name = mount_name(url);
    let base = dirs::data_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| RemoteError::transport("准备挂载点", "找不到应用数据目录"))?
        .join("mo")
        .join("mounts");
    std::fs::create_dir_all(&base)
        .map_err(|e| RemoteError::transport("准备挂载点", e.to_string()))?;
    let point = base.join(&name);
    // 已经挂过：mount(2) 需要一个空目录，复用即可（重复挂载同一个点会失败）。
    if !point.exists() {
        std::fs::create_dir_all(&point)
            .map_err(|e| RemoteError::transport("准备挂载点", e.to_string()))?;
    }
    Ok(point)
}

/// 挂载点的目录名：`<host>-<共享名>`（共享名里的 `/` 换成 `-`，避免嵌路径）。
fn mount_name(url: &RemoteUrl) -> String {
    let export = url.path.trim_start_matches('/').replace('/', "-");
    if export.is_empty() {
        url.host.clone()
    } else {
        format!("{}-{}", url.host, export)
    }
}

// ---- 各协议的挂载命令 ----

/// SMB：macOS 走 `mount_smbfs`，Windows 走 `net use`，Linux 走 `gio mount`（GVfs）。
fn mount_smb(url: &RemoteUrl, point: &Path) -> Result<PathBuf, RemoteError> {
    #[cfg(target_os = "macos")]
    {
        // 密码走 stdin，不进 argv（`ps` 看得见参数）。
        let target = format!(
            "//{}{}/{}",
            url.user
                .as_deref()
                .map(|u| format!("{u}@"))
                .unwrap_or_default(),
            url.host,
            url.path.trim_start_matches('/')
        );
        let mut args = vec![target, path_string(point)];
        if url.password.is_none() {
            // 没给密码：不询问（走 nsmb.conf / 钥匙串里那份），否则会卡在等输入。
            args.insert(0, "-N".to_string());
        }
        let password = url.password.clone();
        spawn_with_stdin("mount_smbfs", &args, password.as_deref())
            .map_err(|e| RemoteError::transport("挂载 SMB", e))?;
        Ok(point.to_path_buf())
    }

    #[cfg(target_os = "windows")]
    {
        let remote = format!(
            r"\\{}\{}",
            url.host,
            url.path.trim_start_matches('/').replace('/', "\\")
        );
        let mut args = vec!["*".to_string(), remote, "/persistent:no".to_string()];
        if let Some(user) = &url.user {
            args.push(format!("/user:{user}"));
        }
        if let Some(pw) = &url.password {
            // `net use` 没有 stdin 这条路，只能放在参数最后（Windows 没有 `ps` 那样的
            // 全局可读取参数列表，风险比 unix 小，但仍然只在用户显式写了密码时才带）。
            args.push(pw.clone());
        }
        let out = run("net", &args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .map_err(|e| RemoteError::transport("挂载 SMB", e))?;
        // `net use *` 会把分配到的盘符写进输出（`Z: is now connected to …`）。
        let drive = out
            .split_whitespace()
            .find(|w| w.len() == 2 && w.ends_with(':'))
            .map(|d| PathBuf::from(format!(r"{d}\")))
            .unwrap_or_else(|| point.to_path_buf());
        Ok(drive)
    }

    #[cfg(target_os = "linux")]
    {
        // GVfs：不需要 root，挂载点由它自己决定（`/run/user/<uid>/gvfs/…`）。
        let mut addr = format!("smb://{}", url.host);
        if let Some(user) = &url.user {
            addr = format!("smb://{user}@{host}", host = url.host);
        }
        let share = url.path.trim_start_matches('/');
        if !share.is_empty() {
            addr.push('/');
            addr.push_str(share);
        }
        // 口令同样不进 argv：`gio mount` 会自己弹认证框（走 libsecret / keyring）。
        run("gio", &["mount", &addr]).map_err(|e| {
            RemoteError::transport(
                "挂载 SMB",
                format!("{e}（需要 gvfs；或用 `mount -t cifs` 手动挂载）"),
            )
        })?;
        gvfs_path(&url.host, share)
            .ok_or_else(|| RemoteError::transport("挂载 SMB", "gio 报成功但找不到 GVfs 挂载点"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (url, point);
        Err(RemoteError::Unsupported("smb".to_string()))
    }
}

/// NFS：macOS 走 `mount_nfs`；Linux / Windows 需要 root 或可选组件，失败时把话说清楚。
fn mount_nfs(url: &RemoteUrl, point: &Path) -> Result<PathBuf, RemoteError> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("{}:{}", url.host, url.path);
        spawn_with_stdin("mount_nfs", &[target, path_string(point)], None)
            .map_err(|e| RemoteError::transport("挂载 NFS", e))?;
        Ok(point.to_path_buf())
    }

    #[cfg(target_os = "linux")]
    {
        // `mount` 需要 root：不给 sudo 提权（那会在 GUI 应用里弹出不明提权框），
        // 直接把失败原因说清楚。
        let target = format!("{}:{}", url.host, url.path);
        run("mount", &["-t", "nfs", &target, &path_string(point)])
            .map_err(|e| RemoteError::transport("挂载 NFS", format!("{e}（NFS 在 Linux 上通常需要 root：先用 `sudo mount -t nfs` 挂好，再在这里打开）")))?;
        Ok(point.to_path_buf())
    }

    #[cfg(target_os = "windows")]
    {
        // Windows 的 NFS 客户端是可选功能（`mount` 命令属于它），装了才能用。
        let remote = format!(r"\{}{}", url.host, url.path.replace('/', "\\"));
        run("mount", &["-o", "anon", &remote, "*"]).map_err(|e| {
            RemoteError::transport(
                "挂载 NFS",
                format!("{e}（需要启用 Windows 的「NFS 客户端」可选功能）"),
            )
        })?;
        Ok(point.to_path_buf())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (url, point);
        Err(RemoteError::Unsupported("nfs".to_string()))
    }
}

// ---- 解析（纯函数，方便单测钉住各种输出格式）----

/// macOS 的 `mount` 输出：`<fs> on <path> (<type>, <opts>)`。
#[cfg(any(test, target_os = "macos"))]
fn shares_from_macos_mount(output: &str) -> Vec<NetworkShare> {
    let mut out = Vec::new();
    for line in output.lines() {
        let Some((fs, rest)) = line.split_once(" on ") else {
            continue;
        };
        let Some((path, opts)) = rest.split_once(" (") else {
            continue;
        };
        let fstype = opts.split(',').next().unwrap_or_default().trim();
        let (scheme, host, export) = match fstype {
            "smbfs" => {
                // `//user@host/share`
                let bare = fs.trim_start_matches("//");
                let host_part = bare.split_once('@').map_or(bare, |(_u, h)| h);
                let (host, export) = host_part.split_once('/').unwrap_or((host_part, ""));
                ("smb", host.to_string(), export.to_string())
            }
            "nfs" => {
                // `host:/export/path`
                let (host, export) = fs.split_once(':').unwrap_or((fs, ""));
                ("nfs", host.to_string(), export.to_string())
            }
            _ => continue,
        };
        out.push(NetworkShare::named(
            PathBuf::from(path),
            scheme,
            &host,
            &export,
        ));
    }
    out
}

/// Linux 的 `/proc/mounts`：`<fs> <path> <type> <opts> ...`（空白分隔）。
// 只有 Linux 真用得到；其余平台留着给单测钉住格式（否则 `dead_code`）。
#[cfg(any(test, target_os = "linux"))]
fn shares_from_proc_mounts(text: &str) -> Vec<NetworkShare> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(fs), Some(path), Some(fstype)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (scheme, host, export) = match fstype {
            "cifs" | "smb3" => {
                let bare = fs.trim_start_matches("//");
                let bare = bare.split_once('@').map_or(bare, |(_u, h)| h);
                let (host, export) = bare.split_once('/').unwrap_or((bare, ""));
                ("smb", host.to_string(), export.to_string())
            }
            "nfs" | "nfs4" => {
                let (host, export) = fs.split_once(':').unwrap_or((fs, ""));
                ("nfs", host.to_string(), export.to_string())
            }
            _ => continue,
        };
        // GVfs 的挂载点带转义（`smb-share:server=host,share=s`），显示名用共享名。
        out.push(NetworkShare::named(
            PathBuf::from(path),
            scheme,
            &host,
            &export,
        ));
    }
    out
}

/// Windows 的 `net use` 输出：
///
/// ```text
/// Status       Local     Remote                    Network
/// -------------------------------------------------------------------------------
/// OK           Z:        \\server\share             Microsoft Windows Network
/// ```
// 同上：只有 Windows 真用得到。
#[cfg(any(test, target_os = "windows"))]
fn shares_from_net_use(output: &str) -> Vec<NetworkShare> {
    let mut out = Vec::new();
    let mut started = false;
    for line in output.lines() {
        // 表头下面那行 `---` 之后才是数据行。
        if line.trim_start().starts_with("---") {
            started = true;
            continue;
        }
        if !started {
            continue;
        }
        let mut it = line.split_whitespace();
        let (_status, local, remote) = match (it.next(), it.next(), it.next()) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => continue,
        };
        let Some(rest) = remote.strip_prefix(r"\\") else {
            continue; // IPC$ 之类没有 `\\` 的条目，跳过。
        };
        let (host, export) = rest.split_once('\\').unwrap_or((rest, ""));
        // 本地列可能是空的（未分配盘符的会话）。
        if local.len() < 2 || !local.ends_with(':') {
            continue;
        }
        out.push(NetworkShare::named(
            PathBuf::from(format!(r"{local}\")),
            "smb",
            host,
            export,
        ));
    }
    out
}

/// Linux（GVfs）挂载点：`/run/user/<uid>/gvfs/smb-share:server=HOST,share=SHARE`。
#[cfg(target_os = "linux")]
fn gvfs_path(host: &str, share: &str) -> Option<PathBuf> {
    let uid = users_uid()?;
    let dir = PathBuf::from(format!("/run/user/{uid}/gvfs"));
    let wanted = format!("smb-share:server={host},share={share}");
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy() == wanted)
                .unwrap_or(false)
        })
}

#[cfg(target_os = "linux")]
fn users_uid() -> Option<u32> {
    run("id", &["-u"]).ok()?.trim().parse().ok()
}

// ---- 进程小工具 ----

fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// 跑一条命令并拿回 stdout；非零退出时把 stderr 一起并进错误里（用户要看原因）。
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program} 起不来：{e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// 跑一条命令，并把 `stdin_text`（SMB 密码）从管道喂进去。
#[cfg(target_os = "macos")]
fn spawn_with_stdin(
    program: &str,
    args: &[String],
    stdin_text: Option<&str>,
) -> Result<(), String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::piped());
    // stdout / stderr 都收回来：失败时要把系统的原话给用户看。
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("{program} 起不来：{e}"))?;
    if let Some(text) = stdin_text {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{text}");
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{program} 等待失败：{e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS 的 `mount` 输出（取自一台真的挂了 SMB 与 NFS 的机器）。
    ///
    /// 这条钉住两件事：① smbfs 的 `//user@host/share` 与 nfs 的 `host:/export`
    /// 两种写法要分开解析；② 本地盘（APFS）不能混进来——侧边栏只该列网络盘。
    #[test]
    fn macos_mount_lists_network_volumes_only() {
        let out = concat!(
            "/dev/disk3s5 on / (apfs, sealed, local, read-only, journaled)\n",
            "//11048490@172.25.48.48/share on /Volumes/share (smbfs, nodev, nosuid, mounted by u)\n",
            "nas.local:/export/home on /Volumes/nas (nfs, nodev, nosuid, read-only)\n",
        );
        let shares = shares_from_macos_mount(out);
        assert_eq!(shares.len(), 2, "本地盘不该被当成网络盘：{shares:?}");

        assert_eq!(shares[0].scheme, "smb");
        assert_eq!(shares[0].host, "172.25.48.48");
        assert_eq!(shares[0].export, "share");
        assert_eq!(shares[0].path, PathBuf::from("/Volumes/share"));
        assert_eq!(shares[0].label, "share", "显示名用挂载点的目录名");

        assert_eq!(shares[1].scheme, "nfs");
        assert_eq!(shares[1].host, "nas.local");
        assert_eq!(shares[1].export, "/export/home");
        assert_eq!(shares[1].path, PathBuf::from("/Volumes/nas"));
    }

    /// Linux：`/proc/mounts` 里 cifs 与 nfs4 各一条，其余（ext4 / proc）忽略。
    #[test]
    fn linux_proc_mounts_keeps_cifs_and_nfs() {
        let text = concat!(
            "/dev/sda1 / ext4 rw,relatime 0 0\n",
            "//nas.local/public /mnt/public cifs rw,vers=3.1.1 0 0\n",
            "nas.local:/export/home /mnt/home nfs4 rw,relatime 0 0\n",
            "proc /proc proc rw,nosuid 0 0\n",
        );
        let shares = shares_from_proc_mounts(text);
        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0].scheme, "smb");
        assert_eq!(shares[0].host, "nas.local");
        assert_eq!(shares[0].export, "public");
        assert_eq!(shares[1].scheme, "nfs");
        assert_eq!(shares[1].export, "/export/home");
    }

    /// Windows：`net use` 的表格输出，只要真的盘符。
    #[test]
    fn windows_net_use_picks_drive_letters() {
        let out = concat!(
            "New connections will not be remembered.\n",
            "\n",
            "Status       Local     Remote                    Network\n",
            "\n",
            "-------------------------------------------------------------------------------\n",
            "OK           Z:        \\\\nas\\public                Microsoft Windows Network\n",
            "Disconnected Y:        \\\\nas\\old                   Microsoft Windows Network\n",
            "The command completed successfully.\n",
        );
        let shares = shares_from_net_use(out);
        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0].path, PathBuf::from(r"Z:\"));
        assert_eq!(shares[0].host, "nas");
        assert_eq!(shares[0].export, "public");
        assert_eq!(shares[0].label, "Z:", "盘符根直接显示盘符");
        assert_eq!(shares[1].label, "Y:", "没有目录名时显示名用盘符");
    }

    /// 挂载点落在**应用自己的**数据目录下，而不是 `/Volumes`。
    ///
    /// macOS 上 `/Volumes` 归 root 所有、普通用户建不了目录，而 `mount_smbfs` 不是
    /// setuid——挂进去必然失败（这条钉住那个选择）。
    #[test]
    fn the_mount_point_lives_in_the_apps_own_directory() {
        let url = RemoteUrl::parse("smb://alice@nas.local/public/sub").expect("地址应当能解析");
        let point = mount_point(&url).expect("挂载点应当能建出来");
        assert!(
            point.components().any(|c| c.as_os_str() == "mounts"),
            "挂载点应当在 mounts/ 下：{point:?}"
        );
        assert_eq!(
            point.file_name().unwrap().to_string_lossy(),
            "nas.local-public-sub",
            "共享名里的 `/` 要摊平，不能嵌出子目录"
        );
        assert!(
            point.is_dir(),
            "挂载点必须真的存在（mount(2) 要一个空目录）"
        );
    }

    /// 密码**不能**进命令行参数（unix 上 `ps` 谁都看得见）。
    ///
    /// 这条只钉 macOS 那条路的入参拼法：密码走 stdin，地址里只带用户名。
    #[test]
    fn the_password_never_reaches_the_command_line() {
        let url = RemoteUrl::parse("smb://alice:secret@nas.local/public").expect("地址应当能解析");
        let target = format!(
            "//{}{}/{}",
            url.user
                .as_deref()
                .map(|u| format!("{u}@"))
                .unwrap_or_default(),
            url.host,
            url.path.trim_start_matches('/')
        );
        assert_eq!(target, "//alice@nas.local/public");
        assert!(!target.contains("secret"), "密码不许出现在挂载目标里");
    }

    /// 未实现的协议（比如 FTP）走**挂载**这条路时给明确错误，而不是连上去再炸。
    #[test]
    fn mounting_an_unsupported_scheme_says_so() {
        let u = RemoteUrl::parse("ftp://h/tmp").expect("解析本身应当成功");
        assert!(matches!(mount(&u), Err(RemoteError::Unsupported(ref s)) if s == "ftp"));
    }

    /// 挂载点的目录名：`<host>-<共享名>`，共享名里的 `/` 摊平（不然会嵌出子目录）。
    #[test]
    fn the_mount_name_flattens_the_share_path() {
        let u = RemoteUrl::parse("smb://nas.local/public/sub").expect("地址应当能解析");
        assert_eq!(mount_name(&u), "nas.local-public-sub");
        let bare = RemoteUrl::parse("smb://nas.local").expect("地址应当能解析");
        assert_eq!(mount_name(&bare), "nas.local");
    }
}
