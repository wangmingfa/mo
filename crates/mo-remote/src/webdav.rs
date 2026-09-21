//! WebDAV 后端：把 `reqwest_dav::Client` 包装成 [`mo_fs::FileSystem`]。
//!
//! ## 路径约定
//!
//! 与 FTP / SFTP 后端一致：传给本实现的路径一律是**远程绝对路径**
//! （`/pub/incoming`），主机与凭据由本对象持有，上层只记「正在浏览哪个连接」。
//!
//! ## 与 FTP / SFTP 的关键差异：HTTP 没有会话
//!
//! FTP 有控制连接、SSH 有通道，闲置久了会被单方面掐断，所以那两个后端**必须**
//! 自带 runtime 把 socket 钉在同一个 reactor 上。WebDAV 走 HTTP：每个请求独立、
//! 连接池由 `reqwest` 内部管，`Client` 本身 `Clone + Send + Sync`，没有「断线」
//! 这种会话态。因此这里不需要独占连接对象，只需一个 runtime 来驱动 `reqwest`：
//!
//! - `read_dir_blocking`（mo-app 列目录走的这条路）在 blocking 池上被调用，那里
//!   没有任何 reactor，`reqwest` 需要一处在跑的 runtime——用本连接自有的多线
//!   runtime `block_on` 驱动。
//! - 其余 `async` 方法经 `rt.handle().spawn(...)` 把请求派到常驻 worker 上跑，
//!   外层只 `await` 结果（与 SFTP 后端同一套做法，避免跨 runtime 驱动）。
//!
//! ## 凭据错误为什么要在 connect 就探一次
//!
//! FTP/SSH 在 `connect` 阶段就有登录握手，凭据不对当场报 `AuthRequired`。HTTP 是
//! 惰性的——建 `Client` 不做任何网络往返，401 要到第一个请求才冒出来。所以
//! [`WebDavFileSystem::connect`] 主动对根目录 PROPFIND 一次，把「凭据被拒」在连接
//! 阶段就归成 `AuthRequired`，UI 才会弹「输入账号密码」的框，而不是浏览时才报错。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use mo_core::{EntryKind, FileId, FileMetadata, MoError, Permissions};
use mo_fs::{FileSystem, ReadDirEntry};
use percent_encoding::percent_decode_str;
use reqwest_dav::types::list_cmd::{ListProp, ListResponse};
use reqwest_dav::{Auth, ClientBuilder, DecodeError, Depth, Error as DavError};
use tokio::runtime::Runtime;

use crate::{RemoteError, RemoteUrl};

/// 一个 WebDAV 连接。
pub struct WebDavFileSystem {
    url: RemoteUrl,
    /// 驱动 `reqwest` 的 runtime（见模块文档：HTTP 无会话，只需一处在跑的 reactor）。
    rt: Arc<Runtime>,
    /// 底层客户端：`Clone + Send + Sync`，方法都收 `&self`，连接池由 reqwest 自管，
    /// 因此不必像 FTP/SFTP 那样用 Mutex 串起来。
    client: reqwest_dav::Client,
}

impl WebDavFileSystem {
    /// 建客户端并（为尽早发现凭据被拒）探一次根目录。
    pub fn connect(url: &RemoteUrl) -> Result<Self, RemoteError> {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .map_err(|e| RemoteError::transport("创建 runtime", e))?,
        );

        let base = base_url(url);
        let auth = match (url.user.clone(), url.password.clone()) {
            (Some(user), Some(password)) => Auth::Basic(user, password),
            (Some(user), None) => Auth::Basic(user, String::new()),
            (None, _) => Auth::Anonymous,
        };

        let client = rt.block_on(async {
            let client = ClientBuilder::new()
                .set_host(base)
                .set_auth(auth)
                .build()
                .map_err(|e| classify("连接", &e))?;
            // HTTP 无登录握手：凭据错误只在第一个请求上暴露。先对根目录 PROPFIND
            // 一次，把 401 / 403 归成 AuthRequired，让上层在连接阶段就能弹认证框。
            if let Err(e) = client.list_rsp("/", Depth::Number(0)).await {
                let remote = classify("连接", &e);
                // 只有「要凭据」才阻断连接；其余（根目录不可枚举、403 之外的
                // 奇怪应答）先放行，真正浏览到具体目录时再报，别把能用的连接判死。
                if matches!(remote, RemoteError::AuthRequired { .. }) {
                    return Err(remote);
                }
            }
            Ok::<_, RemoteError>(client)
        })?;

        Ok(Self {
            url: url.clone(),
            rt,
            client,
        })
    }

    /// 连接对应的地址（回显时不带密码）。
    pub fn url(&self) -> &RemoteUrl {
        &self.url
    }

    /// 探活：对根目录 PROPFIND（Depth 0）一次往返。
    ///
    /// 选它作为「还活着吗」的探针：每个 WebDAV 服务器都必须能 PROPFIND 根、一次
    /// 往返、不改任何状态。**带超时**（见 [`crate::PROBE_TIMEOUT`]），超时按「断了」
    /// 处理。注意 401 / 403 仍算「活着」——连上了、只是鉴权/授权问题，重连也没用。
    pub fn is_alive(&self) -> bool {
        let client = self.client.clone();
        self.rt.block_on(async {
            let probe = tokio::time::timeout(crate::PROBE_TIMEOUT, async move {
                client.list_rsp("/", Depth::Number(0)).await
            })
            .await;
            match probe {
                Ok(Ok(_)) => true,
                Ok(Err(e)) => matches!(classify("探活", &e), RemoteError::AuthRequired { .. }),
                Err(_) => false,
            }
        })
    }

    /// 把 trait 里的路径统一成远程绝对路径字符串（与 FTP / SFTP 同法）。
    fn remote(path: &Path) -> String {
        let s = path.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            s
        } else {
            format!("/{s}")
        }
    }

    /// 在本连接自有的 runtime 上驱动一个「返回已分类错误」的请求。
    async fn dispatch<T>(
        &self,
        kind: &'static str,
        fut: impl std::future::Future<Output = Result<T, RemoteError>> + Send + 'static,
    ) -> Result<T, MoError>
    where
        T: Send + 'static,
    {
        match self.rt.handle().spawn(fut).await {
            Ok(res) => res.map_err(MoError::from),
            Err(e) => Err(MoError::Other(format!("{kind} 任务失败：{e}"))),
        }
    }

    /// 目标是否存在（供 `write_file` 拒绝覆盖远端文件用）。
    ///
    /// Depth-0 PROPFIND：拿到应答即存在，404 即不存在，其余错误照实抛出。
    async fn exists(&self, remote: &str) -> Result<bool, RemoteError> {
        let client = self.client.clone();
        let path = remote.to_string();
        self.rt
            .handle()
            .spawn(async move {
                match client.list_rsp(&path, Depth::Number(0)).await {
                    Ok(_) => Ok(true),
                    Err(e) => match status_code(&e) {
                        Some(404) => Ok(false),
                        _ => Err(classify("检查存在", &e)),
                    },
                }
            })
            .await
            .map_err(|e| RemoteError::transport("检查存在", e))?
    }
}

/// 目标地址的 scheme 归一成 `http` / `https`，并拼出 `scheme://host[:port]`。
///
/// 刻意**不写默认端口**（80 / 443），让 `reqwest` 按 scheme 隐含的端口走——与
/// [`RemoteUrl::authority`] 的「默认端口省略」一致，否则同一个连接会长出两个样子。
/// 用户显式写了端口才带上。WebDAV 服务器常挂在子路径（如 Nextcloud 的
/// `/remote.php/dav`）下，但那是「浏览位置」的一部分、由上层路径承载，不烧进
/// base URL，与 FTP/SFTP 的处理方式相同。
fn base_url(url: &RemoteUrl) -> String {
    let scheme = match url.scheme.as_str() {
        "davs" | "https" => "https",
        _ => "http",
    };
    let host = if url.host.contains(':') && !url.host.starts_with('[') {
        format!("[{}]", url.host)
    } else {
        url.host.clone()
    };
    match url.port {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    }
}

/// 从错误里抠出 HTTP 状态码（若有）。WebDAV 侧只有两种载体：
/// `Decode(Server)` 与 `Decode(StatusMismatched)`。
fn status_code(e: &DavError) -> Option<u16> {
    match e {
        DavError::Decode(DecodeError::Server(s)) => Some(s.response_code),
        DavError::Decode(DecodeError::StatusMismatched(s)) => Some(s.response_code),
        _ => None,
    }
}

/// 把底层错误分成四类，语义与 FTP / SFTP 后端一致：
///
/// * 401 / 403 → [`RemoteError::AuthRequired`]（UI 弹认证框）；
/// * 其它带 HTTP 状态的应答 → [`RemoteError::Transport`]（这一步没做成，与连接无关）；
/// * 纯传输层失败（连不上 / DNS / 超时 / 发不出，`reqwest` 侧）→ [`RemoteError::Disconnected`]
///   （HTTP 无会话，标成断线让上层「重试一次」即可，正好覆盖瞬时网络抖动）；
/// * 剩下（响应解码失败等）→ [`RemoteError::Transport`]。
fn classify(kind: &'static str, e: &DavError) -> RemoteError {
    if let Some(code) = status_code(e) {
        return match code {
            401 | 403 => RemoteError::auth(kind, e),
            _ => RemoteError::transport(kind, e),
        };
    }
    match e {
        DavError::Reqwest(_) | DavError::ReqwestDecode(_) => RemoteError::disconnected(kind, e),
        _ => RemoteError::transport(kind, e),
    }
}

/// PROPFIND 的 `href` 常常是百分号编码、甚至带绝对 URI 前缀（`http://host/dav/x`）。
/// 还原成从服务器根算起的路径：先剥掉 `scheme://host`，再百分号解码。
fn decoded_path(href: &str) -> String {
    let s = percent_decode_str(href).decode_utf8_lossy();
    match s.find("://") {
        Some(i) => match s[i + 3..].find('/') {
            Some(j) => s[i + 3 + j..].to_string(),
            None => "/".to_string(),
        },
        None => s.into_owned(),
    }
}

/// 去掉结尾的 `/`（根目录归一成空串），用作比较与拼接的基准。
fn trim_trailing(p: &str) -> &str {
    p.trim_end_matches('/')
}

/// 从一条 `ListResponse` 里取状态为 2xx 的那段属性（PROPFIND 可能带多段 propstat，
/// 分段回报哪些属性成功）。没有成功段就返回 `None`（该条不计入结果）。
fn ok_prop(resp: &ListResponse) -> Option<&ListProp> {
    resp.prop_stat
        .iter()
        .find(|ps| {
            ps.status
                .split_whitespace()
                .nth(1)
                .is_some_and(|c| c.starts_with('2'))
        })
        .map(|ps| &ps.prop)
}

/// 把 Depth-1 的 PROPFIND 应答映射成上层目录项。
///
/// PROPFIND Depth 1 会把「被请求的目录本身」也作为第一条返回（href == 请求路径），
/// 必须滤掉；名字取 href 的最后一段。
fn entries_from(dir: &str, responses: Vec<ListResponse>) -> Vec<ReadDirEntry> {
    let dir_key = trim_trailing(dir).to_string();
    let mut out = Vec::with_capacity(responses.len());
    for resp in responses {
        let Some(prop) = ok_prop(&resp) else {
            continue;
        };
        let path = trim_trailing(&decoded_path(&resp.href)).to_string();
        // 目录自身（含根目录：path 为空）——不是子项，跳过。
        if path.is_empty() || path == dir_key {
            continue;
        }
        let name = match path.rsplit_once('/') {
            Some((_, name)) => name,
            None => path.as_str(),
        }
        .to_string();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let child = if dir_key.is_empty() {
            format!("/{name}")
        } else {
            format!("{dir_key}/{name}")
        };
        let child_path = PathBuf::from(&child);
        let kind = if prop.resource_type.collection.is_some() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        out.push(ReadDirEntry::new(
            FileId::synthetic(&child_path),
            name,
            kind,
            child_path,
        ));
    }
    out
}

/// UTC 时刻换算成 `SystemTime`（负数按 0 兜底，与 FTP 侧一致）。
fn system_time_from_utc(t: chrono::DateTime<chrono::Utc>) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(t.timestamp().max(0) as u64)
}

#[async_trait]
impl FileSystem for WebDavFileSystem {
    fn is_alive(&self) -> bool {
        WebDavFileSystem::is_alive(self)
    }

    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        let dir = Self::remote(path);
        let client = self.client.clone();
        let dir_for_entries = dir.clone();
        let responses = self
            .dispatch("列目录", async move {
                client
                    .list_rsp(&dir, Depth::Number(1))
                    .await
                    .map_err(|e| classify("列目录", &e))
            })
            .await?;
        Ok(entries_from(&dir_for_entries, responses))
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        let dir = Self::remote(path);
        let client = self.client.clone();
        let dir_for_entries = dir.clone();
        let responses = self
            .rt
            .block_on(async move {
                client
                    .list_rsp(&dir, Depth::Number(1))
                    .await
                    .map_err(|e| classify("列目录", &e))
            })
            .map_err(MoError::from)?;
        Ok(entries_from(&dir_for_entries, responses))
    }

    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError> {
        let remote = Self::remote(path);
        let client = self.client.clone();
        let remote_for_msg = remote.clone();
        let responses = self
            .dispatch("读取元数据", async move {
                client
                    .list_rsp(&remote, Depth::Number(0))
                    .await
                    .map_err(|e| classify("读取元数据", &e))
            })
            .await?;
        let prop = responses
            .iter()
            .find_map(ok_prop)
            .ok_or_else(|| MoError::Other(format!("读不到 {remote_for_msg} 的元数据")))?;
        Ok(FileMetadata {
            size: prop.content_length.unwrap_or(0).max(0) as u64,
            modified: prop.last_modified.map(system_time_from_utc),
            created: None,
            permissions: Permissions::default(),
        })
    }

    async fn create_dir(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let client = self.client.clone();
        self.dispatch("建目录", async move {
            client
                .mkcol(&remote)
                .await
                .map_err(|e| classify("建目录", &e))
        })
        .await
    }

    async fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), MoError> {
        let remote = Self::remote(path);
        // WebDAV 的 PUT 会覆盖：先探一次存在性，绝不静默覆盖远端数据（与 FTP/SFTP 同法）。
        if self.exists(&remote).await? {
            return Err(MoError::Other(format!("远端已存在 {remote}——拒绝覆盖")));
        }
        let body = contents.to_vec();
        let client = self.client.clone();
        self.dispatch("上传", async move {
            client
                .put(&remote, body)
                .await
                .map_err(|e| classify("上传", &e))
        })
        .await
    }

    async fn remove_file(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let client = self.client.clone();
        self.dispatch("删除文件", async move {
            client
                .delete(&remote)
                .await
                .map_err(|e| classify("删除文件", &e))
        })
        .await
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), MoError> {
        let remote = Self::remote(path);
        let client = self.client.clone();
        // WebDAV 的 DELETE 对集合是递归删除，正合 trait 的「删除目录（递归）」语义。
        self.dispatch("删除目录", async move {
            client
                .delete(&remote)
                .await
                .map_err(|e| classify("删除目录", &e))
        })
        .await
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), MoError> {
        let from = Self::remote(from);
        let to = Self::remote(to);
        let client = self.client.clone();
        self.dispatch("重命名", async move {
            client
                .mv(&from, &to)
                .await
                .map_err(|e| classify("重命名", &e))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest_dav::types::list_cmd::{ListPropStat, ListResourceType};
    use reqwest_dav::{ServerError, StatusMismatchedError};

    /// `ListProp` 不 derive `Default`（只有 `ListResourceType` 有），测里手搭全字段。
    fn lp(collection: bool) -> ListProp {
        ListProp {
            last_modified: None,
            resource_type: ListResourceType {
                collection: collection.then_some(()),
                ..Default::default()
            },
            quota_used_bytes: None,
            quota_available_bytes: None,
            tag: None,
            content_length: None,
            content_type: None,
            calendar_data: None,
        }
    }
    fn resp(href: &str, collection: bool) -> ListResponse {
        ListResponse {
            href: href.to_string(),
            prop_stat: vec![ListPropStat {
                status: "HTTP/1.1 200 OK".to_string(),
                prop: lp(collection),
            }],
        }
    }

    #[test]
    fn paths_are_normalized_to_remote_absolute() {
        assert_eq!(WebDavFileSystem::remote(Path::new("/pub")), "/pub");
        assert_eq!(WebDavFileSystem::remote(Path::new("pub/x")), "/pub/x");
        assert_eq!(
            WebDavFileSystem::remote(Path::new("pub\\x")),
            "/pub/x",
            "Windows 分隔符也要归一"
        );
    }

    /// davs / https 走加密端口，webdav / dav 走明文；默认端口不写进 base URL。
    #[test]
    fn base_url_maps_scheme_and_keeps_explicit_port() {
        let u = RemoteUrl::parse("webdav://host/dav").unwrap();
        assert_eq!(base_url(&u), "http://host");
        let u = RemoteUrl::parse("davs://host/dav").unwrap();
        assert_eq!(base_url(&u), "https://host", "davs 隐含 443，不写默认端口");
        let u = RemoteUrl::parse("webdav://host:8080/dav").unwrap();
        assert_eq!(base_url(&u), "http://host:8080", "显式端口要保留");
        let u = RemoteUrl::parse("davs://[::1]:8443/dav").unwrap();
        assert_eq!(base_url(&u), "https://[::1]:8443", "IPv6 宿主加方括号");
    }

    /// 401 / 403 必须判成「需要凭据」——UI 靠这个分类弹认证框。
    #[test]
    fn auth_status_is_classified_as_auth_required() {
        for code in [401u16, 403] {
            let e = DavError::Decode(DecodeError::Server(ServerError {
                response_code: code,
                exception: "x".into(),
                message: "denied".into(),
            }));
            assert!(
                matches!(classify("连接", &e), RemoteError::AuthRequired { .. }),
                "{code} 应当触发认证框"
            );
        }
    }

    /// 其它 HTTP 状态是「这一步没做成」，不是断线——别为「404 目录不存在」去重连。
    #[test]
    fn other_status_is_transport_not_disconnect() {
        let e = DavError::Decode(DecodeError::StatusMismatched(StatusMismatchedError {
            response_code: 404,
            expected_code: 207,
        }));
        assert!(matches!(
            classify("列目录", &e),
            RemoteError::Transport { .. }
        ));
    }

    /// href 可能带百分号编码与绝对 URI 前缀，都要还原成根相对路径。
    #[test]
    fn hrefs_are_decoded_to_server_relative_paths() {
        assert_eq!(decoded_path("/dav/foo%20bar.txt"), "/dav/foo bar.txt");
        assert_eq!(
            decoded_path("http://host/dav/%E4%B8%AD%E6%96%87"),
            "/dav/中文",
            "剥掉 scheme://host 前缀并解码 UTF-8"
        );
    }

    /// Depth-1 应答里第一条是目录自身，必须滤掉；子项名字取 href 末段。
    #[test]
    fn self_entry_is_filtered_and_children_are_named() {
        let entries = entries_from(
            "/pub",
            vec![
                resp("/pub/", true),               // 目录自身 → 滤掉
                resp("/pub/reports/", true),       // 子目录
                resp("/pub/notes%20a.txt", false), // 子文件（名字含空格）
            ],
        );
        assert_eq!(entries.len(), 2, "自身不应计入");
        assert_eq!(entries[0].name, "reports");
        assert_eq!(entries[0].kind, EntryKind::Directory);
        assert_eq!(entries[0].path, PathBuf::from("/pub/reports"));
        assert_eq!(entries[1].name, "notes a.txt");
        assert_eq!(entries[1].kind, EntryKind::File);
    }

    /// 根目录（dir = "/"）下拼子项不能长出双斜杠。
    #[test]
    fn root_children_paths_have_single_separator() {
        let entries = entries_from("/", vec![resp("/hello.txt", false)]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("/hello.txt"));
    }
}
