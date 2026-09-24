//! FTP 后端：把 [`AsyncFtpStream`] 包装成 [`mo_fs::FileSystem`]。
//!
//! ## 路径约定
//!
//! 传给本实现的路径一律是**远程绝对路径**（`/pub/incoming`），不是本地路径、
//! 也不是完整的 URL——主机与凭据由本对象自己持有。这样上层只要记住
//! 「正在浏览哪个连接」即可，不必每次都把 `ftp://user@host:port` 拼进路径。
//!
//! ## 为什么自己带一份 runtime
//!
//! TCP 连接在建 socket 时就注册到了当时的 reactor，之后所有读写都得由同一个
//! runtime 驱动。mo-app 的共享 runtime 与 blocking 池是两个不同的池，若在这里
//! 建连接、在那里 poll，症状是「偶尔卡住 / 偶尷超时」这种最难查的问题。
//! 因此每个连接独占一份 **1 worker 的多线 runtime**（与 SFTP 后端同一套约定），
//! 分两种用法：
//!
//! - 阻塞入口（`read_dir_blocking` / 探活 / 连接）：`rt.block_on`，调用方本就在
//!   blocking 池里，不该碰异步 worker；
//! - `async` trait 方法：经 [`FtpFileSystem::run`]（即 `rt.handle().spawn`）把任务
//!   派回**这条连接自己的** worker，外层只 `await` 结果。
//!
//! ⚠️ 这里原来是 current-thread runtime + 直接在 `async fn` 里 `await`：那种写法把
//! 这条连接的 socket 交给了**调用方**的 runtime 去 poll，正是上面说的跨 runtime
//! 驱动（SFTP 早就用 `spawn` 躲开了，FTP 漏了这层）。多线 runtime 才能让
//! `handle().spawn` 的任务真的被 worker 驱动起来——current-thread 的 runtime 闲置时
//! 没人 poll 它，spawn 出去的任务会永远挂着。
//!
//! 代价是一个连接占一个线程，这与它在 `spawn_blocking` 池里被使用的方式一致。

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mo_core::{EntryKind, FileId, FileMetadata, MoError, Permissions};
use mo_fs::{FileSystem, ReadDirEntry};
use suppaftp::list::{File, ListParser};
use suppaftp::tokio::AsyncFtpStream;
use suppaftp::FtpResult;
use tokio::sync::Mutex;

use crate::{RemoteError, RemoteUrl};

/// 一个 FTP 连接。
pub struct FtpFileSystem {
    url: RemoteUrl,
    /// 仅供本连接使用的 runtime（见模块文档）。
    ///
    /// 用 `Arc` 是为了让 `async` 方法能把任务派回这里：[`Self::run`] 要求 future 是
    /// `'static`，得把句柄搬进去。
    rt: Arc<tokio::runtime::Runtime>,
    /// FTP 控制连接：所有命令都要 `&mut`，而 trait 只给 `&self`。
    ///
    /// `Arc` 同理——`run` 里的 future 要自己拿一份锁的句柄。
    conn: Arc<Mutex<AsyncFtpStream>>,
}

impl FtpFileSystem {
    /// 这条连接独占的那份 runtime（见模块文档：多线、1 worker）。
    ///
    /// 抽成函数是为了让「它必须能自己驱动 spawn 出去的任务」这件事可被测试
    /// ——`async` 方法靠 `rt.handle().spawn` 把远程往返派回这里，而调用方
    /// （mo-app 的共享 runtime）不会去 block_on 它；runtime 若是 current-thread，
    /// 那些任务就永远没人 poll。见 `tests::the_connection_runtime_drives_its_own_tasks`。
    fn connection_runtime(url: &RemoteUrl) -> Result<tokio::runtime::Runtime, RemoteError> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            // 一个连接一个线程：线程名带上端点，profile 时一眼看出是哪条连接。
            .thread_name(format!("mo-ftp-{}", url.host))
            .enable_all()
            .build()
            .map_err(|e| RemoteError::transport("创建 runtime", e))
    }

    /// 建连接并登录。
    ///
    /// 用户名 / 密码缺省时按匿名登录（`anonymous`）——多数公共 FTP 都接受，
    /// 也让「地址里没写凭据」不至于直接失败。
    pub fn connect(url: &RemoteUrl) -> Result<Self, RemoteError> {
        let rt = Self::connection_runtime(url)?;

        let addr = format!("{}:{}", url.host, url.port_or_default().unwrap_or(21));
        let user = url.user.clone().unwrap_or_else(|| "anonymous".to_string());
        let password = url
            .password
            .clone()
            .unwrap_or_else(|| "anonymous@example.com".to_string());

        let conn = rt.block_on(async {
            let mut stream = AsyncFtpStream::connect(&addr)
                .await
                .map_err(|e| transport_error("连接", e))?;
            stream.login(&user, &password).await.map_err(login_error)?;
            Ok::<_, RemoteError>(stream)
        })?;

        Ok(Self {
            url: url.clone(),
            rt: Arc::new(rt),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 连接对应的地址（回显时不带密码）。
    pub fn url(&self) -> &RemoteUrl {
        &self.url
    }

    /// 探活：发一个 `NOOP` 看控制连接还在不在。
    ///
    /// `NOOP` 是 RFC 959 要求必须实现的、一次往返、不改任何状态——正好回答
    /// 「闲置这一阵之后，这条控制连接还在吗」。**必须带超时**（见
    /// [`crate::PROBE_TIMEOUT`]），超时按「断了」处理。
    ///
    /// 阻塞式：走本连接自有的 runtime，和其余动作同一个约定（见模块文档）。
    pub fn is_alive(&self) -> bool {
        self.rt.block_on(async {
            let probe = tokio::time::timeout(crate::PROBE_TIMEOUT, async {
                let mut conn = self.conn.lock().await;
                conn.noop().await
            })
            .await;
            matches!(probe, Ok(Ok(())))
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

    /// 在连接自有的 runtime 上驱动一个 future（供 `async` 方法使用，
    /// 避免跨 runtime 驱动这条连接的 socket——见模块文档）。
    ///
    /// 与 SFTP 后端的 `run` 是同一条约定：外层只 `await` 结果，真正的远程往返
    /// 由这条连接自己的 worker 完成。
    async fn run<F, T>(&self, f: F) -> T
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.rt.handle().spawn(f).await.expect("ftp 后台任务失败")
    }

    /// 列目录：优先 MLSD（机器可读、自带类型与大小），服务器不支持时回退 LIST。
    async fn list_dir(&self, dir: &str) -> Result<Vec<File>, MoError> {
        let dir = dir.to_string();
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            match conn.mlsd(Some(&dir)).await {
                Ok(lines) => Ok(lines.iter().filter_map(|l| parse_mlsd(l)).collect()),
                // 服务器不实现 MLSD 是常态（RFC 3659 是可选扩展），回退到 LIST。
                Err(_) => {
                    let lines = conn
                        .list(Some(&dir))
                        .await
                        .map_err(|e| MoError::from(transport_error("列目录", e)))?;
                    Ok(lines.iter().filter_map(|l| parse_list(l)).collect())
                }
            }
        })
        .await
    }

    async fn entries_in(&self, dir: &str) -> Result<Vec<ReadDirEntry>, MoError> {
        let parent = PathBuf::from(dir);
        Ok(self
            .list_dir(dir)
            .await?
            .into_iter()
            .filter(|f| f.name() != "." && f.name() != "..")
            .map(|f| {
                let path = parent.join(file_name_only(f.name()));
                ReadDirEntry::new(
                    FileId::synthetic(&path),
                    file_name_only(f.name()).to_string(),
                    if f.is_directory() {
                        EntryKind::Directory
                    } else if f.is_symlink() {
                        EntryKind::Symlink
                    } else {
                        EntryKind::File
                    },
                    path,
                )
            })
            .collect())
    }
}

/// 把底层错误分成「连接断了」与「这一步没做成」。
///
/// 优先用 `io::ErrorKind`：`suppaftp` 把网络错误包在
/// `FtpError::ConnectionError(io::Error)` 里，能拿到真正的 kind；拿不到 kind 的
/// 再用错误文本兜底（见 [`crate::text_looks_disconnected`]）。
///
/// 这个分类决定上层「重连重试」还是「报错给用户」——用户报的那个
/// `Connection error: Broken pipe (os error 32)` 就是漏在了这里。
fn transport_error(kind: &'static str, e: suppaftp::FtpError) -> RemoteError {
    let text = e.to_string();
    let disconnected = match &e {
        suppaftp::FtpError::ConnectionError(io) => {
            crate::io_kind_is_disconnect(io.kind()) || crate::text_looks_disconnected(&text)
        }
        _ => crate::text_looks_disconnected(&text),
    };
    if disconnected {
        RemoteError::disconnected(kind, text)
    } else {
        RemoteError::transport(kind, text)
    }
}

/// 有些服务器在 MLSD / LIST 里回的是完整路径，只留最后一段当名字。
fn file_name_only(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn parse_mlsd(line: &str) -> Option<File> {
    ListParser::parse_mlsd(line.trim()).ok()
}

/// LIST 的两种主流格式：先按 POSIX（`drwxr-xr-x …`），不行再按 DOS（`01-01-70  …`）。
fn parse_list(line: &str) -> Option<File> {
    let l = line.trim();
    ListParser::parse_posix(l)
        .or_else(|_| ListParser::parse_dos(l))
        .ok()
}

/// 把 FTP 登录失败分成「凭据被拒」与「别的毛病」两类。
///
/// `530 Not logged in`（RFC 959）是登录失败的规范应答：用户名 / 密码不对，或
/// 服务器不接受匿名。**只有它能触发「请输入账号密码」的弹窗**；其余（421 服务
/// 不可用、连接被中断…）照旧走 `Transport`——否则一句「密码不对」会把真正的
/// 故障盖住，用户会在正确的密码上反复试。
fn login_error(e: suppaftp::FtpError) -> RemoteError {
    match &e {
        suppaftp::FtpError::UnexpectedResponse(resp) if resp.status.code() == 530 => {
            RemoteError::auth("登录", &e)
        }
        _ => transport_error("登录", e),
    }
}

#[async_trait]
impl FileSystem for FtpFileSystem {
    fn is_alive(&self) -> bool {
        FtpFileSystem::is_alive(self)
    }

    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.entries_in(&Self::remote(path)).await
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        // blocking 池里不能碰 mo-app 的 runtime；本连接自带的那份在这里正合适。
        let dir = Self::remote(path);
        self.rt.block_on(self.entries_in(&dir))
    }

    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError> {
        let remote = Self::remote(path);
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            // 目录没有 SIZE 语义（多数服务器直接报错 550），失败即按目录处理。
            let size = conn.size(&remote).await.ok().unwrap_or(0) as u64;
            let modified = conn.mdtm(&remote).await.ok().map(system_time_from_naive);
            Ok(FileMetadata {
                size,
                modified,
                created: None,
                permissions: Permissions::default(),
            })
        })
        .await
    }

    async fn create_dir(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            conn.mkdir(&remote)
                .await
                .map_err(|e| MoError::from(transport_error("建目录", e)))
        })
        .await
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, MoError> {
        use tokio::io::AsyncReadExt as _;
        let remote = Self::remote(path);
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            // `retr` 的闭包是异步的，且要把数据流**还回去**（suppaftp 12 的约定：
            // 由它自己 finish 并读取完成应答，我们不能在闭包里 finish）。
            conn.retr(&remote, |mut stream| {
                Box::pin(async move {
                    let mut buf = Vec::new();
                    stream
                        .read_to_end(&mut buf)
                        .await
                        .map_err(suppaftp::FtpError::ConnectionError)?;
                    Ok::<_, suppaftp::FtpError>((buf, stream))
                })
            })
            .await
            .map_err(|e| MoError::from(transport_error("下载", e)))
        })
        .await
    }

    async fn is_dir(&self, path: &Path) -> bool {
        // FTP 没有「这是什么类型」的直接指令：沿用 `metadata` 那条约定
        // ——目录没有 SIZE 语义（多数服务器直接报 550），SIZE 失败即当目录。
        let remote = Self::remote(path);
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            conn.size(&remote).await.is_err()
        })
        .await
    }

    async fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), MoError> {
        let remote = Self::remote(path);
        // future 要 `'static`：内容拷一份进去（远程往返期间 `contents` 的借用不能悬着）。
        let mut cursor = Cursor::new(contents.to_vec());
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            // 「目标已存在必须失败」由调用方保证（见 trait 文档）；FTP 的 STOR 会覆盖，
            // 这里先探一次 SIZE：有大小就认为已存在，绝不静默覆盖远端数据。
            if conn.size(&remote).await.is_ok() {
                return Err(MoError::Other(format!("远端已存在 {remote}——拒绝覆盖")));
            }
            conn.put_file(&remote, &mut cursor)
                .await
                .map_err(|e| MoError::from(transport_error("上传", e)))?;
            Ok(())
        })
        .await
    }

    async fn remove_file(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            conn.rm(&remote)
                .await
                .map_err(|e| MoError::from(transport_error("删除文件", e)))
        })
        .await
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            conn.rmdir(&remote)
                .await
                .map_err(|e| MoError::from(transport_error("删除目录", e)))
        })
        .await
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), MoError> {
        let from = Self::remote(from);
        let to = Self::remote(to);
        let conn = self.conn.clone();
        self.run(async move {
            let mut conn = conn.lock().await;
            conn.rename(&from, &to)
                .await
                .map_err(|e| MoError::from(transport_error("重命名", e)))
        })
        .await
    }
}

impl Drop for FtpFileSystem {
    fn drop(&mut self) {
        // 尽量优雅地 QUIT：失败无所谓（连接可能早就断了），但不能在 drop 里 panic。
        self.rt.block_on(async {
            let mut conn = self.conn.lock().await;
            let _: FtpResult<()> = conn.quit().await;
        });
    }
}

/// `mdtm` 给的是 UTC 的 `NaiveDateTime`（无时区），换算成 `SystemTime`。
fn system_time_from_naive(t: chrono::NaiveDateTime) -> std::time::SystemTime {
    use std::time::{Duration, UNIX_EPOCH};
    let secs = t.and_utc().timestamp().max(0) as u64;
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 530 `Not logged in` 是「服务器要账号密码」的唯一信号。
    ///
    /// UI 靠这个分类决定弹不弹认证框：判成 `Transport` 只会干显示一句错误
    /// （改造前的行为），判成 `AuthRequired` 才会问用户名密码。
    #[test]
    fn login_530_asks_for_credentials() {
        let err = suppaftp::FtpError::UnexpectedResponse(suppaftp::types::Response {
            status: suppaftp::Status::from(530u32),
            body: b"530 Login incorrect.".to_vec(),
        });
        assert!(
            matches!(login_error(err), RemoteError::AuthRequired { .. }),
            "530 应当被判为「需要凭据」"
        );
    }

    /// 其余登录失败仍是普通传输错误——别让「服务不可用」冒充「密码不对」，
    /// 那会让用户在一个正确的密码上反复试。
    #[test]
    fn login_other_failures_stay_transport() {
        let err = suppaftp::FtpError::UnexpectedResponse(suppaftp::types::Response {
            status: suppaftp::Status::from(421u32),
            body: b"421 Service not available.".to_vec(),
        });
        assert!(
            matches!(login_error(err), RemoteError::Transport { .. }),
            "421 不该被当成「需要凭据」"
        );
    }

    /// 这条连接自己的 runtime 必须**自己**驱动 spawn 出去的任务。
    ///
    /// `async` trait 方法（`metadata` / `rename` / `remove_*` …）走
    /// `rt.handle().spawn`，而调用方是 mo-app 的共享 runtime——它**不会**去
    /// `block_on` 这条连接的 runtime。所以那份 runtime 必须是**多线**的才有 worker
    /// 去 poll 这些任务；换成 current-thread，任务就永远挂在队列里，表现为
    /// 「删了 / 改名了但没反应，也不报错」——正是 SFTP 早就躲开、FTP 原来踩着的坑。
    #[test]
    fn the_connection_runtime_drives_its_own_tasks() {
        let url = RemoteUrl::parse("ftp://127.0.0.1:21").expect("测试地址应当能解析");
        let rt = FtpFileSystem::connection_runtime(&url).expect("runtime 应当能建起来");

        // 只 spawn，**不** block_on（模拟调用方在别的 runtime 上 await 结果）。
        let task = rt.handle().spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            7u8
        });

        // 在**另一个** runtime 上等它——这是真实调用姿势（mo-app 的共享 runtime）。
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("调用方 runtime");
        let got = caller.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), task)
                .await
                .expect("任务应当被这条连接自己的 worker 驱动完成——而不是永远挂着")
                .expect("任务不该 panic")
        });
        assert_eq!(got, 7);
    }

    /// 连接被掐断（用户报的那个 `Broken pipe (os error 32)`）必须归到
    /// `Disconnected`——上层据此重建连接重试，而不是把它当普通错误弹给用户。
    #[test]
    fn broken_pipe_is_classified_as_disconnected() {
        let e = suppaftp::FtpError::ConnectionError(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "Broken pipe",
        ));
        assert!(
            matches!(
                transport_error("列目录", e),
                RemoteError::Disconnected { .. }
            ),
            "断掉的连接要能被认出来"
        );
    }

    /// 别的失败照旧是普通传输错误：别把「550 目录不存在」当成断线去重连
    /// （那会变成「每次进一个不存在的目录都重登一次」）。
    #[test]
    fn other_failures_stay_transport() {
        let e = suppaftp::FtpError::UnexpectedResponse(suppaftp::types::Response {
            status: suppaftp::Status::from(550u32),
            body: b"550 Failed to change directory.".to_vec(),
        });
        assert!(matches!(
            transport_error("列目录", e),
            RemoteError::Transport { .. }
        ));
    }

    #[test]
    fn paths_are_normalized_to_remote_absolute() {
        assert_eq!(FtpFileSystem::remote(Path::new("/pub")), "/pub");
        assert_eq!(FtpFileSystem::remote(Path::new("pub/x")), "/pub/x");
        assert_eq!(
            FtpFileSystem::remote(Path::new("pub\\x")),
            "/pub/x",
            "Windows 分隔符也要归一"
        );
    }

    #[test]
    fn list_lines_are_parsed_in_both_dialects() {
        let posix = "-rw-r--r-- 1 user group 1234 Nov 5 13:46 example.txt";
        let f = parse_list(posix).expect("POSIX 行应能解析");
        assert_eq!(f.name(), "example.txt");
        assert!(f.is_file());
        assert_eq!(f.size(), 1234);

        let dir = "drwxr-xr-x 2 user group 4096 Nov 5 13:46 incoming";
        assert!(parse_list(dir).expect("目录行应能解析").is_directory());
    }

    #[test]
    fn full_paths_from_servers_are_reduced_to_names() {
        assert_eq!(file_name_only("/pub/incoming/a.txt"), "a.txt");
        assert_eq!(file_name_only("a.txt"), "a.txt");
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
