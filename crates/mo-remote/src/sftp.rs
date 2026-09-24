//! SFTP 后端：把 `russh` 的 `SftpSession` 包装成 [`mo_fs::FileSystem`]。
//!
//! ## 路径约定
//!
//! 与 FTP 后端一致：传给本实现的路径一律是**远程绝对路径**（`/home/user`），
//! 主机与凭据由本对象自己持有，上层只记「正在浏览哪个连接」。
//!
//! ## 为什么自带一份 runtime
//!
//! `russh` 的 SFTP 会话内部靠一条 SSH 通道与远端往返，所有请求都要由那条
//! 通道所在的 runtime 驱动。为避免「在 mo-app 的共享 runtime 上建连接、又在
//! blocking 池里 poll」导致的跨 runtime 卡顿（和 FTP 后端同样的坑），每个连接
//! 独占一份多线 runtime（1 个 worker 常驻），连接与每一次 SFTP 调用都在它之上跑。
//!
//! - `read_dir_blocking`：直接在 `rt.block_on` 上驱动（mo-app 列目录走 blocking 池，
//!   本就不该碰异步 worker）。
//! - 其余 `async` 方法：经 `rt.handle().spawn(...)` 把任务派到常驻 worker 上跑，
//!   外层只 `await` 结果——既正确又不会阻塞 mo-app 的 runtime。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use mo_core::{EntryKind, FileId, FileMetadata, MoError, Permissions};
use mo_fs::{FileSystem, ReadDirEntry};
use russh::client::{connect, AuthResult, Config, Handler};
use russh::keys::PublicKeyOrCertificate;
use russh_sftp::client::fs::DirEntry;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::FileType;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

use crate::{RemoteError, RemoteUrl};

/// russh 需要的 Handler：这里只接管「是否信任服务器公钥」。
///
/// 文件管理器场景没有已知主机的信任库，默认**接受所有主机密钥**
/// （和 `ssh` 首次连接行为一致）。未来要接 `known_hosts` 再收紧。
struct ClientHandler;

impl Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// 一个 SFTP 连接。
pub struct SftpFileSystem {
    url: RemoteUrl,
    /// 仅供本连接使用的 runtime（见模块文档）。
    rt: Arc<Runtime>,
    /// SFTP 会话：`&mut` 才能发请求，而 trait 只给 `&self`，用锁包一层。
    sftp: Arc<Mutex<SftpSession>>,
}

impl SftpFileSystem {
    /// 建 SSH 连接、做密码认证、拉起 sftp 子系统。
    ///
    /// 用户名缺省按 `anonymous`、密码缺省为空串——多数 SFTP 服务器要求显式凭据，
    /// 这里不强行给默认值，让服务器在认证阶段明确报错而不是连上后干瞪眼。
    pub fn connect(url: &RemoteUrl) -> Result<Self, RemoteError> {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .map_err(|e| transport_error("创建 runtime", e))?,
        );

        let port = url.port_or_default().unwrap_or(22);
        // IPv6 字面量必须加方括号，否则 `ToSocketAddrs` 会把 `::` 当成端口分隔符。
        let addr = if url.host.contains(':') && !url.host.starts_with('[') {
            format!("[{}]:{port}", url.host)
        } else {
            format!("{}:{port}", url.host)
        };
        let user = url.user.clone().unwrap_or_else(|| "anonymous".to_string());
        let password = url.password.clone().unwrap_or_default();

        let sftp = rt.block_on(async {
            let config = Arc::new(Config::default());
            let mut client = connect(config, addr, ClientHandler)
                .await
                .map_err(|e| transport_error("连接", e))?;
            let auth = client
                .authenticate_password(&user, &password)
                .await
                .map_err(|e| transport_error("认证", e))?;
            // `AuthResult::Failure` 就是「凭据不对」——报成 `AuthRequired` 而不是
            // `Transport`，UI 才会弹「输入账号密码」的框而不是干显示一句错误。
            // （`Err(..)` 那条是传输层故障，仍走 `transport`。）
            if !matches!(auth, AuthResult::Success) {
                return Err(RemoteError::auth("认证", "服务器拒绝提供的凭据"));
            }
            let channel = client
                .channel_open_session()
                .await
                .map_err(|e| transport_error("打开通道", e))?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(|e| transport_error("启动 sftp 子系统", e))?;
            let stream = channel.into_stream();
            SftpSession::new(stream)
                .await
                .map_err(|e| transport_error("初始化 sftp", e))
        })?;

        Ok(Self {
            url: url.clone(),
            rt,
            sftp: Arc::new(Mutex::new(sftp)),
        })
    }

    /// 连接对应的地址（回显时不带密码）。
    pub fn url(&self) -> &RemoteUrl {
        &self.url
    }

    /// 探活：`stat(".")` 一次往返。
    ///
    /// 选 `stat` 而不是别的：每个 SFTP 服务器都必须实现它、一次往返、不改任何
    /// 状态。**带超时**（见 [`crate::PROBE_TIMEOUT`]）：SSH 通道已经死了但没有
    /// FIN 时请求会一直等（`russh-sftp` 自己的每请求超时是 10 秒，比这里长），
    /// 超时按「断了」处理，交给上层重建连接。
    pub fn is_alive(&self) -> bool {
        self.rt.block_on(async {
            let probe = tokio::time::timeout(crate::PROBE_TIMEOUT, async {
                let s = self.sftp.lock().await;
                s.metadata(".".to_string()).await
            })
            .await;
            matches!(probe, Ok(Ok(_)))
        })
    }

    /// 把 trait 里的路径统一成远程绝对路径字符串。
    fn remote(path: &Path) -> String {
        let s = path.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            s
        } else {
            format!("/{s}")
        }
    }

    /// 在连接自有的 runtime 上驱动一个 future（供 `async` trait 方法使用，
    /// 避免跨 runtime 驱动 russh 的会话任务）。
    async fn run<F, T>(&self, f: F) -> T
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.rt.handle().spawn(f).await.expect("sftp 后台任务失败")
    }
}

/// 把 SFTP 侧的错误分成「连接断了」与「这一步没做成」。
///
/// 只能按**文本**判：`russh_sftp::Error::IO(String)` 把底层错误拍成了字符串
/// （那个枚举不带 `source`），`russh::Error` 虽然带 `io::Error` 但连接期 / 会话期
/// 混在一起、类型也杂；统一走 [`crate::text_looks_disconnected`] 更省心，判据本身
/// 在那边有单测钉着（含 `Error::Timeout` —— 闲置之后一次请求超时等价于断线）。
fn transport_error(kind: &'static str, e: impl std::fmt::Display) -> RemoteError {
    if crate::text_looks_disconnected(&e.to_string()) {
        RemoteError::disconnected(kind, e)
    } else {
        RemoteError::transport(kind, e)
    }
}

/// 把 `ReadDir` 的迭代结果映射成上层条目（已自动过滤 `.` / `..`）。
fn collect_entries(rd: impl Iterator<Item = DirEntry>) -> Vec<ReadDirEntry> {
    rd.map(|de| {
        let name = de.file_name();
        let kind = match de.file_type() {
            FileType::Dir => EntryKind::Directory,
            FileType::Symlink => EntryKind::Symlink,
            FileType::File | FileType::Other => EntryKind::File,
        };
        let path = PathBuf::from(de.path());
        ReadDirEntry::new(FileId::synthetic(&path), name, kind, path)
    })
    .collect()
}

#[async_trait]
impl FileSystem for SftpFileSystem {
    fn is_alive(&self) -> bool {
        SftpFileSystem::is_alive(self)
    }

    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        let remote = Self::remote(path);
        let sftp = self.sftp.clone();
        let rd = self
            .run(async move {
                let sftp = sftp.lock().await;
                sftp.read_dir(remote).await
            })
            .await
            .map_err(|e| MoError::from(transport_error("列目录", e)))?;
        Ok(collect_entries(rd))
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        let remote = Self::remote(path);
        let sftp = self.sftp.clone();
        let rd = self
            .rt
            .block_on(async move {
                let sftp = sftp.lock().await;
                sftp.read_dir(remote).await
            })
            .map_err(|e| MoError::from(transport_error("列目录", e)))?;
        Ok(collect_entries(rd))
    }

    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError> {
        let remote = Self::remote(path);
        let sftp = self.sftp.clone();
        let meta = self
            .run(async move {
                let s = sftp.lock().await;
                s.metadata(remote).await
            })
            .await
            .map_err(|e| MoError::from(transport_error("读取元数据", e)))?;
        Ok(FileMetadata {
            size: meta.size.unwrap_or(0),
            modified: meta.mtime.map(system_time_from_unix),
            created: None,
            permissions: Permissions::default(),
        })
    }

    async fn create_dir(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let sftp = self.sftp.clone();
        self.run(async move {
            let s = sftp.lock().await;
            s.create_dir(remote).await
        })
        .await
        .map_err(|e| MoError::from(transport_error("建目录", e)))
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, MoError> {
        let remote = Self::remote(path);
        let sftp = self.sftp.clone();
        self.run(async move {
            let s = sftp.lock().await;
            s.read(remote).await
        })
        .await
        .map_err(|e| MoError::from(transport_error("下载", e)))
    }

    async fn is_dir(&self, path: &Path) -> bool {
        // SFTP 的 attrs 自带类型位（不用启发式猜），一次 metadata 就能定。
        let remote = Self::remote(path);
        let sftp = self.sftp.clone();
        self.run(async move {
            let s = sftp.lock().await;
            s.metadata(remote)
                .await
                .map(|a| a.is_dir())
                .unwrap_or(false)
        })
        .await
    }

    async fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let contents = contents.to_vec();
        let sftp = self.sftp.clone();
        self.run(async move {
            let s = sftp.lock().await;
            // 「目标已存在必须失败」由调用方保证（见 trait 文档）；SFTP 的 write 会覆盖，
            // 这里先探一次存在性：存在就拒绝，绝不静默覆盖远端数据。
            if s.try_exists(remote.clone())
                .await
                .map_err(|e| MoError::from(transport_error("检查存在", e)))?
            {
                return Err(MoError::Other(format!("远端已存在 {remote}——拒绝覆盖")));
            }
            s.write(remote, &contents)
                .await
                .map_err(|e| MoError::from(transport_error("上传", e)))
        })
        .await
    }

    async fn remove_file(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let sftp = self.sftp.clone();
        self.run(async move {
            let s = sftp.lock().await;
            s.remove_file(remote).await
        })
        .await
        .map_err(|e| MoError::from(transport_error("删除文件", e)))
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let sftp = self.sftp.clone();
        self.run(async move {
            let s = sftp.lock().await;
            s.remove_dir(remote).await
        })
        .await
        .map_err(|e| MoError::from(transport_error("删除目录", e)))
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), MoError> {
        let from = Self::remote(from);
        let to = Self::remote(to);
        let sftp = self.sftp.clone();
        self.run(async move {
            let s = sftp.lock().await;
            s.rename(from, to).await
        })
        .await
        .map_err(|e| MoError::from(transport_error("重命名", e)))
    }
}

impl Drop for SftpFileSystem {
    fn drop(&mut self) {
        // 尽量优雅地关掉 sftp 通道：失败无所谓（连接可能早就断了），但不能在 drop 里 panic。
        if let Ok(sftp) = self.sftp.try_lock() {
            let _ = self.rt.block_on(sftp.close());
        }
    }
}

/// SFTP 的 `mtime` 是「自 Unix 纪元起的秒数」（u32），换算成 `SystemTime`。
fn system_time_from_unix(secs: u32) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_normalized_to_remote_absolute() {
        assert_eq!(SftpFileSystem::remote(Path::new("/home")), "/home");
        assert_eq!(SftpFileSystem::remote(Path::new("home/x")), "/home/x");
        assert_eq!(
            SftpFileSystem::remote(Path::new("home\\x")),
            "/home/x",
            "Windows 分隔符也要归一"
        );
    }

    #[test]
    fn unsupported_scheme_is_reported_not_panicked() {
        // 用尚未实现的 smb 验证「未实现协议给明确错误」，而不是连上去再炸。
        let u = RemoteUrl::parse("smb://h/tmp").expect("解析本身应当成功");
        assert!(!crate::supports(&u.scheme));
        let got = crate::connect(&u);
        assert!(
            matches!(got, Err(RemoteError::Unsupported(ref scheme)) if scheme == "smb"),
            "未实现的协议要给出明确错误，而不是连上去再炸"
        );
    }
}
