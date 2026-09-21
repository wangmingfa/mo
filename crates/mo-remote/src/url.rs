//! 远程地址：解析、归一化与回显。
//!
//! 形状：`scheme://[user[:password]@]host[:port]/path`
//!
//! 刻意自己解析而不用 `url` crate：只用得到其中一小撮语义，而这里有几个
//! 与浏览器 URL **不同**的取舍，写清楚比套通用解析器更好维护：
//!
//! * **路径必须以 `/` 开头且是绝对路径**：文件管理器没有「相对地址」的概念，
//!   `ftp://host` 的根是 `/`，不是空串。
//! * **默认端口不写回 Host 串**：`ftp://h` 与 `ftp://h:21` 同义，回显时统一省略，
//!   否则「已连接的地址」列表里同一个连接会长出两个样子。
//! * **密码原样保留**：`@` / `:` 都是合法密码字符，因此按**最后一个 `@`** 切用户信息，
//!   第一个 `:` 之后全是密码。

use crate::RemoteError;

/// 各协议的默认端口表。
pub const DEFAULT_PORTS: &[(&str, u16)] = &[
    ("ftp", 21),
    ("ftps", 990),
    ("sftp", 22),
    ("ssh", 22),
    ("webdav", 80),
    ("dav", 80),
    ("davs", 443),
    ("http", 80),
    ("https", 443),
    ("smb", 445),
    ("nfs", 2049),
];

/// 某协议的默认端口（未知协议返回 `None`）。
pub fn default_port(scheme: &str) -> Option<u16> {
    DEFAULT_PORTS
        .iter()
        .find(|(s, _)| *s == scheme)
        .map(|(_, p)| *p)
}

/// 一个远程地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    /// 协议名，一律小写（`FTP://` 与 `ftp://` 是同一个）。
    pub scheme: String,
    /// 用户名（`None` 表示匿名 / 由后端决定）。
    pub user: Option<String>,
    /// 密码（`None` 表示不携带或提示时询问）。
    pub password: Option<String>,
    /// 主机名或 IP（IPv6 允许带方括号）。
    pub host: String,
    /// 显式指定的端口（`None` 表示用协议默认端口）。
    pub port: Option<u16>,
    /// 远程绝对路径，**总是以 `/` 开头**。
    pub path: String,
}

impl RemoteUrl {
    /// 解析一个远程地址。
    pub fn parse(input: &str) -> Result<Self, RemoteError> {
        let s = input.trim();
        let Some((scheme, rest)) = split_scheme(s)? else {
            return Err(RemoteError::BadUrl("缺少 `scheme://` 前缀".to_string()));
        };
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            return Err(RemoteError::BadUrl("缺少主机名".to_string()));
        }

        // 用户信息：按最后一个 @ 切（密码里允许有 @），之后第一个 : 之后全是密码。
        let (userinfo, hostport) = match rest.rsplit_once('@') {
            Some((info, hp)) => (Some(info), hp),
            None => (None, rest),
        };
        if hostport.is_empty() {
            return Err(RemoteError::BadUrl("缺少主机名".to_string()));
        }
        let (user, password) = match userinfo {
            Some(info) => match info.split_once(':') {
                Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
                None => (Some(info.to_string()), None),
            },
            None => (None, None),
        };
        if userinfo.is_some() && user.as_deref().is_some_and(str::is_empty) {
            return Err(RemoteError::BadUrl("用户名为空".to_string()));
        }

        let (host, port, path) = split_host_port_path(hostport)?;
        if path.is_empty() {
            return Err(RemoteError::BadUrl("路径必须是一条绝对路径".to_string()));
        }

        Ok(Self {
            scheme,
            user,
            password,
            host,
            port,
            path,
        })
    }

    /// 实际连接端口（显式指定优先，否则协议默认）。
    pub fn port_or_default(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }

    /// `host[:port]` 的规范化写法：IPv6 加方括号，默认端口省略。
    ///
    /// 抽出来是因为 [`RemoteUrl::display`] 与 [`RemoteUrl::endpoint`] 必须共用
    /// 同一套写法——否则同一个连接会在「已连接的地址」和「记住的服务器」里
    /// 长出两个样子，钥匙串条目也就跟着对不上了。
    fn authority(&self) -> String {
        let host_part = match self.host.contains(':') && !self.host.starts_with('[') {
            true => format!("[{}]", self.host),
            false => self.host.clone(),
        };
        let port_part = match (self.port, default_port(&self.scheme)) {
            (Some(p), Some(d)) if p == d => String::new(),
            (Some(p), _) => format!(":{p}"),
            (None, _) => String::new(),
        };
        format!("{host_part}{port_part}")
    }

    /// 回显成地址串：默认端口与密码都不会写回来。
    ///
    /// 密码不回显是有意的——这个串会进 UI（侧边栏 / 标题栏）与日志。
    pub fn display(&self) -> String {
        let user_part = self
            .user
            .as_ref()
            .map(|u| format!("{u}@"))
            .unwrap_or_default();
        format!(
            "{}://{}{}{}",
            self.scheme,
            user_part,
            self.authority(),
            self.path
        )
    }

    /// 连接标识：`scheme://host[:port]`，**不含用户名 / 密码 / 路径**。
    ///
    /// 钥匙串条目与「记住的服务器」都用它当 key：同一台机器换个用户登录、或
    /// 浏览到别的目录，都该落在同一条记录上。也正因为不含用户名，用户只敲
    /// `ftp://主机:端口` 时才能查到之前存下的凭据。
    pub fn endpoint(&self) -> String {
        format!("{}://{}", self.scheme, self.authority())
    }

    /// 去掉具体路径，只留「连到哪台机器的哪个位置」。用作连接标识。
    pub fn base(&self) -> String {
        let mut stripped = self.clone();
        stripped.path = "/".to_string();
        stripped.display()
    }

    /// 地址栏回显：用给定路径替换原路径后再回显（不写密码、不写默认端口）。
    ///
    /// 浏览远程目录时 `panel.path` 是远程绝对路径（如 `/pub`），需要把它拼回
    /// `scheme://host` 上得到完整、可复制的 URL 显示给用户。
    pub fn display_at(&self, path: &str) -> String {
        let mut u = self.clone();
        u.path = path.to_string();
        u.display()
    }
}

/// 切出 `scheme` 与其余部分；`None` 表示没有合法的 `://`。
fn split_scheme(s: &str) -> Result<Option<(String, &str)>, RemoteError> {
    match s.find("://") {
        Some(i) => {
            let scheme = s[..i].to_ascii_lowercase();
            if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(RemoteError::BadUrl(format!("非法协议名 `{}`", &s[..i])));
            }
            Ok(Some((scheme, &s[i + 3..])))
        }
        None => Ok(None),
    }
}

/// 把 `host[:port][/path]` 切成三份；缺端口或缺路径都给默认值。
fn split_host_port_path(s: &str) -> Result<(String, Option<u16>, String), RemoteError> {
    let (hostport, path) = match s.find('/') {
        Some(i) => (&s[..i], s[i..].to_string()),
        None => (s, "/".to_string()),
    };

    if let Some(close) = hostport.find(']') {
        // IPv6 字面量：`[::1]:2121`
        let host = hostport[1..close].to_string();
        let rest = &hostport[close + 1..];
        let port = parse_port(rest.strip_prefix(':'))?;
        return Ok((host, port, path));
    }

    // 普通情形：最后一个冒号后是纯数字才算端口；**不是数字就要报错**——
    // 否则 `h:not-a-port` 会被当成「主机名叫 h:not-a-port」，把坏输入一直
    // 留到连接时才炸（那里只会给一句不知所云的 DNS 失败）。
    match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            let port = parse_port(Some(p))?;
            Ok((h.to_string(), port, path))
        }
        Some((_, p)) if !p.is_empty() => Err(RemoteError::BadUrl(format!("非法端口号 `{p}`"))),
        // 裸写 IPv6（`::1`）不带方括号时按主机原样处理：它与端口分隔符冲突，
        // 规范写法是加方括号，见解析成功的那条测试。
        _ => Ok((hostport.to_string(), None, path)),
    }
}

fn parse_port(raw: Option<&str>) -> Result<Option<u16>, RemoteError> {
    match raw {
        None => Ok(None),
        Some(p) => p
            .parse::<u16>()
            .map(Some)
            .map_err(|_| RemoteError::BadUrl(format!("非法端口号 `{p}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_path_and_defaults_root() {
        let u = RemoteUrl::parse("ftp://example.com").expect("应解析成功");
        assert_eq!(u.scheme, "ftp");
        assert_eq!(u.host, "example.com");
        assert_eq!(u.path, "/", "没给路径时根是 `/` 而不是空串");
        assert_eq!(u.port, None);
        assert_eq!(u.port_or_default(), Some(21));
    }

    /// 连接标识只留 `scheme://host[:port]`。
    ///
    /// 钥匙串条目与「记住的服务器」都按它索引，所以用户名 / 密码 / 路径都不能
    /// 进去——同一台机器换个用户登录、或浏览到别的目录，都得命中同一条记录。
    #[test]
    fn endpoint_keeps_only_scheme_host_and_port() {
        let u = RemoteUrl::parse("ftp://alice:s3cr3t@example.com:2121/pub/incoming")
            .expect("应解析成功");
        assert_eq!(u.endpoint(), "ftp://example.com:2121");
    }

    /// 默认端口不写进标识：`ftp://host` 与 `ftp://host:21` 是同一台。
    ///
    /// 否则用户两种写法各存一份凭据，勾了「记住密码」也认不出来。
    #[test]
    fn endpoint_omits_the_default_port() {
        let short = RemoteUrl::parse("ftp://host").expect("应解析成功");
        let long = RemoteUrl::parse("ftp://host:21").expect("应解析成功");
        assert_eq!(short.endpoint(), long.endpoint());
        assert_eq!(short.endpoint(), "ftp://host");
    }

    #[test]
    fn scheme_is_case_insensitive() {
        let u = RemoteUrl::parse("FTP://Example.COM/pub").expect("应解析成功");
        assert_eq!(u.scheme, "ftp");
        assert_eq!(u.path, "/pub");
    }

    #[test]
    fn parses_user_password_port_and_path() {
        let u = RemoteUrl::parse("ftp://alice:s3cr3t@10.0.0.5:2121/incoming/x.txt")
            .expect("应解析成功");
        assert_eq!(u.user.as_deref(), Some("alice"));
        assert_eq!(u.password.as_deref(), Some("s3cr3t"));
        assert_eq!(u.host, "10.0.0.5");
        assert_eq!(u.port, Some(2121));
        assert_eq!(u.path, "/incoming/x.txt");
    }

    #[test]
    fn at_sign_in_password_does_not_break_host() {
        // 按**最后一个** @ 切：密码里的 @ 属于用户信息。
        let u = RemoteUrl::parse("ftp://alice:p@ssw0rd@example.com/pub").expect("应解析成功");
        assert_eq!(u.user.as_deref(), Some("alice"));
        assert_eq!(u.password.as_deref(), Some("p@ssw0rd"));
        assert_eq!(u.host, "example.com");
        assert_eq!(u.path, "/pub");
    }

    #[test]
    fn ipv6_host_with_port() {
        let u = RemoteUrl::parse("ftp://[::1]:2121/pub").expect("应解析成功");
        assert_eq!(u.host, "::1");
        assert_eq!(u.port, Some(2121));
    }

    #[test]
    fn display_omits_default_port_and_password() {
        let u = RemoteUrl::parse("ftp://alice:s3cr3t@h:21/pub").expect("应解析成功");
        assert_eq!(u.display(), "ftp://alice@h/pub", "默认端口与密码都不回显");

        let u2 = RemoteUrl::parse("ftp://h:2121/pub").expect("应解析成功");
        assert_eq!(u2.display(), "ftp://h:2121/pub", "非默认端口要保留");
    }

    #[test]
    fn ipv6_display_adds_brackets() {
        let u = RemoteUrl::parse("ftp://[::1]/pub").expect("应解析成功");
        assert_eq!(u.display(), "ftp://[::1]/pub");
    }

    #[test]
    fn base_strips_the_path() {
        let u = RemoteUrl::parse("ftp://alice@h:21/pub/deep/file.txt").expect("应解析成功");
        assert_eq!(u.base(), "ftp://alice@h/");
    }

    #[test]
    fn display_at_replaces_only_the_path() {
        let u = RemoteUrl::parse("ftp://alice@h:21/pub").expect("应解析成功");
        // 密码 / 默认端口不回显，但路径换成当前浏览位置。
        assert_eq!(u.display_at("/pub/incoming"), "ftp://alice@h/pub/incoming");
    }

    #[test]
    fn rejects_malformed_urls() {
        for bad in [
            "example.com/pub", // 缺 scheme
            "ftp://",          // 缺主机
            "ftp://h:not-a-port/p",
            "ftp://@h/p",      // 空用户名
            "ftp://h:99999/p", // 端口越界
        ] {
            assert!(
                RemoteUrl::parse(bad).is_err(),
                "`{bad}` 本应解析失败却成功了 —— 这是放行坏输入、会在连接时才炸"
            );
        }
    }
}
