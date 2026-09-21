//! mo-remote：远程文件系统的**连接层**。
//!
//! 提供的东西：
//!
//! * [`RemoteUrl`] —— 一份可解析 / 可回显的远程地址（`scheme://user@host:port/path`），
//!   带单测钉住各种边界（缺路径、隐式默认端口、IPv6 宿主、密码里的 `@`）；
//! * [`supports`] —— 该由哪个后端接手；
//! * [`connect`] —— 建连接，返回一个实现了 [`mo_fs::FileSystem`] 的对象；
//! * [`RemoteError::Disconnected`] —— 把「连接断了」从「这一步没做成」里分出来，
//!   上层据此**重建连接再试一次**，而不是把网络错误原文弹给用户；
//! * [`is_disconnected`] / [`text_looks_disconnected`] —— 上面那个分类的判据。
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
    /// 连接已经断了：对端关闭、被服务器掐断（FTP 闲置超时很常见），或休眠 /
    /// 换网之后旧 socket 失效。
    ///
    /// 与 [`RemoteError::Transport`] 分开是有意的：上层拿到它**不该报错给用户**，
    /// 而该重建连接再试一次。混进 `Transport` 就只能把
    /// `Connection error: Broken pipe (os error 32)` 这种句子直接弹到用户脸上
    /// ——这正是用户报的那个「切回 FTP 弹报错」。
    #[error("{kind}失败：连接已断开（{detail}）")]
    Disconnected {
        /// 失败发生在哪一步（列目录 / 上传…）。
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

    /// 「连接已经断了」——上层据此重连重试，而不是报错给用户。
    pub fn disconnected(kind: &'static str, e: impl std::fmt::Display) -> Self {
        RemoteError::Disconnected {
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
        // 「连接断了」要用 `io::ErrorKind::NotConnected` 打个标记：跨到 mo-app 之后
        // 只剩 `MoError`，而那里要能一眼分出「重连再来一次」与「真的失败了」。
        // 判据是 [`is_disconnected`]。
        if matches!(e, RemoteError::Disconnected { .. }) {
            return MoError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                e.to_string(),
            ));
        }
        MoError::Io(std::io::Error::other(e.to_string()))
    }
}

/// 探活一次往返最多等多久。
///
/// 必须有这个上限：对端被网络静默丢弃（NAT 回收、机器休眠、拔网线）时，写会
/// 成功、读会一直等，没有超时就把调用线程（mo-app 的 blocking 池）拖住几分钟。
/// 超时一律按「断了」处理，交给上层重建连接。
pub(crate) const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 这个错误是不是「连接已经断了」（见 [`RemoteError::Disconnected`]）。
///
/// 判据放在本 crate 而不管在调用方，是因为**产生**这个标记的知识在这里
/// （哪几种 `io::ErrorKind` / 哪些错误文本算断线）。
pub fn is_disconnected(e: &MoError) -> bool {
    matches!(e, MoError::Io(io) if io.kind() == std::io::ErrorKind::NotConnected)
}

/// `io::ErrorKind` 里哪些算「连接没了」而不是「这个操作本身不合法」。
///
/// FTP 侧用得上（`suppaftp::FtpError::ConnectionError` 里是货真价实的
/// `io::Error`）；SFTP 侧拿不到 kind，只能走 [`text_looks_disconnected`]。
pub(crate) fn io_kind_is_disconnect(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        kind,
        BrokenPipe | ConnectionReset | ConnectionAborted | NotConnected | UnexpectedEof | TimedOut
    )
}

/// 从错误文本判断「连接断了」。
///
/// **SFTP 侧只能这样**：`russh_sftp::Error::IO(String)` 把底层错误拍成了字符串
/// （那个枚举不带 `source`），拿不到 `io::ErrorKind`。`Error::Timeout` 也在列：
/// 那是「请求 10 秒没应答」，在闲置之后等价于连接已死（`client/mod.rs` 的
/// 每请求超时默认 10 秒），重连重试的代价远小于把 `Timeout` 弹给用户。
///
/// 写成有单测的函数而不是就地 `contains`，是为了让「哪些句子算断线」这件事
/// 有一个可审视、可回归的地方。
pub fn text_looks_disconnected(detail: &str) -> bool {
    const MARKS: [&str; 8] = [
        "broken pipe",
        "connection reset",
        "connection aborted",
        "not connected",
        "unexpected eof",
        "session closed",
        "channel closed",
        "timeout",
    ];
    let d = detail.to_ascii_lowercase();
    MARKS.iter().any(|m| d.contains(m))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 「连接断了」跨到 `MoError` 之后必须留下可判别的痕迹——mo-app 就靠它决定
    /// 「重连再来一次」还是「报错给用户」。混成一个字符串就分不出来了。
    #[test]
    fn disconnect_survives_the_trip_to_mo_error() {
        let m: MoError = RemoteError::disconnected("列目录", "Broken pipe (os error 32)").into();
        assert!(is_disconnected(&m), "断线标记必须能跨层判出来");
        assert!(
            m.to_string().contains("连接已断开"),
            "给用户看的话要说明白：{m}"
        );
    }

    /// 普通失败不能被当成断线——那会让「进一个不存在的目录」变成「重登一次」。
    #[test]
    fn plain_failures_are_not_disconnects() {
        let m: MoError = RemoteError::transport("列目录", "550 no such directory").into();
        assert!(!is_disconnected(&m));
    }

    /// 文本判据是 SFTP 侧唯一能用的（`russh_sftp::Error::IO(String)` 不带 source），
    /// 所以把「哪些句子算断线」逐条钉住。
    #[test]
    fn common_disconnect_phrases_are_recognised() {
        for s in [
            "Broken pipe (os error 32)",
            "Connection reset by peer",
            "Connection aborted",
            "Unexpected EOF",
            "session closed",
            "Timeout",
        ] {
            assert!(text_looks_disconnected(s), "「{s}」应当算断线");
        }
        assert!(!text_looks_disconnected("550 no such directory"));
        assert!(!text_looks_disconnected("Permission denied"));
    }
}
