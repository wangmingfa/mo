//! mo-remote：远程文件系统的**连接层**。
//!
//! 提供的东西：
//!
//! * [`RemoteUrl`] —— 一份可解析 / 可回显的远程地址（`scheme://user@host:port/path`），
//!   带单测钉住各种边界（缺路径、隐式默认端口、IPv6 宿主、密码里的 `@`）；
//! * [`supports`] —— 该由哪个后端接手；
//! * [`connect`] —— 建连接，返回一个实现了 [`mo_fs::FileSystem`] 的对象。
//!
//! ## 为什么 SMB / NFS 不在这里
//!
//! Rust 生态没有成熟可维护的 SMB / NFS **客户端**实现（SMB2/3 与 NFSv4 都是
//! 大协议）。而这三端操作系统本身都会把网络盘挂成目录：
//!
//! * macOS：`/Volumes/<share>`
//! * Linux：`/mnt` / `/run/user/<uid>/gvfs`（GVfs 挂载点）
//! * Windows：`\\server\share` 驱动器号
//!
//! 文件管理器里「挂载后当本地目录浏览」才是正确解法——它顺带免掉了凭据管理、
//! 断点续传、Kerberos 这一整套。因此 SMB / NFS 的计划是**发现 + 触发系统挂载**，
//! 而不是在进程里实现协议。见 `devlog/` 里 Roadmap 第四阶段的记录。
//!
//! ## FTP 为什么是第一个
//!
//! FTP 的每个动作都能一对一映射到 [`mo_fs::FileSystem`] 的方法（列目录 / 建目录 /
//! 上传 / 下载 / 改名 / 删除），而且 `suppaftp` 是纯 Rust、无本机依赖，
//! 三平台都能编。它作为「这条路走得通」的验证：SFTP 后端已按同样的形状接好
//! （见 [`sftp`]），WebDAV / 云存储待议。
//!
//! ## ⚠️ 每个连接自带 runtime
//!
//! [`ftp::FtpFileSystem`] 内部持有一个**独占的 tokio runtime**。
//!
//! 原因：`suppaftp::AsyncFtpStream` 里的 TCP 连接在建socket 时注册到「当时的
//! reactor」，后续所有读写都必须由同一个 runtime 驱动。若让它在 mo-app 的共享
//! runtime 上建立、又在别的池线程上被 poll，就会出现难以复现的卡顿与超时。
//! 独占一份 runtime + 统一 [`tokio::runtime::Runtime::block_on`] 是最省心的解，
//! 代价是每个远程连接占一个线程——和它在 `spawn_blocking` 池里被使用的方式
//! 刚好一致（见 `mo_fs::FileSystem::read_dir_blocking`）。

pub mod ftp;
pub mod sftp;
mod url;

pub use self::url::{RemoteUrl, DEFAULT_PORTS};

use mo_core::MoError;
use std::sync::Arc;

/// 远程连接 / 协议错误。
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// 地址本身写得不对（缺主机名、路径不是绝对路径…）。
    #[error("远程地址无法解析：{0}")]
    BadUrl(String),
    /// 认得 scheme，但还没实现对应后端。
    #[error("暂不支持的远程协议：{0}")]
    Unsupported(String),
    /// 连接 / 认证 / 传输失败。底层原因保留原文，便于排障。
    #[error("{kind} 失败：{detail}")]
    Transport {
        /// 失败发生在哪一步（连接 / 登录 / 列目录…）。
        kind: &'static str,
        /// 底层错误文本。
        detail: String,
    },
    /// 服务器拒绝这组凭据：匿名登录不被接受，或用户名 / 密码不对。
    ///
    /// 与 [`RemoteError::Transport`] 分开是有意的——UI 拿到它要弹「输入账号密码」
    /// 的框，而「连不上 / 超时 / DNS 失败」要显示成地址错误。混成一句字符串就
    /// 没法分流了（这正是改造前的问题：一切都压成 `Transport`）。
    #[error("{kind}被服务器拒绝：{detail}")]
    AuthRequired {
        /// 失败发生在哪一步（登录 / 认证）。
        kind: &'static str,
        /// 底层错误文本。
        detail: String,
    },
}

impl RemoteError {
    /// 统一的「这一步失败了」构造器：底层错误文本进 `detail`。
    pub fn transport(kind: &'static str, e: impl std::fmt::Display) -> Self {
        RemoteError::Transport {
            kind,
            detail: e.to_string(),
        }
    }

    /// 「服务器不接受这组凭据」——UI 据此弹认证框。
    pub fn auth(kind: &'static str, e: impl std::fmt::Display) -> Self {
        RemoteError::AuthRequired {
            kind,
            detail: e.to_string(),
        }
    }
}

impl From<RemoteError> for MoError {
    fn from(e: RemoteError) -> Self {
        MoError::Io(std::io::Error::other(e.to_string()))
    }
}

/// 是否已有可用于该协议的后端。
pub fn supports(scheme: &str) -> bool {
    matches!(scheme, "ftp" | "sftp")
}

/// 按给定地址建一条远程连接。
///
/// 返回的对象实现了 [`mo_fs::FileSystem`]，调用方只依赖那个 trait——
/// 「路径在本文中必须是本地路径」这一假设由此打破的第一步。
pub fn connect(url: &RemoteUrl) -> Result<Arc<dyn mo_fs::FileSystem>, RemoteError> {
    match url.scheme.as_str() {
        "ftp" => Ok(Arc::new(ftp::FtpFileSystem::connect(url)?)),
        "sftp" => Ok(Arc::new(sftp::SftpFileSystem::connect(url)?)),
        s => Err(RemoteError::Unsupported(s.to_string())),
    }
}
