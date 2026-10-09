//! provider 宿主（P3，devlog/plugin-system.md §5）：起进程、握手、一问一答、
//! 超时 kill、退避停用、空闲回收。
//!
//! ## 协议（JSON Lines over stdio）
//!
//! 刻意**不用** LSP 的 `Content-Length` 头，也不用完整 JSON-RPC：这里只有「一问一答」、
//! 不需要并发帧、不需要通知——头部与通知只增加插件作者的出错面。
//!
//! ```text
//! 宿主 → 插件：{"id":1,"method":"classify","params":{...}}\n
//! 插件 → 宿主：{"id":1,"result":{...}}\n   或   {"id":1,"error":"人话"}\n
//! ```
//!
//! 生命周期：`initialize{protocol:1}` → `{name, version, methods[]}`（`methods` 必须是
//! 清单声明的子集，否则只信清单）；退出先发 `shutdown`，不退就 kill。
//!
//! ## 进程模型与纪律
//!
//! * **一个扩展至多一个进程**（`Host` 内部把调用串行化——协议本就是一问一答，
//!   并发帧是给作者挖坑）；同一次 UI 操作内超时**不重试**。
//! * 超时 / 崩溃 = kill 该进程 + 记一次连续失败；连续 3 次进指数退避
//!   （`disabled_until`），期间所有调用直接短路——**内置预览照常**，扩展管理器
//!   亮「已停用」。
//! * 插件 → 宿主只有一个 `host.log`（stderr 落盘）。刻意不做 `host.read_file`
//!   这类反向请求：有了它，capability 模型就是破的（插件想读什么自己发个路径即可）。
//! * 进程只能拿到入参里给的东西：`classify` 的 `head_b64`（文件头 4 KiB）仅在清单
//!   授予 `read-contents` 时由**宿主**读取并附上——capability 的执行点在派发侧，
//!   不在进程侧。
//! * 全部阻塞 IO 走 std 线程 / blocking 池；**任何等待都不许出现在渲染路径上**
//!   （渲染只查 `Manager` 的内存表，零 IO）。
//!
//! ## `classify` 结果只收 `label`
//!
//! 协议允许插件答 `group` / `icon_key` / `columns`，但界面今天只消费「种类」列的
//! 文案——按「收一个字段就投一个字段」的规矩，多余的答案收下但不用（将来界面多出
//! 消费点再逐个放行），并在 devlog §4.14 记账。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::extensions::{self, Extension};
use crate::AppState;

/// 协议版本。清单里的 `provider` 段不写版本——握手时问。
pub const PROTOCOL_VERSION: u64 = 1;

/// 握手帧 id 的起点：`2^53 - 1`，即 JS `Number.MAX_SAFE_INTEGER`。
///
/// 协议帧 id 是 u64，但 provider 可能是任何语言写的——JS 系（bun/node）的 number
/// 只能无损表示 ≤2^53 的整数。握手 id 从这里往下数，既保住「与调用帧（从 0 起）
/// 不撞车」的初衷，又保证 JS 系 provider 能原样回显。
pub const JS_SAFE_ID: u64 = (1u64 << 53) - 1;

/// 握手超时缺省（清单 `startup_timeout_ms` 可覆盖）。
pub const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 2000;
/// 单次调用超时缺省（清单 `call_timeout_ms` 可覆盖）。
pub const DEFAULT_CALL_TIMEOUT_MS: u64 = 800;

/// 空闲多久回收进程。插件进程常驻没好处（起一个通常 <50ms），白占一份内存。
pub const IDLE_RECYCLE: Duration = Duration::from_secs(30);

/// 连续失败多少次进退避。
pub const BACKOFF_AFTER: u32 = 3;
/// 退避基数与上限：第 3 次失败停 5s，之后翻倍，封顶 5 分钟。
const BACKOFF_BASE: Duration = Duration::from_secs(5);
const BACKOFF_CAP: Duration = Duration::from_secs(300);

/// `classify` 附文件头的长度（devlog §5：前 4 KiB，且仅在授予 `read-contents` 时）。
pub const HEAD_BYTES: usize = 4096;

// ---------- 协议帧 ----------

#[derive(Serialize)]
struct Request<'a> {
    id: u64,
    method: &'a str,
    params: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct Response {
    id: u64,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct InitResult {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    methods: Vec<String>,
}

// ---------- base64（只编码：宿主从不收位图，见 devlog §5） ----------

/// 标准字母表的 base64 编码。够用 15 行就不为此引一个依赖。
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---------- 进程规格 ----------

/// 一个 provider 进程怎么起、能说什么、有什么能力。由清单派生（[`Self::from_extension`]）。
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// 扩展 id（也是缓存与日志的命名空间）。
    pub ext_id: String,
    /// argv：`argv[0]` 已按扩展目录解析成绝对/可直接执行的形式。
    pub argv: Vec<String>,
    /// 进程工作目录 = 扩展目录（插件带的数据文件按相对路径找得到）。
    pub dir: PathBuf,
    /// 清单声明的方法（已过 `validate`，只认 classify / preview）。
    pub methods: Vec<String>,
    /// 清单声明的能力（原样保留；`read-contents` 的执行点在派发侧）。
    pub capabilities: Vec<String>,
    /// 这个扩展用 `types` 认领的扩展名（小写、不含点，先到先得已裁过）。
    pub covered_exts: Vec<String>,
    /// stderr 落盘位置（host.log，扩展管理器「查看日志」的落点）。
    pub log_path: PathBuf,
    /// 握手超时。
    pub startup_timeout: Duration,
    /// 单次调用超时。
    pub call_timeout: Duration,
    /// 握手时传给插件的设置对象：清单声明的 defaults 为底、用户改过的值覆盖
    /// （合并在 [`Manager::host`] 做）。空对象 = 没声明设置。
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl SpawnSpec {
    /// 从一份清单派生进程规格；`owners` 是「扩展名 → 赢家扩展 id」的先到先得表
    /// （调用方按目录序填），本扩展只保留自己赢下的那些扩展名。
    pub fn from_extension(ext: &Extension, owners: &BTreeMap<String, String>) -> Option<Self> {
        let p = ext.manifest.provider.as_ref()?;
        let dir = ext
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        // run[0] 相对路径按扩展目录解析；`Command::new` 对相对路径的查找行为跨平台
        // 有差异（是否含 cwd 不一致），先变成绝对路径就没有歧义。
        let mut argv = p.run.clone();
        let prog = Path::new(&argv[0]);
        if prog.is_relative() {
            argv[0] = dir.join(prog).to_string_lossy().to_string();
        }
        let covered_exts: Vec<String> = ext
            .manifest
            .types
            .iter()
            .flat_map(|t| t.ext.iter())
            .filter_map(|raw| {
                // `validate` 已拦认不出的写法；这里再过一遍规范化（不 panic）。
                let key = raw.trim().trim_start_matches('.').to_ascii_lowercase();
                owners
                    .get(&key)
                    .filter(|w| *w == &ext.manifest.id)
                    .map(|_| key)
            })
            .collect();
        let log_path = mo_cache::cache_dir()
            .join("plugin-logs")
            .join(&ext.manifest.id)
            .join("host.log");
        Some(Self {
            ext_id: ext.manifest.id.clone(),
            argv,
            dir,
            methods: p.methods.clone(),
            capabilities: ext.manifest.capabilities.clone(),
            covered_exts,
            settings: crate::extensions::settings_defaults(&ext.manifest),
            log_path,
            startup_timeout: Duration::from_millis(
                p.startup_timeout_ms.unwrap_or(DEFAULT_STARTUP_TIMEOUT_MS),
            ),
            call_timeout: Duration::from_millis(
                p.call_timeout_ms.unwrap_or(DEFAULT_CALL_TIMEOUT_MS),
            ),
        })
    }
}

// ---------- 进程与调用 ----------

/// 一个活着的 provider 子进程。
struct Proc {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    /// stdout 行通道：专用读线程把行搬进来；线程随进程退出（EOF）自然结束。
    rx: Receiver<String>,
    /// 本进程的应答帧计数器（握手期从 u64::MAX 往下数，调用期从 0 递增）。
    next_id: u64,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 调用失败的四种形态。Timeout / Crashed 记连续失败（进程不健康）；
/// Protocol 是插件**健康地**说「答不了」（不计失败）；Backoff 是退避期短路。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    Timeout,
    Crashed(String),
    Protocol(String),
    Backoff,
}

/// 一个扩展的 provider 宿主：进程生命周期 + 退避账。
pub struct Host {
    spec: SpawnSpec,
    inner: std::sync::Mutex<HostInner>,
    health: std::sync::Mutex<Health>,
}

struct HostInner {
    proc: Option<Proc>,
    next_id: u64,
    last_used: Instant,
}

struct Health {
    consecutive: u32,
    disabled_until: Option<Instant>,
    /// 最近一次失败的原因（扩展管理器「已停用」那行要说清为什么）。
    last_error: Option<String>,
}

impl Host {
    pub fn new(spec: SpawnSpec) -> Self {
        Self {
            spec,
            inner: std::sync::Mutex::new(HostInner {
                proc: None,
                next_id: 0,
                last_used: Instant::now(),
            }),
            health: std::sync::Mutex::new(Health {
                consecutive: 0,
                disabled_until: None,
                last_error: None,
            }),
        }
    }

    pub fn spec(&self) -> &SpawnSpec {
        &self.spec
    }

    /// 退避是否在生效中（扩展管理器亮「已停用」的判据）。
    pub fn disabled_until(&self) -> Option<(Instant, u32, String)> {
        let h = self.health.lock().unwrap();
        h.disabled_until
            .filter(|until| *until > Instant::now())
            .map(|until| {
                (
                    until,
                    h.consecutive,
                    h.last_error.clone().unwrap_or_default(),
                )
            })
    }

    /// 发起一次调用（阻塞，至多 `call_timeout`；握手另算 `startup_timeout`）。
    ///
    /// **串行化**是刻意的：协议只有一问一答，同一个进程上并发两问没有意义——
    /// 内层的 `Mutex` 就是「排队」两个字的实现。
    pub fn call(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        if self.disabled_until().is_some() {
            return Err(CallError::Backoff);
        }
        let mut inner = self.inner.lock().unwrap();
        // 空闲回收：闲置超过 `IDLE_RECYCLE` 的进程就地丢掉（Drop 会 kill + wait），
        // 下一次调用重新起。这不是为了省内存——是防止插件在两次调用之间被升级/换掉
        // 之后还拿着旧句柄答话。
        if inner.last_used.elapsed() > IDLE_RECYCLE {
            inner.proc = None;
        }
        if inner.proc.is_none() {
            if let Err(e) = self.spawn_and_handshake(&mut inner) {
                drop(inner);
                self.record(e.clone());
                return Err(e);
            }
        }
        inner.last_used = Instant::now();
        let id = inner.next_id;
        inner.next_id += 1;

        let req =
            serde_json::to_string(&Request { id, method, params }).expect("协议帧序列化不会失败");
        {
            let proc = inner.proc.as_mut().expect("上面刚确保进程在");
            if let Err(e) = writeln!(proc.stdin, "{req}").and_then(|_| proc.stdin.flush()) {
                // 写不进去 = 管道断了（进程死了）。
                let err = CallError::Crashed(format!("stdin 断了：{e}"));
                inner.proc = None;
                drop(inner);
                self.record(err.clone());
                return Err(err);
            }
        }

        // 收答循环单独成块：借用到块尾即还，之后才能动 `inner.proc`（kill 那几臂）。
        enum Outcome {
            Answer(serde_json::Value),
            Protocol(String),
            Timeout,
            Disconnected,
        }
        let outcome = {
            let deadline = Instant::now() + self.spec.call_timeout;
            let proc = inner.proc.as_mut().expect("上面刚确保进程在");
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match proc.rx.recv_timeout(remaining) {
                    Ok(line) => match serde_json::from_str::<Response>(&line) {
                        Ok(resp) if resp.id == id => {
                            if let Some(err) = resp.error {
                                break Outcome::Protocol(err);
                            }
                            break Outcome::Answer(resp.result.unwrap_or(serde_json::Value::Null));
                        }
                        // 不成帧的行（插件往 stdout 写了人话）与迟到的旧应答：等下一行。
                        _ => {
                            tracing::warn!(target: "mo_provider",
                                "扩展 {} 的进程输出不成帧或不是本轮应答：{line:.120}",
                                self.spec.ext_id);
                            continue;
                        }
                    },
                    Err(RecvTimeoutError::Timeout) => break Outcome::Timeout,
                    Err(RecvTimeoutError::Disconnected) => break Outcome::Disconnected,
                }
            }
        };
        match outcome {
            Outcome::Answer(result) => {
                self.record_clear();
                Ok(result)
            }
            Outcome::Protocol(err) => Err(CallError::Protocol(err)),
            Outcome::Timeout => {
                // 超时 = kill 该进程（devlog §5），同一次 UI 操作内不重试。
                inner.proc = None;
                drop(inner);
                self.record(CallError::Timeout);
                Err(CallError::Timeout)
            }
            Outcome::Disconnected => {
                inner.proc = None;
                drop(inner);
                let err = CallError::Crashed("进程退出了（stdout 关闭）".into());
                self.record(err.clone());
                Err(err)
            }
        }
    }

    /// 起进程 + 握手。失败只报错误（一律记连续失败——握手都过不去的进程不可信）。
    fn spawn_and_handshake(&self, inner: &mut HostInner) -> Result<(), CallError> {
        let mut cmd = std::process::Command::new(&self.spec.argv[0]);
        cmd.args(&self.spec.argv[1..])
            .current_dir(&self.spec.dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        // stderr 落 host.log：插件 → 宿主的唯一通道（设计稿 §5）。
        if let Some(parent) = self.spec.log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.spec.log_path)
        {
            Ok(f) => {
                cmd.stderr(std::process::Stdio::from(f));
            }
            Err(e) => {
                tracing::warn!(target: "mo_provider",
                    "扩展 {} 的 host.log 打不开（{e}），stderr 丢弃", self.spec.ext_id);
                cmd.stderr(std::process::Stdio::null());
            }
        }
        // Windows：GUI 构建的 exe 接 stdio 会冒黑窗（devlog §5）。
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| CallError::Crashed(format!("起进程失败：{e}")))?;
        let stdin = child.stdin.take().expect("刚声明 piped");
        let stdout = child.stdout.take().expect("刚声明 piped");
        let (tx, rx) = std::sync::mpsc::channel();
        let ext_id = self.spec.ext_id.clone();
        if let Err(e) = std::thread::Builder::new()
            .name(format!("provider-{ext_id}"))
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(l) => {
                            if tx.send(l).is_err() {
                                break; // 宿主已经不要了
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CallError::Crashed(format!("起读线程失败：{e}")));
        }

        let mut proc = Proc {
            child,
            stdin,
            rx,
            // 握手帧用 id 空间的最高「JS 安全整数」（2^53-1，往下数）：与调用帧（从 0
            // 递增）在实际时间尺度上永不撞车——迟到的握手应答不会被误认成某次调用的
            // 回答。**不能用 u64::MAX**：JS/bun 实现的 provider 里 number 是 double，
            // 超过 2^53 的整数会被舍入（u64::MAX → 18446744073709552000），宿主按
            // 「id 对不上」把正确回包丢掉，握手白等到超时（2026-10-09 srt-tools 实案）。
            next_id: JS_SAFE_ID,
        };
        // 握手：initialize → {name, version, methods[]}。settings 随握手授予——
        // 插件不读这个字段也不破坏（协议帧本来就是宽松解析）；改设置后宿主会
        // forget_ext 重建进程，新值在下一次握手生效。
        let params = serde_json::json!({
            "protocol": PROTOCOL_VERSION,
            "settings": self.spec.settings,
        });
        let deadline = Instant::now() + self.spec.startup_timeout;
        // 握手失败多半是进程坏了：kill 掉再上报（proc drop 时也会 kill，
        // 让「失败路径不留进程」与代码顺序无关）。
        let init = roundtrip(&mut proc, "initialize", &params, deadline)?;
        let init: InitResult = serde_json::from_value(init)
            .map_err(|e| CallError::Protocol(format!("initialize 应答不成形：{e}")))?;
        tracing::debug!(target: "mo_provider",
            "扩展 {} 握手完成：{} {}",
            self.spec.ext_id, init.name, init.version);
        // methods 必须是清单声明的子集，否则只信清单（devlog §5）。这里只记差异，
        // 真正的「信谁」由派发侧按清单判——清单是装扩展时用户看过确认卡的那份。
        for m in &init.methods {
            if !self.spec.methods.contains(m) {
                tracing::warn!(target: "mo_provider",
                    "扩展 {} 的进程自称会「{m}」，清单没声明——只信清单",
                    self.spec.ext_id);
            }
        }
        // 落位：调用的 id 从 0 开始（握手占了 u64::MAX 的高端）。
        proc.next_id = 0;
        inner.proc = Some(proc);
        Ok(())
    }

    fn record_clear(&self) {
        let mut h = self.health.lock().unwrap();
        h.consecutive = 0;
        h.disabled_until = None;
    }

    /// 记一次进程级失败（超时 / 崩溃 / 起不来）；连续 3 次进指数退避。
    fn record(&self, err: CallError) {
        let mut h = self.health.lock().unwrap();
        h.consecutive += 1;
        h.last_error = Some(match &err {
            CallError::Timeout => "调用超时".to_string(),
            CallError::Crashed(m) => format!("进程崩溃：{m}"),
            CallError::Protocol(m) => format!("协议错误：{m}"),
            CallError::Backoff => "退避中".to_string(),
        });
        if h.consecutive >= BACKOFF_AFTER {
            let extra = h.consecutive - BACKOFF_AFTER;
            let delay = BACKOFF_BASE
                .checked_mul(1u32 << extra.min(8))
                .unwrap_or(BACKOFF_CAP)
                .min(BACKOFF_CAP);
            h.disabled_until = Some(Instant::now() + delay);
            tracing::warn!(target: "mo_provider",
                "扩展 {} 连续失败 {} 次，停用 {:?}",
                self.spec.ext_id, h.consecutive, delay);
        }
    }
}

/// 一次「写问、读答」的原语（握手专用；普通调用在 `Host::call` 里有自己的循环——
/// 那边超时要动 `inner.proc`，两处合用反而别扭）。
fn roundtrip(
    proc: &mut Proc,
    method: &str,
    params: &serde_json::Value,
    deadline: Instant,
) -> Result<serde_json::Value, CallError> {
    let id = proc.next_id;
    proc.next_id = proc.next_id.wrapping_sub(1);
    let req = serde_json::to_string(&Request { id, method, params }).expect("协议帧序列化不会失败");
    writeln!(proc.stdin, "{req}")
        .and_then(|_| proc.stdin.flush())
        .map_err(|e| CallError::Crashed(format!("stdin 断了：{e}")))?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match proc.rx.recv_timeout(remaining) {
            Ok(line) => {
                let Ok(resp) = serde_json::from_str::<Response>(&line) else {
                    continue;
                };
                if resp.id != id {
                    continue;
                }
                if let Some(err) = resp.error {
                    return Err(CallError::Protocol(err));
                }
                return Ok(resp.result.unwrap_or(serde_json::Value::Null));
            }
            Err(RecvTimeoutError::Timeout) => return Err(CallError::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(CallError::Crashed("进程退出了（stdout 关闭）".into()))
            }
        }
    }
}

// ---------- 管理器 ----------

/// 全部 provider 宿主 + classify 结果的内存表。进程级一份，挂在 [`crate::AppState`]。
///
/// 渲染路径只允许调 [`Self::classify_label_of`] / [`Self::needs_classify`] /
/// [`Self::backoff_line`]——短锁查表、零 IO。真活（起进程、问插件、落缓存）都在
/// blocking 池（[`crate::AppState::request_classify`]）。
pub struct Manager {
    inner: std::sync::Mutex<ManagerInner>,
}

struct ManagerInner {
    /// 扩展 id → 宿主。懒创建：声明了 provider 的扩展被真正问到才起 Host（Host
    /// 本身便宜，进程才是贵的）。
    hosts: HashMap<String, Arc<Host>>,
    /// `(扩展目录签名, 规格表, 扩展名归属表)`：与 type_labels 同一条签名缓存纪律。
    /// 归属表包 `Arc` 是因为列表**每帧**取一次快照（行循环里只查表，见
    /// [`Self::owners_snapshot`]），别让它每帧深拷贝一份。
    specs: Option<SpecsCache>,
    /// classify 结果内存表：路径 → (归属扩展 id, 标签)。带归属是因为卸载时要按
    /// 扩展清账，而路径本身不携带「谁答的」。
    labels: HashMap<PathBuf, (String, String)>,
    /// 在途去重：后台批次还没落账的路径不再排第二个任务（与缩略图 inflight 同理）。
    inflight: HashSet<PathBuf>,
    /// sqlite 缓存。打开失败降级为 None（功能退化为不落盘，不影响正确性）。
    cache: Option<Arc<mo_cache::ClassifyCache>>,
    /// list 行的测试种子（headless 不起真进程）。键 `(扩展 id, source)`。
    list_seeds: HashMap<(String, String), Vec<ListRow>>,
}

/// [`Manager`] 的签名缓存行：`(扩展目录签名, 规格表, 扩展名归属表)`。
type SpecsCache = (
    u64,
    BTreeMap<String, Arc<SpawnSpec>>,
    Arc<BTreeMap<String, String>>,
);

impl Manager {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for Manager {
    fn default() -> Self {
        Self {
            inner: std::sync::Mutex::new(ManagerInner {
                hosts: HashMap::new(),
                specs: None,
                labels: HashMap::new(),
                inflight: HashSet::new(),
                cache: None,
                list_seeds: HashMap::new(),
            }),
        }
    }
}

impl Manager {
    /// 签名变了就重扫扩展目录、重建规格表与归属表。返回归属表（扩展名 → 扩展 id）。
    fn sync_specs(&self, config_json: &Path) -> BTreeMap<String, String> {
        let mut g = self.inner.lock().unwrap();
        let root = extensions::extensions_root(config_json);
        let fp = extensions::fingerprint(&root);
        if let Some((seen, _, owners)) = &g.specs {
            if *seen == fp {
                return (**owners).clone();
            }
        }
        let exts = extensions::load(&root);
        // 先到先得的归属表：load 按目录名排过序，谁先占谁是确定的（与 type_labels
        // 同一条规则——两个 provider 抢同一个扩展名时结果必须确定）。
        let mut owners: BTreeMap<String, String> = BTreeMap::new();
        for e in &exts {
            if !e.manifest.enabled {
                continue;
            }
            for t in &e.manifest.types {
                for raw in &t.ext {
                    let key = raw.trim().trim_start_matches('.').to_ascii_lowercase();
                    owners.entry(key).or_insert_with(|| e.manifest.id.clone());
                }
            }
        }
        let mut specs = BTreeMap::new();
        for e in &exts {
            if e.manifest.provider.is_some() {
                if let Some(spec) = SpawnSpec::from_extension(e, &owners) {
                    specs.insert(spec.ext_id.clone(), Arc::new(spec));
                }
            }
        }
        let owners = Arc::new(owners);
        g.specs = Some((fp, specs, owners.clone()));
        (*owners).clone()
    }

    /// 归属表快照（扩展名 → 扩展 id）。**每帧取一次**（签名检查要做一次 read_dir，
    /// 与 `type_labels` 每帧那次同价），行循环里只对快照查表、零 IO。
    pub fn owners_snapshot(&self, config_json: &Path) -> Arc<BTreeMap<String, String>> {
        self.sync_specs(config_json);
        self.inner
            .lock()
            .unwrap()
            .specs
            .as_ref()
            .map(|(_, _, o)| o.clone())
            .unwrap_or_default()
    }

    /// 取（或建）一个扩展的宿主。`config_json` 供规格缓存失效用；`overrides`
    /// 是用户改过的设置值（config.json 里的那份），与规格里清单声明的 defaults
    /// 合并成握手要传的 settings——只在**建进程**时生效，改过设置要由调用方
    /// `forget_ext` 让宿主重建。
    pub fn host(
        &self,
        ext_id: &str,
        config_json: &Path,
        overrides: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<Arc<Host>> {
        self.sync_specs(config_json);
        let mut g = self.inner.lock().unwrap();
        if let Some(host) = g.hosts.get(ext_id) {
            return Some(host.clone());
        }
        let specs = &g.specs.as_ref()?.1;
        let spec = specs.get(ext_id)?;
        let mut spec = (**spec).clone();
        if !overrides.is_empty() {
            spec.settings
                .extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        let host = Arc::new(Host::new(spec));
        g.hosts.insert(ext_id.to_string(), host.clone());
        Some(host)
    }

    /// 这个路径该由哪个扩展的 provider 答 `classify`（没归属就 `None`）。
    pub fn owner_of(&self, path: &Path, config_json: &Path) -> Option<String> {
        let owners = self.sync_specs(config_json);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())?;
        owners.get(&ext).cloned()
    }

    /// 渲染路径查询：这个路径有没有 classify 标签。
    pub fn classify_label_of(&self, path: &Path) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .labels
            .get(path)
            .map(|(_, l)| l.clone())
    }

    /// 渲染路径查询：该不该为这条路径派 classify 任务（有归属、还没答案、没在途）。
    pub fn needs_classify(&self, path: &Path, config_json: &Path) -> bool {
        let g = self.inner.lock().unwrap();
        if g.labels.contains_key(path) || g.inflight.contains(path) {
            return false;
        }
        drop(g);
        self.owner_of(path, config_json).is_some()
    }

    /// 批次入口去重：把没在途的路径标上在途并返回（已在途的丢弃）。
    pub fn take_inflight(&self, paths: &[PathBuf]) -> Vec<PathBuf> {
        let mut g = self.inner.lock().unwrap();
        paths
            .iter()
            .filter(|p| g.inflight.insert((*p).clone()))
            .cloned()
            .collect()
    }

    pub fn clear_inflight(&self, paths: &[PathBuf]) {
        let mut g = self.inner.lock().unwrap();
        for p in paths {
            g.inflight.remove(p);
        }
    }

    /// 落一批答案：内存表 + sqlite（调用方在 blocking 池）。`rows` 是完整账目：
    /// `(扩展 id, 路径, 写入当时 mtime 秒, 写入当时 size, 标签)`——mtime / size 是
    /// 缓存失效的依据（devlog §7：与缩略图同一套），落库时必须带上。
    pub fn store_labels(&self, rows: &[(String, PathBuf, i64, i64, String)]) {
        let mut g = self.inner.lock().unwrap();
        for (ext_id, path, _, _, label) in rows {
            g.labels
                .insert(path.clone(), (ext_id.clone(), label.clone()));
        }
        if let Some(cache) = &g.cache {
            let _ = cache.put_many(rows);
        }
    }

    /// 打开（或复用）sqlite 缓存；打不开就 None（不落盘）。
    pub fn cache(&self) -> Option<Arc<mo_cache::ClassifyCache>> {
        let mut g = self.inner.lock().unwrap();
        if g.cache.is_none() {
            g.cache = mo_cache::ClassifyCache::open(&mo_cache::plugin_classify_file())
                .ok()
                .map(Arc::new);
        }
        g.cache.clone()
    }

    /// 卸载扩展时清它的账：宿主、内存表里它答的那些行、sqlite 里它的全部行。
    pub fn forget_ext(&self, ext_id: &str) {
        let mut g = self.inner.lock().unwrap();
        g.hosts.remove(ext_id);
        g.labels.retain(|_, (owner, _)| owner != ext_id);
        if let Some(cache) = &g.cache {
            let _ = cache.clear_ext(ext_id);
        }
    }

    /// 退避期的那句人话（扩展管理器「已停用」行）；不在退避期则 `None`。
    ///
    /// **只查已经存在的宿主**，不懒创建——这是渲染路径上的查询，起 `Host` 结构
    /// （乃至顺带做签名检查的 read_dir）都越界了；从没用过的 provider 不可能在
    /// 退避期，`None` 就是正确答案。
    pub fn backoff_line(&self, ext_id: &str) -> Option<String> {
        let g = self.inner.lock().unwrap();
        let host = g.hosts.get(ext_id)?;
        let (until, fails, reason) = host.disabled_until()?;
        let secs = until
            .saturating_duration_since(Instant::now())
            .as_secs()
            .max(1);
        Some(format!(
            "provider 已停用：连续失败 {fails} 次（{reason}），约 {secs} 秒后自动恢复；日志：{}",
            host.spec().log_path.display()
        ))
    }

    /// 测试钩子：直接种一条 classify 答案（headless 测试不起真进程）。
    pub fn seed_label_for_tests(&self, path: &Path, label: &str) {
        self.inner
            .lock()
            .unwrap()
            .labels
            .insert(path.to_path_buf(), ("test".to_string(), label.to_string()));
    }

    /// 调一个扩展的 `list` 方法取一页行。**阻塞**（进程 IO + 每行一次 stat）——
    /// 只许在 blocking 池里调，渲染路径上出现就是事故（模块头那条红线）。
    ///
    /// stat 的两重用意：一是「双击行不用再判目录文件」（点击是渲染路径上的事件，
    /// 那时再 stat 就迟了）；二是顺手把死路径筛成「点开失败有提示」而不是静默。
    /// 没有落盘缓存——列表是动态的（「最近文件」这类），第一版每次开面板拉一次，
    /// 够快也不说谎。
    pub fn list(
        &self,
        ext_id: &str,
        source: &str,
        config_json: &Path,
        overrides: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<ListRow>, CallError> {
        // 测试种子优先（见 `seed_list_rows_for_tests`）。
        if let Some(rows) = self
            .inner
            .lock()
            .unwrap()
            .list_seeds
            .get(&(ext_id.to_string(), source.to_string()))
        {
            return Ok(rows.clone());
        }
        let host = self
            .host(ext_id, config_json, overrides)
            .ok_or_else(|| CallError::Protocol(format!("扩展「{ext_id}」没有可用的 provider")))?;
        let result = host.call("list", &list_params(source))?;
        let mut rows = list_rows_from_result(&result);
        for r in &mut rows {
            if let Some(p) = &r.path {
                r.is_dir = p.is_dir();
            }
        }
        Ok(rows)
    }

    /// 测试钩子：直接种一个列表源的应答（headless 测试不起真进程，见 P3 同款）。
    pub fn seed_list_rows_for_tests(&self, ext_id: &str, source: &str, rows: Vec<ListRow>) {
        self.inner
            .lock()
            .unwrap()
            .list_seeds
            .insert((ext_id.to_string(), source.to_string()), rows);
    }
}

// ---------- 派发 ----------

/// `classify` 的入参。`head_b64` 仅在调用方核对过 `read-contents` 能力时才 Some——
/// **capability 的执行点在这里**：进程能拿到什么由宿主决定，不由插件开口。
pub fn build_classify_params(path: &Path, size: u64, head: Option<&[u8]>) -> serde_json::Value {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut v = serde_json::json!({
        "path": path.to_string_lossy(),
        "name": name,
        "size": size,
    });
    if let Some(head) = head {
        v["head_b64"] = serde_json::Value::String(base64_encode(head));
    }
    v
}

/// capability 执行点（classify 方向）：授了 `read-contents` 才允许读文件头。
/// 没授权时**连打开文件都不发生**——「少给」不是字段留空，是读都没读。
pub fn maybe_head(capabilities: &[String], path: &Path) -> Option<Vec<u8>> {
    capabilities
        .iter()
        .any(|c| c == "read-contents")
        .then(|| read_head(path))
        .flatten()
}

/// 读文件头（至多 `HEAD_BYTES`）。读失败按没有头处理——classify 的主答不需要它。
pub fn read_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let mut filled = 0;
    loop {
        let n = f.read(&mut buf[filled..]).ok()?;
        if n == 0 {
            break;
        }
        filled += n;
        if filled == buf.len() {
            break;
        }
    }
    buf.truncate(filled);
    Some(buf)
}

/// classify 应答里宿主消费的字段。协议允许 `group` / `icon_key` / `columns`，
/// 本轮只收 `label`（devlog §4.14：界面只有「种类」列一个消费点）。
#[derive(Deserialize)]
struct ClassifyResult {
    #[serde(default)]
    label: Option<String>,
}

/// preview 应答的形状：`kind` ∈ text / markdown / json / code / image-file / rows /
/// unsupported；文本类带 `text`，`image-file` 带 `path`（插件私有临时目录里的**路径**，
/// 由 Mo 的图片渲染器读——绝不接收位图字节，devlog §5）。
#[derive(Deserialize)]
struct PreviewResult {
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

/// 把 classify 应答折成一行标签（只收 `label`；空串按没答处理）。
pub fn classify_label_of_result(result: &serde_json::Value) -> Option<String> {
    let r: ClassifyResult = serde_json::from_value(result.clone()).ok()?;
    let label = r.label?;
    let label = label.trim().to_string();
    (!label.is_empty()).then_some(label)
}

/// 把 preview 应答折成 [`mo_preview::Preview`]；`None` = 这一问没答成，回落内置预览
/// （`rows` 属于 P4 的列表源，本轮没有渲染器，同样回落）。
pub fn preview_from_result(result: &serde_json::Value, size: u64) -> Option<mo_preview::Preview> {
    let r: PreviewResult = serde_json::from_value(result.clone()).ok()?;
    use mo_preview::{Preview, PreviewKind};
    let mk = |kind: PreviewKind, text: Option<String>| {
        Some(Preview {
            kind,
            title: String::new(), // 调用方（AppState::preview）会用文件名补
            text,
            image: None,
            size,
        })
    };
    match r.kind.as_str() {
        "text" => mk(PreviewKind::Text, r.text),
        "markdown" => mk(PreviewKind::Markdown, r.text),
        "json" => mk(PreviewKind::Json, r.text),
        "code" => mk(PreviewKind::Code, r.text),
        "image-file" => {
            let p = PathBuf::from(r.path?);
            if !p.is_file() {
                tracing::warn!(target: "mo_provider",
                    "preview image-file 指向的 {p:?} 不是文件，回落内置预览");
                return None;
            }
            Some(Preview {
                kind: PreviewKind::Image,
                title: String::new(),
                text: None,
                image: Some(p),
                size,
            })
        }
        "rows" => {
            tracing::warn!(target: "mo_provider",
                "preview 回了 rows（P4 的列表源），本轮没有渲染器，回落内置预览");
            None
        }
        "unsupported" => None,
        other => {
            tracing::warn!(target: "mo_provider", "preview 回了认不出的 kind「{other}」");
            None
        }
    }
}

// ---------- list 列表源（P4，devlog §5 表格最后一行） ----------

/// `list` 应答里的一行。
///
/// 第一版**只读**（devlog §9 风险 1 的止损）：收 `id` / `name` / `path` / `subtitle`；
/// `icon` 不收（没有可信图标表，§4.7 同一条「写了界面上没有」的纪律）；`next`
/// （翻页游标）不消费——分页正是 §9 点名的不变量之一，「不动的列表」先把第一页画稳。
#[derive(Debug, Clone, PartialEq)]
pub struct ListRow {
    /// 行标识（协议要求必填）。
    pub id: String,
    /// 主文案。
    pub name: String,
    /// 可选落点：有路径的行**双击**走 Mo 的导航（目录进入 / 文件交系统默认应用），
    /// 没有路径的行就是「不动的一行」。进程在本机跑，它给的路径就是本机视角的路径
    /// （远程挂载点对它来说也是本机目录）——所以落点走**本地入口**纪律（`open_local`）。
    pub path: Option<PathBuf>,
    /// 宿主 stat 补上的（见 [`Manager::list`]）；stat 失败按文件处理——点开失败
    /// 会有提示，不静默。
    pub is_dir: bool,
    /// 灰色副文案（可选）。
    pub subtitle: Option<String>,
}

/// `list` 入参：第一版只带 `source`（`query` / `cursor` 是协议字段，宿主暂时不发）。
pub fn list_params(source: &str) -> serde_json::Value {
    serde_json::json!({ "source": source })
}

/// 把 list 应答折成行。缺 `rows` / 不是数组 → 空表（调用方按「没答上来」处理）。
/// 单行缺 `id` / `name`：跳过这一行并 warn——成形的行照常收，一颗老鼠屎不倒一锅粥。
pub fn list_rows_from_result(result: &serde_json::Value) -> Vec<ListRow> {
    let Some(rows) = result.get("rows").and_then(|r| r.as_array()) else {
        tracing::warn!(target: "mo_provider", "list 应答里没有 rows 数组");
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            let parse_str = |key: &str| -> Option<String> {
                r.get(key)
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            };
            let Some(id) = parse_str("id") else {
                tracing::warn!(target: "mo_provider", "list 有一行没写 id，跳过：{r:.80}");
                return None;
            };
            let Some(name) = parse_str("name") else {
                tracing::warn!(target: "mo_provider", "list 行「{id}」没写 name，跳过");
                return None;
            };
            Some(ListRow {
                id,
                name,
                path: parse_str("path").map(PathBuf::from),
                is_dir: false,
                subtitle: parse_str("subtitle"),
            })
        })
        .collect()
}

// ---------- 批量派发（blocking 池专用，见 AppState::request_classify） ----------

/// 一批路径的 classify 派发：按归属扩展分组 → 逐个问宿主 → 命中缓存的直接落账 →
/// 全部落账后置 dirty 让刷新泵合并广播。
///
/// **失败语义**（devlog §5）：宿主超时 / 崩溃 / 退避 → 本批剩余的**全部放弃**（同一批
/// 就是一同一次 UI 操作，不重试）；插件健康地拒答（Protocol）→ 跳过该文件继续。
pub(crate) fn classify_batch(app: &AppState, paths: Vec<PathBuf>) {
    let config = AppState::config_path();
    let mut by_ext: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for p in &paths {
        if let Some(ext_id) = app.providers().owner_of(p, &config) {
            by_ext.entry(ext_id).or_default().push(p.clone());
        }
    }
    let mtime_secs = |md: &std::fs::Metadata| {
        md.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    };
    for (ext_id, group) in by_ext {
        let overrides = app.extension_setting_overrides(&ext_id);
        let host = app.providers().host(&ext_id, &config, &overrides);
        let host = match host {
            Some(h) if h.disabled_until().is_none() => h,
            _ => {
                // 没宿主（不该发生，防御一下）或退避中：本批放弃，在途标志照清。
                app.providers().clear_inflight(&group);
                continue;
            }
        };
        let cache = app.providers().cache();
        let mut rows: Vec<(String, PathBuf, i64, i64, String)> = Vec::new();
        for p in &group {
            // 条目可能刚被删：stat 不到就放过（标签它也用不上了）。
            let Ok(md) = std::fs::metadata(p) else {
                continue;
            };
            let (mtime, size) = (mtime_secs(&md), md.len() as i64);
            if let Some(cache) = &cache {
                if let Ok(Some(row)) = cache.get(&ext_id, p) {
                    if row.mtime == mtime && row.size == size {
                        rows.push((ext_id.clone(), p.clone(), mtime, size, row.label));
                        continue;
                    }
                }
            }
            let head = maybe_head(&host.spec().capabilities, p);
            let params = build_classify_params(p, md.len(), head.as_deref());
            match host.call("classify", &params) {
                Ok(result) => {
                    if let Some(label) = classify_label_of_result(&result) {
                        rows.push((ext_id.clone(), p.clone(), mtime, size, label));
                    }
                }
                Err(CallError::Timeout | CallError::Crashed(_) | CallError::Backoff) => break,
                Err(CallError::Protocol(_)) => continue,
            }
        }
        // 在途标志**整批**清（含已落账的——它们之后由 needs_classify 的「有答案」挡住）。
        // break 掉的那批不清的话，这些路径这辈子都不会再被派发。
        app.providers().clear_inflight(&group);
        if !rows.is_empty() {
            app.providers().store_labels(&rows);
            app.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// provider 预览（`AppState::preview` 的前置钩子）：路径被声明了 `preview` 方法的
/// 扩展认领时先问插件；任何失败（没答 / 超时 / 崩溃 / `unsupported`）都回落内置预览
/// ——「provider 卡死，内置预览照常」就是这条兜底。
///
/// 协议答的 `image-file` 是插件私有临时目录里的**路径**，由 Mo 的图片渲染器读；
/// 绝不接收位图字节（devlog §5 的硬约束）。
pub(crate) fn provider_preview(app: &AppState, path: &Path) -> Option<mo_preview::Preview> {
    let config = AppState::config_path();
    let ext_id = app.providers().owner_of(path, &config)?;
    let overrides = app.extension_setting_overrides(&ext_id);
    let host = app.providers().host(&ext_id, &config, &overrides)?;
    if !host.spec().methods.iter().any(|m| m == "preview") {
        return None;
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let kind = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
        .map(mo_core::types::preview_of)
    {
        Some(mo_core::types::PreviewClass::Image) => "image",
        Some(mo_core::types::PreviewClass::Pdf) => "pdf",
        Some(mo_core::types::PreviewClass::Markdown) => "markdown",
        Some(mo_core::types::PreviewClass::Json) => "json",
        Some(mo_core::types::PreviewClass::Code) => "code",
        _ => "text",
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let params = serde_json::json!({
        "path": path.to_string_lossy(),
        "name": name,
        "kind": kind,
        "max_bytes": 512 * 1024,
    });
    match host.call("preview", &params) {
        Ok(result) => {
            let mut pv = preview_from_result(&result, size)?;
            pv.title = name;
            Some(pv)
        }
        Err(e) => {
            tracing::debug!(target: "mo_provider",
                "扩展 {ext_id} 的 preview 没答成（{e:?}），回落内置预览");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// base64 的形状：RFC 4648 向量 + padding。协议里只有宿主编码这一半。
    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    /// capability 执行点：授了 `read-contents` 才附 `head_b64`；没授就连字段都不该有
    /// ——「少给」必须以**字段不存在**体现，让插件想猜都没得猜。
    #[test]
    fn head_b64_only_when_capability_granted() {
        let p = Path::new("/tmp/a.srt");
        let with_head = build_classify_params(p, 10, Some(b"hello"));
        assert_eq!(
            with_head["head_b64"],
            serde_json::json!(base64_encode(b"hello"))
        );
        assert_eq!(with_head["name"], serde_json::json!("a.srt"));
        assert_eq!(with_head["size"], serde_json::json!(10));

        let without = build_classify_params(p, 10, None);
        assert!(
            without.get("head_b64").is_none(),
            "没授 read-contents 时连字段都不能出现：{without}"
        );
    }

    /// classify 应答只收非空 `label`；group / icon_key 收下不用；坏 JSON / 缺字段不炸。
    #[test]
    fn classify_result_folds_to_label_only() {
        let ok = serde_json::json!({"label": "字幕", "group": "document", "icon_key": "x"});
        assert_eq!(classify_label_of_result(&ok).as_deref(), Some("字幕"));
        assert_eq!(
            classify_label_of_result(&serde_json::json!({"label": "  "})),
            None
        );
        assert_eq!(
            classify_label_of_result(&serde_json::json!({"group": "d"})),
            None
        );
        assert_eq!(
            classify_label_of_result(&serde_json::json!("不是对象")),
            None
        );
    }

    /// preview 应答折叠：文本类按 kind 落到对应 PreviewKind；image-file 要文件真的在
    /// （不在就回落）；rows / unsupported / 认不出的 kind 一律回落。
    #[test]
    fn preview_result_folds_by_kind() {
        let text = preview_from_result(&serde_json::json!({"kind": "markdown", "text": "# hi"}), 3)
            .expect("markdown 应折出来");
        assert_eq!(text.kind, mo_preview::PreviewKind::Markdown);
        assert_eq!(text.text.as_deref(), Some("# hi"));

        // image-file：路径存在才折，不存在回落。
        let dir = std::env::temp_dir().join(format!("mo-pvd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let img = dir.join("x.png");
        std::fs::write(&img, b"fake").unwrap();
        let got = preview_from_result(
            &serde_json::json!({"kind": "image-file", "path": img.display().to_string()}),
            4,
        )
        .expect("存在的图片路径应折出来");
        assert_eq!(got.kind, mo_preview::PreviewKind::Image);
        assert_eq!(got.image.as_deref(), Some(img.as_path()));
        assert!(
            preview_from_result(
                &serde_json::json!({"kind": "image-file", "path": "/no/such/x.png"}),
                4
            )
            .is_none(),
            "指了个不存在的路径要回落"
        );

        for kind in ["rows", "unsupported", "什么玩意"] {
            assert!(
                preview_from_result(&serde_json::json!({"kind": kind, "text": "x"}), 1).is_none(),
                "{kind} 应回落内置预览"
            );
        }
    }

    /// list 应答的解析（P4）：成形的行照收；缺 id / name 的行跳过（不倒一锅粥）；
    /// path / subtitle 空白串按「没给」处理；icon 字段收下不用（协议天然忽略）。
    /// 变异靶子：解析侧把跳过改成整表放弃 / 把空白串照收。
    #[test]
    fn list_rows_parse_skips_malformed_rows() {
        let result = serde_json::json!({
            "rows": [
                { "id": "r1", "name": "行一", "subtitle": "副标题", "icon": "star" },
                { "id": "  ", "name": "空白id" },
                { "id": "r3", "name": "" },
                { "name": "没有id" },
                { "id": "r5", "name": "行五", "path": "/tmp/x.p4x", "subtitle": "  " }
            ]
        });
        let rows = list_rows_from_result(&result);
        assert_eq!(rows.len(), 2, "5 行进 2 行出：3 行坏的跳过");
        assert_eq!(rows[0].id, "r1");
        assert_eq!(rows[0].name, "行一");
        assert_eq!(rows[0].subtitle.as_deref(), Some("副标题"));
        assert!(rows[0].path.is_none());
        assert_eq!(rows[1].id, "r5");
        assert_eq!(rows[1].path, Some(PathBuf::from("/tmp/x.p4x")));
        assert!(rows[1].subtitle.is_none(), "空白 subtitle 按没给处理");

        // 缺 rows / rows 不是数组 → 空表。
        assert!(list_rows_from_result(&serde_json::json!({})).is_empty());
        assert!(list_rows_from_result(&serde_json::json!({ "rows": "nope" })).is_empty());
        // 入参只带 source（query / cursor 本轮不发）。
        assert_eq!(
            list_params("recent"),
            serde_json::json!({ "source": "recent" })
        );
    }

    /// capability 执行点（门函数）：授了 read-contents 才读；没授权时文件就在那儿
    /// 也不许打开——「少给」不是字段留空，是读都没读。变异靶子：把 any 判断去掉。
    #[test]
    fn head_is_read_only_with_read_contents_capability() {
        let dir = std::env::temp_dir().join(format!("mo-pvcap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("a.p3x");
        std::fs::write(&f, b"HEAD-CONTENT").unwrap();

        assert!(maybe_head(&[], &f).is_none(), "没授权：不读");
        assert!(
            maybe_head(&["write".to_string(), "net".to_string()], &f).is_none(),
            "别的授权不顶替 read-contents"
        );
        assert_eq!(
            maybe_head(&["read-contents".to_string()], &f).as_deref(),
            Some(b"HEAD-CONTENT".as_slice()),
            "授权了才读，且读到的是文件头"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 退避账：连续失败计数、达到阈值后短路；一次成功清零。
    #[test]
    fn backoff_shortens_to_disabled_after_three_failures() {
        let spec = SpawnSpec {
            ext_id: "t".into(),
            argv: vec!["true".into()],
            dir: PathBuf::from("."),
            methods: vec!["classify".into()],
            capabilities: vec![],
            covered_exts: vec![],
            log_path: std::env::temp_dir().join("mo-pv-test.log"),
            startup_timeout: Duration::from_millis(100),
            call_timeout: Duration::from_millis(100),
            settings: Default::default(),
        };
        let host = Host::new(spec);
        assert!(host.disabled_until().is_none());
        for _ in 0..(BACKOFF_AFTER - 1) {
            host.record(CallError::Timeout);
        }
        assert!(host.disabled_until().is_none(), "没到阈值不该停用");
        host.record(CallError::Timeout);
        let (until, fails, reason) = host.disabled_until().expect("第三次失败应进退避");
        assert!(fails >= BACKOFF_AFTER);
        assert!(until > Instant::now(), "停用要落在未来");
        assert!(!reason.is_empty(), "要带上原因");
        // 退避期内调用直接短路，不碰进程。
        assert_eq!(
            host.call("classify", &serde_json::json!({})),
            Err(CallError::Backoff)
        );
        // 成功清零（内部方法，测试用）。
        host.record_clear();
        assert!(host.disabled_until().is_none());
    }
}
