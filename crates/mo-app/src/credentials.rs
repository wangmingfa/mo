//! 远程服务器的凭据：用户名连同密码存进**系统钥匙串**。
//!
//! 为什么不用 `config.json`：那是明文 JSON，把密码写进去只是把「写在地址栏里」
//! 换个地方泄露。这里走平台原生的加密存储——macOS Keychain、Windows 凭据管理器、
//! Linux Secret Service（gnome-keyring / KWallet），与浏览器存密码同一级别。
//! `config.json` 里只留服务器地址与用户名（见 `mo_config::SavedServer`）。
//!
//! 条目的 key 是连接的**规范化标识**（`scheme://host[:port]`，见 `RemoteUrl::endpoint`），
//! 值是一行 JSON `{"user":…,"password":…}`——钥匙串的一个条目只能存一个字符串，
//! 用户名与密码打包在一起才不会出现「只剩用户名」的半截记录。
//!
//! **钥匙串不可用时不致命**：无桌面会话的 Linux、没装 keyring 的容器都会初始化
//! 失败。此时 [`store`] 返回错误（UI 提示「这次不记住」）、[`load`] 返回 `None`
//! （照常走匿名登录、需要时弹认证框），浏览远程文件本身不受影响。

use serde::{Deserialize, Serialize};

/// 钥匙串里的服务名。macOS 上命令行能查到：
/// `security find-generic-password -s mo-remote`。
const SERVICE: &str = "mo-remote";

/// 一个条目里存的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Secret {
    user: String,
    password: String,
}

/// 条目 key：连接的规范化标识。
///
/// 用的是 `RemoteUrl::endpoint()` 的输出而不是用户敲进来的原串，所以
/// `ftp://Host` 与 `ftp://host:21` 会命中同一条记录。
fn entry(endpoint: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, endpoint).map_err(|e| describe("打开钥匙串条目", &e))
}

/// 把「记住密码」写进钥匙串（同一个 endpoint 已有条目就覆盖）。
pub fn store(endpoint: &str, user: &str, password: &str) -> Result<(), String> {
    let secret = Secret {
        user: user.to_string(),
        password: password.to_string(),
    };
    let json = serde_json::to_string(&secret).map_err(|e| e.to_string())?;
    entry(endpoint)?
        .set_password(&json)
        .map_err(|e| describe("写入钥匙串", &e))
}

/// 取出记住的用户名与密码。
///
/// 没存过、钥匙串不可用、条目内容坏了——一律返回 `None`。这条路径在每次连接前
/// 都会走一遍，「没有记录」是常态，不该在 UI 上冒错；调用方拿到 `None` 就照常
/// 走匿名 / 弹认证框。
pub fn load(endpoint: &str) -> Option<(String, String)> {
    let ok = entry(endpoint).ok()?;
    match ok.get_password() {
        Ok(json) => match serde_json::from_str::<Secret>(&json) {
            Ok(s) => Some((s.user, s.password)),
            Err(e) => {
                tracing::debug!(endpoint, "钥匙串条目内容无法解析，按未保存处理：{e}");
                None
            }
        },
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            tracing::debug!(endpoint, "读取钥匙串失败，按未保存处理：{e}");
            None
        }
    }
}

/// 删掉一台服务器存下的凭据（「忘记」）。本来就没存过也算成功。
pub fn forget(endpoint: &str) -> Result<(), String> {
    match entry(endpoint)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(describe("删除钥匙串条目", &e)),
    }
}

/// 钥匙串错误 → 给人看的一句话。
///
/// `NoDefaultStore` 是最常见的一种：Linux 上没有 Secret Service（无桌面会话 /
/// 容器里），或系统钥匙串没启动。它的默认 Display 是一串英文错误码，直接甩给
/// 用户没用，这里换成一句能懂的话。
fn describe(what: &str, e: &keyring::Error) -> String {
    match e {
        keyring::Error::NoDefaultStore => {
            format!("{what}失败：系统钥匙串不可用，本次不会记住密码")
        }
        other => format!("{what}失败：{other}"),
    }
}
