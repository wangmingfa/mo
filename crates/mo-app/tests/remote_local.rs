//! 本地位置入口，以及**远程连接的生命周期**。
//!
//! 三件事在这里被钉住：
//!
//! 1. **本地位置必须走 `open_local`**。用户报的 bug：连上 FTP 后点侧边栏「快捷访问」
//!    没反应。根因不是点击没触发，而是那条点击走的是 `open_directory`——它用**当前
//!    生效的后端**（远程），拿 `/Users/…` 去 FTP 上读必然失败；错误又被调用方
//!    `let _ =` 丢掉，界面就一动不动。
//! 2. **切到本地目录不断开远程连接**。`open_local` 只把「看哪边」切回本地，连接留着；
//!    切回来（`open_connection` / 再连同一台服务器）不重新登录、不重建 socket，
//!    并且回到上次待过的目录。
//! 3. **关标签页也不断开**——用户后来纠正过一次：「关闭标签页不会断开已经连接的
//!    服务器，只有退出应用才会断开」。所以连接活在进程级 `SessionRegistry` 里，
//!    比 `AppState` 活得久；终点只有显式断开（`disconnect_connection`）与进程退出。
//!
//! 4. **闲置被服务器掐断的连接会自愈**。用户报的现象：连上 FTP、过一阵子切回去，
//!    弹 `Broken pipe (os error 32)`，然后再也回不去。根因是「会话活着」与「连接
//!    还能用」被当成了同一件事——FTP 服务器会把闲置的控制连接单方面关掉。现在读目录
//!    之前先探活（`FileSystem::is_alive`），断了就用会话里记着的凭据**原地重连**。
//! 5. **切换失败要整体回滚**。失败时若只翻了「看哪边」而目录没换成功，就会留下
//!    「标签页徽标说 FTP、列表还是本地那份」的半切换态——侧边栏两边同时高亮
//!    （用户报的「关闭弹窗后左侧选中了 2 个项目」）。
//! 6. **同端点同用户名只占一行**。用户报的现象：闲置断线后从认证框重新填了密码，
//!    侧边栏「远程」区长出了两行同一台服务器——旧那行已经死了，点它只会继续失败。
//!    根因是复用判据带了密码，密码一变就被当成一条新连接。现在判据是「端点 +
//!    用户名」，密码变了就在**原来那个编号**上把连接与凭据换掉。
//! 7. **「是不是目录」只问列表模型**。远程条目的路径（`/1`）在本机不存在，
//!    `Path::is_dir()` 会把远程目录判成文件，双击就被交给系统 `open`——日志里
//!    `The file /1 does not exist.`，界面一动不动。判据取当前列表那一行的 `kind`。
//! 8. **文件操作按「这条路径属于哪个后端」分流**。删除 / 重命名 / 新建若一律走本机
//!    管线，远程条目那条路径在本机不存在，结果就是「成功」地什么都不做；反过来把
//!    本地路径当远程发给服务器更糟。判据是「它是不是当前列表里那一行」，**不是**
//!    「我现在在看远程吗」——去重 / 同步会拿着本地路径调同一批 API。
//! 9. **传输的端点由「拥有那一头的那一页」回答**。第 8 条那个判据答不出传输需要的
//!    东西：粘贴到当前目录时 `dest` 自己不是列表里的行，分栏拖拽时目标那一头根本
//!    不在源 `AppState` 的列表里。所以两端走 [`Endpoint`]（本地 / 那条会话），
//!    UI 侧源窗格与落点窗格各问各的。远程有一端时走 `TransferOperation`，
//!    并**刻意不**记可逆项（撤销模型里的路径都是本地路径）。
//!
//! 为什么守卫落在 `mo-app` 而不是 UI 层：headless 的 GPUI 测试调度器会把「后台
//! tokio 线程唤醒测试任务」判成不确定性直接 panic（点击回调最终 `await` 到
//! `spawn_blocking`），UI 层写不出这条路径的自动化测试。见 `verify-gpui-layout-headless`。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mo_app::{AppState, ConnectFailure, Endpoint, SessionRegistry};
use mo_core::{EntryKind, FileId, FileMetadata, MoError};
use mo_fs::{FileSystem, ReadDirEntry};
use mo_remote::RemoteUrl;

/// 测试用的远程地址。
///
/// 刻意用 `127.0.0.1:1`：**万一复用的短路失效了**、真的去建连接，本机端口 1
/// 会立刻拒绝（而不是把测试卡在一条 SYN 上等超时），用例就会干脆地红掉。
const TEST_URL: &str = "ftp://127.0.0.1:1";

/// 假后端「被问过哪些路径」的记录。
type AskedLog = Arc<Mutex<Vec<PathBuf>>>;

/// 假后端「收到哪些改动调用」的记录，形如 `remove_file:/x`。
type OpLog = Arc<Mutex<Vec<String>>>;

/// 假远程后端的健康状况。
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Health {
    /// 连接好的。
    #[default]
    Ok,
    /// 探活说「断了」——闲置被服务器掐掉的样子。
    Dead,
    /// 探活说「活着」，但一读目录就断——网络在两次操作之间抖掉的样子。
    DiesWhileReading,
}

/// 用户报的那个错误的等价物：`mo_remote` 把「连接断了」标成这个标记（判据见
/// `mo_remote::is_disconnected`），上层据此决定重连重试。
fn disconnected_error() -> MoError {
    mo_remote::RemoteError::disconnected("列目录", "Broken pipe (os error 32)").into()
}

/// 只认识 `/` 与 `/pub` 的假远程后端：其它路径一律「远端没有这样的目录」，
/// 并记下被问过的路径——用来证明「切回来时读的是哪台机器的哪个目录」。
struct FakeRemoteFs {
    asked: AskedLog,
    /// 探活 / 读目录时的行为，见 [`Health`]。
    health: Health,
    /// 列目录里那一条文件的名字。重连返回的新后端会换个名字，好断言「后面那次读
    /// 确实走了新连接」。
    marker: String,
    /// 额外多列一条**目录**（名字自定）。默认 `None`——只有「目录判据来自列表」
    /// 那条守卫需要它：要一个「列表里说是目录、本机磁盘上却不存在」的条目。
    extra_dir: Option<String>,
    /// 收到的**改动类**调用（删除 / 重命名 / 建目录），形如 `remove_file:/x`。
    ///
    /// 有两个用处：① 断言「这个操作真的走了远程后端，而不是本机管线」；
    /// ② 让列表反映改动（`read_dir_blocking` 据此过滤 / 改名），于是「远程没有
    /// watcher，删完必须重读」也是可断言的。
    log: OpLog,
}

impl Default for FakeRemoteFs {
    fn default() -> Self {
        Self {
            asked: Arc::new(Mutex::new(Vec::new())),
            health: Health::Ok,
            marker: "remote.txt".to_string(),
            extra_dir: None,
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl FakeRemoteFs {
    fn unsupported() -> MoError {
        MoError::Other("假远程后端不支持这个操作".to_string())
    }
}

#[async_trait]
impl FileSystem for FakeRemoteFs {
    async fn read_dir(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.read_dir_blocking(path)
    }

    fn is_alive(&self) -> bool {
        self.health != Health::Dead
    }

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.asked.lock().unwrap().push(path.to_path_buf());
        if self.health == Health::DiesWhileReading {
            return Err(disconnected_error());
        }
        if self.health == Health::Dead {
            // 探活已经说过「断了」：这时候绝不该再拿这条连接去读。谁读到这儿，
            // 就说明「闲置先探活」这一步被绕过了。
            panic!("连接已死，探活之后不该再拿它读目录");
        }
        match path.to_str() {
            Some("/") | Some("/pub") => {
                let mut entries = vec![ReadDirEntry::new(
                    FileId::synthetic(Path::new(&self.marker)),
                    self.marker.clone(),
                    EntryKind::File,
                    PathBuf::from(format!("/{}", self.marker)),
                )];
                if let Some(dir) = &self.extra_dir {
                    entries.push(ReadDirEntry::new(
                        FileId::synthetic(Path::new("/extra-dir")),
                        dir.clone(),
                        EntryKind::Directory,
                        PathBuf::from(format!("/{dir}")),
                    ));
                }
                Ok(Self::apply_log(entries, &self.log.lock().unwrap()))
            }
            other => Err(MoError::Other(format!(
                "远端没有这样的目录：{}",
                other.unwrap_or_default()
            ))),
        }
    }

    /// 只认**自己建出来**的路径（`create_dir` / `write_file` 记下的那些）。
    ///
    /// 用来验证「当前列表翻不到那一页时，判重会去问后端」这条兜底路径。
    async fn metadata(&self, path: &Path) -> Result<FileMetadata, MoError> {
        let wanted = path.to_string_lossy().to_string();
        let known = self.log.lock().unwrap().iter().any(|op| {
            matches!(op.strip_prefix("create_dir:"), Some(p) if p == wanted.as_str())
                || matches!(op.strip_prefix("write_file:"), Some(p) if p == wanted.as_str())
        });
        if known {
            Ok(FileMetadata {
                size: 0,
                modified: None,
                created: None,
                permissions: mo_core::Permissions::default(),
            })
        } else {
            Err(Self::unsupported())
        }
    }

    async fn create_dir(&self, path: &Path) -> Result<(), MoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("create_dir:{}", path.display()));
        Ok(())
    }

    async fn write_file(&self, path: &Path, _contents: &[u8]) -> Result<(), MoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("write_file:{}", path.display()));
        Ok(())
    }

    /// 只会「读」它列表里那一条（内容固定 `from-remote`），其余路径照旧不支持。
    ///
    /// 跨端点传输的下载侧要它：没有这一条，「远程 → 本机」那一半根本走不通。
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, MoError> {
        if path == Path::new(&format!("/{}", self.marker)) {
            Ok(b"from-remote".to_vec())
        } else {
            Err(Self::unsupported())
        }
    }

    async fn remove_file(&self, path: &Path) -> Result<(), MoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("remove_file:{}", path.display()));
        Ok(())
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), MoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("remove_dir:{}", path.display()));
        Ok(())
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), MoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("rename:{}->{}", from.display(), to.display()));
        Ok(())
    }
}

impl FakeRemoteFs {
    /// 让列表反映已经发生的改动：删掉的不再列出，改名的按新名列出。
    fn apply_log(entries: Vec<ReadDirEntry>, log: &[String]) -> Vec<ReadDirEntry> {
        let mut out = Vec::new();
        for e in entries {
            // 改名：以**最后一次** rename 为准。
            let renamed = log.iter().rev().find_map(|op| {
                let rest = op.strip_prefix("rename:")?;
                let (from, to) = rest.split_once("->")?;
                (from == e.path.to_string_lossy()).then(|| to.to_string())
            });
            let path = match renamed {
                Some(to) => PathBuf::from(to),
                None => e.path.clone(),
            };
            let gone = log.iter().any(|op| {
                matches!(op.strip_prefix("remove_file:"), Some(p) if p == path.to_string_lossy())
                    || matches!(op.strip_prefix("remove_dir:"), Some(p) if p == path.to_string_lossy())
            });
            if gone {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push(ReadDirEntry::new(e.id, name, e.kind, path));
        }
        out
    }
}

/// 一个真实的本地目录（里面有一个文件，用来验证「真的读出来了」）。
fn local_tree(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("mo-remote-local-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("local.txt"), b"x").unwrap();
    base
}

/// 一个「标签页」：用给定的会话表。
///
/// 传一份独占的表 = 测试隔离；两个 `AppState` 传同一个 `Arc` = 两个标签页共享同一批
/// 连接——后者正是「关标签页不断开」的验证方式。
fn tab(sessions: &Arc<SessionRegistry>) -> AppState {
    let trash = std::env::temp_dir().join(format!("mo-trash-{}", std::process::id()));
    AppState::with_sessions(trash, sessions.clone())
}

/// 登入一条假连接，返回 `(编号, 被问过的路径, 改动记录)`。
fn connect_fake_at(app: &AppState, url: &str) -> (u64, AskedLog, OpLog) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    app.install_backend_for_test(
        Arc::new(FakeRemoteFs {
            asked: asked.clone(),
            log: log.clone(),
            ..Default::default()
        }),
        url,
    );
    // 刚装上的那条就是当前生效的（最后一个编号）。
    let id = app
        .live_connections()
        .last()
        .expect("应当刚登入一条连接")
        .id;
    (id, asked, log)
}

fn connect_fake(app: &AppState) -> Arc<Mutex<Vec<PathBuf>>> {
    connect_fake_at(app, TEST_URL).1
}

/// 登入一条「有毛病」的假连接（探活说断 / 一读就断），返回它的编号。
fn connect_sick(app: &AppState, url: &str, health: Health) -> u64 {
    app.install_backend_for_test(
        Arc::new(FakeRemoteFs {
            health,
            ..Default::default()
        }),
        url,
    );
    app.live_connections()
        .last()
        .expect("应当刚登入一条连接")
        .id
}

/// 「重连」用的建连接实现：记下被调用次数，每次返回一条**新的健康**连接。
///
/// 新连接列出来的文件名带 `fresh-<n>`，于是「后面那次读真的走了新连接」是可断言的
/// ——只断言「重连被调用过」会漏掉「换了连接却还读旧的那份」。
fn counting_connector() -> (Arc<AtomicUsize>, mo_app::Connector) {
    let calls = Arc::new(AtomicUsize::new(0));
    let connector = {
        let calls = calls.clone();
        Arc::new(move |_url: &RemoteUrl| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(FakeRemoteFs {
                marker: format!("fresh-{n}.txt"),
                ..Default::default()
            }) as Arc<dyn FileSystem>)
        })
    };
    (calls, connector)
}

/// 一个「重连也失败」的建连接实现。
///
/// 收的是「现造一个错误」而不是错误本身：`RemoteError` 不是 `Clone`（也没必要为了
/// 测试去给它加一条派生）。
fn failing_connector(make_error: fn() -> mo_remote::RemoteError) -> mo_app::Connector {
    Arc::new(move |_url: &RemoteUrl| Err(make_error()))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

#[test]
fn local_locations_need_open_local_while_a_remote_is_connected() {
    let dir = local_tree("quick");
    let app = tab(&Arc::new(SessionRegistry::new()));
    connect_fake(&app);

    runtime().block_on(async {
        // 连上之后停在远程根。
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程根");
        assert!(app.browsing_remote(), "前置条件：应当正在浏览远程");

        // ① 老行为：直接 `open_directory(本地路径)`。这就是用户点快捷访问时发生的事——
        //    拿本地路径去远程后端读，必然失败。
        let err = app
            .open_directory(&dir)
            .await
            .expect_err("本地路径不该能在远程后端上打开——这正是「点了没反应」的来源");
        assert!(
            err.to_string().contains("远端没有"),
            "报错应来自假远程后端：{err}"
        );

        // ② 正确行为：`open_local(本地路径)`。切回本地，然后真的读出本地目录。
        app.open_local(&dir).await.expect("本地位置应当能打开");
        assert!(
            !app.browsing_remote(),
            "打开本地位置后，当前看的应当是本地（地址栏 / 标签页徽标据此变化）"
        );
        assert!(
            app.remote_url().is_none(),
            "正在看本地时不该有「当前远程地址」"
        );
        assert_eq!(
            app.current_path().await.as_deref(),
            Some(dir.as_path()),
            "应当落在本地目标目录里"
        );
        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "local.txt"),
            "本地目录应当真的读出来了"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// 切到本地**不断开**连接；切回远程复用同一条连接，并回到上次的目录。
#[test]
fn switching_to_a_local_dir_keeps_the_session_alive() {
    let dir = local_tree("switch");
    let app = tab(&Arc::new(SessionRegistry::new()));
    let asked = connect_fake(&app);

    runtime().block_on(async {
        app.open_directory(Path::new("/pub"))
            .await
            .expect("进入远程 /pub");
        assert!(app.browsing_remote());

        // 切到本地：只切走，不断开。
        app.open_local(&dir).await.expect("本地位置应当能打开");
        assert_eq!(
            app.live_connections().len(),
            1,
            "切到本地目录不该丢掉连接——否则用户逛一圈回来就得重新登录"
        );
        assert!(!app.browsing_remote(), "但当前看的是本地");

        // 切回远程：不重新登录、不重建 socket，还回到 /pub。
        app.open_remote().await.expect("应当能切回活着的连接");
        assert!(app.browsing_remote());
        assert_eq!(
            app.current_path().await.as_deref(),
            Some(Path::new("/pub")),
            "切回来应当回到上次待过的远程目录，而不是掉回根"
        );
        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "remote.txt"),
            "应当真的从假远程后端读出了 /pub 的内容"
        );

        // 再连一次同一台服务器（地址栏敲 `ftp://主机:端口/路径`）也必须复用：
        // `finish_connect` 在这里短路进 `use_session`。若短路失效就会真去连
        // 127.0.0.1:1 → 立刻被拒 → 这个 `expect` 失败。
        app.connect_remote(&format!("{TEST_URL}/"))
            .await
            .expect("同一台服务器应当复用活着的连接，而不是重新登录");
        assert!(app.browsing_remote());
        assert_eq!(app.live_connections().len(), 1, "复用不该在表里多留一条");

        // 复用是真的走了假后端：`/` 被问过。
        assert!(
            asked.lock().unwrap().iter().any(|p| p == Path::new("/")),
            "复用后仍应使用原来那条连接的后端"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// **关标签页不断开连接**——用户纠正过的语义：只有退出应用才断开。
///
/// 两个 `AppState` 共享同一张会话表 = 两个标签页共用同一批连接。关掉其中一个
/// （停泵 + 丢掉那一份）之后，连接必须还在，另一个标签页还能直接切过去、回到
/// 上次待过的目录。
#[test]
fn closing_a_tab_keeps_the_connection_alive() {
    let dir = local_tree("close");
    let sessions = Arc::new(SessionRegistry::new());
    let tab_a = tab(&sessions);
    let tab_b = tab(&sessions);
    connect_fake(&tab_a);

    runtime().block_on(async {
        tab_a
            .open_directory(Path::new("/pub"))
            .await
            .expect("进入远程 /pub");

        // 新标签页天然看得见已连着的服务器（表是共享的）。
        let live = tab_b.live_connections();
        assert_eq!(live.len(), 1, "另一个标签页应当看得见这条连接");
        let id = live[0].id;

        // 「关标签页」= 停掉它的后台泵 + 丢掉这一份 `AppState`。
        // 连接不在它身上，所以这一步之后连接照旧——这正是用户要的。
        tab_a.stop_pumps();
        drop(tab_a);

        assert_eq!(
            tab_b.live_connections().len(),
            1,
            "关标签页不该断开连接：只有退出应用才断开"
        );
        tab_b
            .open_connection(id)
            .await
            .expect("另一个标签页应当能直接切到这条连接（不重新登录）");
        assert!(tab_b.browsing_remote());
        assert_eq!(
            tab_b.current_path().await.as_deref(),
            Some(Path::new("/pub")),
            "切过去应当回到它上次待过的远程目录"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// 断开是**按条**的：断一条不动另一条，断到自己在看的那条才回落本地。
#[test]
fn disconnecting_one_connection_leaves_the_others() {
    let dir = local_tree("multi");
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (first, _, _) = connect_fake_at(&app, "ftp://127.0.0.1:1");
    let (second, _, _) = connect_fake_at(&app, "ftp://127.0.0.1:2");

    runtime().block_on(async {
        // 切到第一条，并让它在 /pub 留下位置。
        app.open_connection(first).await.expect("切到第一条");
        app.open_directory(Path::new("/pub"))
            .await
            .expect("进入远程 /pub");

        // 断开「另一条」：当前看的这条不受影响。
        app.disconnect_connection(second).await.expect("断开另一条");
        assert_eq!(app.live_connections().len(), 1, "只该少一条");
        assert!(app.browsing_remote(), "断的不是当前这条，浏览态不变");
        assert_eq!(app.active_connection_id(), Some(first));

        // 断开当前这条：切回本地，且旧编号再也切不回去。
        app.disconnect_connection(first)
            .await
            .expect("断开当前这条");
        assert!(app.live_connections().is_empty());
        assert!(!app.browsing_remote(), "断开当前这条后应当回到本地");
        assert!(
            app.open_connection(second).await.is_err(),
            "已经断开的编号不该还能切过去"
        );
        assert!(
            app.open_connection(first).await.is_err(),
            "刚断开的编号同样不该还能切过去"
        );

        // 后端回落本地：拿本地路径读得动（对比 ① 里同样的调用在远程下必然失败）。
        app.open_directory(&dir)
            .await
            .expect("断开后应当回落本地后端");
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// 命令面板的「断开远程连接」断的是**本标签页当前那条**。
#[test]
fn disconnect_remote_drops_the_session() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    connect_fake(&app);

    runtime().block_on(async {
        app.open_directory(Path::new("/pub")).await.expect("/pub");
        assert!(app.browsing_remote());

        app.disconnect_remote().await.expect("断开应当成功");

        assert!(app.live_connections().is_empty(), "断开后不该还有连接");
        assert!(!app.browsing_remote(), "断开后应当回到本地浏览");
    });
}

/// 闲置被服务器掐断的连接，切回来时会**静默重连**——用户看不到 `Broken pipe`。
///
/// 这就是用户报的那条：连上 FTP → 切去本地干活 → 过一阵子切回来 → 弹报错。
/// 服务器早把闲置的控制连接关了，而「会话活着」并不等于「连接还能用」。
#[test]
fn an_idle_connection_is_revived_before_it_is_used() {
    let (calls, connector) = counting_connector();
    let sessions = Arc::new(SessionRegistry::with_connector(connector));
    let app = tab(&sessions);
    let id = connect_sick(&app, TEST_URL, Health::Dead);
    // 模拟「闲置了一阵子」。真实场景里这是几分钟，测试里把时刻直接往前拨。
    sessions.age_for_test(id, Duration::from_secs(3600));

    runtime().block_on(async {
        app.open_connection(id)
            .await
            .expect("闲置被掐断的连接应当自动重连，而不是把 Broken pipe 弹给用户");

        assert_eq!(calls.load(Ordering::SeqCst), 1, "应当恰好重连一次");
        assert!(app.browsing_remote(), "重连后应当就在这条连接上");
        assert_eq!(
            app.live_connections().len(),
            1,
            "重连是**原地**替换连接对象，不该在侧边栏多留一行"
        );
        assert_eq!(
            app.active_connection_id(),
            Some(id),
            "编号不变——侧边栏的高亮不该跳"
        );
        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "fresh-0.txt"),
            "重连之后必须真的用**新**连接去读（换了连接还在读旧的那份等于没修）"
        );
    });
}

/// 探活说「活着」、一读就断（网络在两次操作之间抖掉）：重连后再读一次，
/// 用户同样看不到报错。守卫 `load_path` 里那个「最多两轮」的循环。
#[test]
fn a_connection_that_dies_mid_read_is_retried_once() {
    let (calls, connector) = counting_connector();
    let sessions = Arc::new(SessionRegistry::with_connector(connector));
    let app = tab(&sessions);
    let id = connect_sick(&app, TEST_URL, Health::DiesWhileReading);

    runtime().block_on(async {
        app.open_connection(id)
            .await
            .expect("读的瞬间断线应当重连后重试一次");

        assert_eq!(calls.load(Ordering::SeqCst), 1, "应当恰好重连一次");
        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "fresh-0.txt"),
            "重试必须走新连接"
        );
    });
}

/// 重连被服务器拒（比如闲置期间那边改了密码）：错误要能让 UI 弹认证框，
/// 而不是干显示一句话。
#[test]
fn a_rejected_reconnect_asks_for_credentials() {
    let sessions = Arc::new(SessionRegistry::with_connector(failing_connector(|| {
        mo_remote::RemoteError::auth("登录", "530 Login incorrect.")
    })));
    let app = tab(&sessions);
    let id = connect_sick(&app, TEST_URL, Health::Dead);
    sessions.age_for_test(id, Duration::from_secs(3600));

    runtime().block_on(async {
        match app.open_connection(id).await {
            Err(ConnectFailure::NeedsCredentials { endpoint, .. }) => {
                assert_eq!(endpoint, TEST_URL)
            }
            other => panic!("应当报「需要凭据」（UI 据此弹认证框），实际：{other:?}"),
        }
    });
}

/// 切换失败要**整体回滚**，不能留下「标签页徽标说 FTP、列表还是本地那份」的半切换态。
///
/// 用户报的「关闭弹窗之后左侧选中了 2 个项目」：`on_remote` 已经翻了、目录却没换成，
/// 于是侧边栏按「当前路径」高亮的那一项和按「当前连接」高亮的那一项同时亮着。
#[test]
fn a_failed_switch_rolls_the_browsing_state_back() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    connect_fake(&app);

    runtime().block_on(async {
        app.open_directory(Path::new("/pub"))
            .await
            .expect("先待在远程 /pub");
        let entries_before = app.current_entries().await.len();

        // 切本地失败：本地根本没有这层目录。
        let missing = std::env::temp_dir().join("mo-nonexistent-here/xyz");
        app.open_local(&missing)
            .await
            .expect_err("不存在的本地路径应当失败");

        assert!(
            app.browsing_remote(),
            "失败后应当还在原来那条连接上（否则徽标 / 地址栏 / 侧边栏高亮会互相矛盾）"
        );
        assert_eq!(
            app.current_path().await.as_deref(),
            Some(Path::new("/pub")),
            "位置不该变"
        );
        assert_eq!(
            app.current_entries().await.len(),
            entries_before,
            "列表还是原来那份"
        );
        assert_eq!(
            app.remote_url().map(|u| u.endpoint()).as_deref(),
            Some(TEST_URL),
            "地址栏仍应显示这台服务器"
        );
    });
}

/// 同端点同用户名在侧边栏**只占一行**。
///
/// 用户报的「左侧选中了 2 个项目」里那条连接就是多出来的：重新走一遍连接（地址栏
/// 再敲一次 / 认证框重填）不该在「远程」区长出第二行。
#[test]
fn the_same_account_never_takes_a_second_row() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (id, asked, _) = connect_fake_at(&app, "ftp://alice@127.0.0.1:1");

    runtime().block_on(async {
        app.connect_remote("ftp://alice@127.0.0.1:1")
            .await
            .expect("同端点同用户名应当就在原来那条上收场（真去连 127.0.0.1:1 会立刻被拒）");

        let live = app.live_connections();
        assert_eq!(live.len(), 1, "同端点同用户名只该有一行");
        assert_eq!(live[0].id, id, "而且就是原来那个编号——侧边栏那一行不新增");
        assert_eq!(app.active_connection_id(), Some(id));
        assert!(
            asked.lock().unwrap().iter().any(|p| p == Path::new("/")),
            "复用应当真的用原来那条连接的后端去读"
        );
    });
}

/// 同一台服务器上换了密码（认证框重填一次）：新连接**接在原来那个编号上**。
///
/// 这条守卫的是用户报的那个「两行」：判据只要还带着密码，密码一变就会既留下旧
/// 那条（已经死了的），又新建一条——点旧的那条继续失败，怎么点都回不去。
#[test]
fn a_new_password_reconnects_the_same_row() {
    let (calls, connector) = counting_connector();
    let sessions = Arc::new(SessionRegistry::with_connector(connector));
    let app = tab(&sessions);
    // 先用「旧密码那套」登入（假后端，避免真建 socket）。
    let (id, _, _) = connect_fake_at(&app, "ftp://alice@127.0.0.1:1");

    runtime().block_on(async {
        app.connect_remote_with_credentials("ftp://127.0.0.1:1", "alice", "pw2")
            .await
            .expect("换了密码应当真连一次");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "换了凭据必须真连，不能拿旧连接凑合"
        );
        let live = app.live_connections();
        assert_eq!(live.len(), 1, "同端点同用户名仍然只有一行");
        assert_eq!(
            live[0].id, id,
            "编号不变——侧边栏那一行不新增、不闪、高亮不跳"
        );
        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.name == "fresh-0.txt"),
            "换完之后必须真的用新连接去读"
        );

        // 新凭据记进了会话：再连一次（地址里不带密码 = 走钥匙串 / 匿名那条）应当
        // 直接复用，而不是又连一次、又添一行。
        app.connect_remote("ftp://alice@127.0.0.1:1")
            .await
            .expect("应当复用刚换过凭据的那条会话");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "复用不该再连一次（凭据没记进会话的话这里会连第二次）"
        );
        assert_eq!(app.live_connections().len(), 1);
    });
}

/// 不变式兜底：表里同 key（端点 + 用户名）已经有条目时，再登入一条也会把旧的
/// 摘掉——这条走的是 `add` 本身（测试用的登入接口绕过 `find` 的复用短路）。
#[test]
fn installing_the_same_account_twice_keeps_one_row() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (_, _, _) = connect_fake_at(&app, "ftp://alice@127.0.0.1:1");
    let (second, _, _) = connect_fake_at(&app, "ftp://alice@127.0.0.1:1");

    let live = app.live_connections();
    assert_eq!(live.len(), 1, "同 key 的旧条目应当被摘掉，不留两行");
    assert_eq!(live[0].id, second, "留下的应当是刚登入的那条");
}

/// 同端点但**账号不同**仍然是两行：一行一条连接，两个账号就是两条连接。
#[test]
fn a_second_account_on_the_same_host_keeps_its_own_row() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (alice, _, _) = connect_fake_at(&app, "ftp://alice@127.0.0.1:1");
    let (bob, _, _) = connect_fake_at(&app, "ftp://bob@127.0.0.1:1");

    assert_ne!(alice, bob, "两个账号是两条连接，不该互相顶掉");
    assert_eq!(
        app.live_connections().len(),
        2,
        "同一台服务器的两个账号应当各占一行"
    );
}

/// 「是不是目录」来自**列表模型**（`entry.kind`），而不是本机磁盘。
///
/// 用户报的「双击 FTP 里的目录没反应，日志里 `The file /1 does not exist.`」：
/// 远程条目的路径（`/1`）在本机根本不存在，`Path::is_dir()` 把远程目录一律判成
/// 文件，双击就把它交给系统 `open` 去开一个本地不存在的路径。这条守卫钉住判据的
/// 出处：模型说目录就是目录——哪怕本机磁盘上根本没有这个路径。
#[test]
fn whether_an_entry_is_a_directory_comes_from_the_listing() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let dir_name = "远端目录";
    app.install_backend_for_test(
        Arc::new(FakeRemoteFs {
            extra_dir: Some(dir_name.to_string()),
            ..Default::default()
        }),
        TEST_URL,
    );

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");

        let remote_dir = PathBuf::from(format!("/{dir_name}"));
        assert!(
            !remote_dir.is_dir(),
            "前提：这个远程目录在本机不存在——否则这条测试测不出两种判据的差别"
        );
        assert_eq!(
            app.entry_is_dir(&remote_dir).await,
            Some(true),
            "列表里它是目录，entry_is_dir 就得说目录（旧的 Path::is_dir() 在这里给 false）"
        );
        assert_eq!(
            app.entry_is_dir(Path::new("/remote.txt")).await,
            Some(false),
            "列表里是文件就得说文件"
        );
        assert_eq!(
            app.entry_is_dir(Path::new("/不在列表里")).await,
            None,
            "不在当前列表里的路径返回 None，交给调用方兜底"
        );
    });
}

/// 删掉一个**远程**条目：走远程后端的删除，**不进本机回收站**。
///
/// 原来一律走 `TrashOperation`（本机回收站）：远程路径在本机不存在，那条操作要么
/// 报错，要么「成功」而服务端文件还在——用户看到的就是「删了，刷新还在」。
#[test]
fn deleting_a_remote_entry_goes_through_the_backend() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (_, _, log) = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");
        let victim = PathBuf::from("/remote.txt");

        app.delete_paths(vec![victim.clone()])
            .await
            .expect("远程删除应当走后端成功");

        assert!(
            log.lock()
                .unwrap()
                .contains(&"remove_file:/remote.txt".to_string()),
            "删除必须发给远程后端（走本机管线的话这里一条记录都没有）"
        );
        assert!(
            app.trash_list().is_empty(),
            "远程条目不该进本机回收站——回收站是本机概念，远端也没有「还原」这回事"
        );
        // 远程没有 watcher：删完必须重读，否则列表里还留着已删除的条目。
        assert!(
            !app.current_entries().await.iter().any(|e| e.path == victim),
            "删完之后列表里不该还有它（说明没重读）"
        );
    });
}

/// 在看远程时删一个**本地**文件：照旧进本机回收站，**不该**发给服务器。
///
/// 这条是给「按路径分流」钉边界的：判据若是「我现在在看远程吗」而不是「这条路径
/// 属于哪个后端」，去重 / 同步拿着本地路径调过来时就会被发到服务器上。
#[test]
fn deleting_a_local_file_while_browsing_remote_still_uses_the_trash() {
    let dir = local_tree("remote-local-delete");
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (_, _, log) = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");
        assert!(app.browsing_remote(), "前提：当前在看远程");

        let victim = dir.join("local.txt");
        app.delete_paths(vec![victim.clone()])
            .await
            .expect("本地删除");

        assert!(
            log.lock().unwrap().is_empty(),
            "本地文件不该被发给远程后端——它不是远程列表里的条目"
        );
        for _ in 0..200 {
            if !app.trash_list().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            app.trash_list().iter().any(|t| t.original == victim),
            "本地删除应当照旧进本机回收站（可撤销）"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// 重命名一个**远程**条目：走远程后端的 `rename`。
///
/// 本机 `RenameOperation` 拿到 `/remote.txt` 这种路径只会「成功」地什么都不做——
/// 那个路径在本机不存在，而操作队列又不报错。
#[test]
fn renaming_a_remote_entry_uses_the_backend() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (_, asked, log) = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");
        let reads_before = asked.lock().unwrap().len();

        app.rename_many(vec![(
            PathBuf::from("/remote.txt"),
            PathBuf::from("/renamed.txt"),
        )])
        .await
        .expect("远程重命名应当走后端成功");

        assert!(
            log.lock()
                .unwrap()
                .contains(&"rename:/remote.txt->/renamed.txt".to_string()),
            "重命名必须发给远程后端"
        );
        assert!(
            asked.lock().unwrap().len() > reads_before,
            "远程没有 watcher：改完名必须重读一次，否则列表里还是旧名字"
        );
        assert!(
            app.current_entries()
                .await
                .iter()
                .any(|e| e.path == Path::new("/renamed.txt")),
            "重读之后列表里应当是新名字"
        );
    });
}

/// 在远程目录里新建文件夹：`create_dir` 发给远程后端，且名字**不与列表里的重名**。
///
/// 去重判据原来是 `Path::exists()`（本机磁盘），远程路径恒为「不存在」，于是
/// 在已经有同名条目的远程目录里新建会撞服务端的错。
#[test]
fn a_new_folder_in_a_remote_dir_asks_the_backend() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (_, _, log) = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");

        let created = app
            .create_folder(Path::new("/"), "remote.txt")
            .await
            .expect("远程建目录应当走后端成功");
        // `unique_path` 把序号插在扩展名**前**：`/remote.txt` → `/remote 2.txt`。
        assert_eq!(
            created,
            PathBuf::from("/remote 2.txt"),
            "名字与列表里那条冲突，应当去重（本机 exists() 判不出来的重名）"
        );
        assert_eq!(
            log.lock().unwrap().last().cloned(),
            Some("create_dir:/remote 2.txt".to_string()),
            "建目录必须发给远程后端"
        );
    });
}

/// 「判重去问后端」那条兜底：目标目录**不是当前这一页**时（列表查不到），
/// 名字冲突仍然要被查出来。
///
/// 列表判重是零 IO 的快路径，但它只认当前目录；跨目录新建（面包屑跳到别处再建、
/// 或者像这里一样在子目录里建）只能问后端。原来这里是 `Path::exists()`——远程
/// 路径恒「不存在」，于是永远不去重。
#[test]
fn a_new_folder_outside_the_current_page_still_dedupes() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (_, _, _) = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");
        // `/sub` 不是当前列表里的路径：判重必须落到「问后端」。
        let first = app
            .create_folder(Path::new("/sub"), "新建文件夹")
            .await
            .expect("远程建目录");
        assert_eq!(first, PathBuf::from("/sub/新建文件夹"));

        let second = app
            .create_folder(Path::new("/sub"), "新建文件夹")
            .await
            .expect("远程建目录");
        assert_eq!(
            second,
            PathBuf::from("/sub/新建文件夹 2"),
            "第二次应当去重——问后端才知道这个名字已经占了"
        );
    });
}

/// SMB / NFS 走**系统挂载**，不是远程会话。
///
/// 侧边栏因此分两区：Mo 自己建的会话在「远程」（点一下走连接 / 重连），操作系统
/// 挂好的网络盘在「网络」（就是个本地目录）。这条钉住分流本身——`smb://` 若被当成
/// 「不支持的协议」直接报错，或者被当成远程会话去建连接，都是错的方向。
#[test]
fn smb_and_nfs_are_mounted_by_the_system_not_connected() {
    let app = tab(&Arc::new(SessionRegistry::new()));

    // 1. 协议要被放行（不再报「暂不支持」）。
    let parsed = mo_remote::RemoteUrl::parse("smb://nas.local/public").expect("解析应当成功");
    assert!(
        mo_remote::mount::is_mountable(&parsed.scheme),
        "smb 应当走系统挂载那条路"
    );
    assert!(
        mo_remote::mount::is_mountable("nfs"),
        "nfs 应当走系统挂载那条路"
    );
    assert!(
        !mo_remote::mount::is_mountable("ftp"),
        "ftp 是 Mo 自己连的，不该被拿去挂载"
    );

    // 2. 真去挂载会失败（本机 127.0.0.1 上没有 SMB），但**失败必须发生在挂载那一步**，
    //    而不是「协议不支持」——两者给用户看的提示完全不同。
    //    地址用 127.0.0.1：连不上会**立刻**被拒，不用等 DNS / 路由超时。
    runtime().block_on(async {
        let err = app
            .connect_remote("smb://127.0.0.1/public")
            .await
            .expect_err("本机挂不上 127.0.0.1，应当失败");
        let msg = err.to_string();
        assert!(
            msg.contains("挂载"),
            "失败应当来自挂载这一步（说明确实走了挂载那条路）：{msg}"
        );
        assert!(
            !msg.contains("暂不支持"),
            "不该再把 smb 当成不支持的协议：{msg}"
        );
        assert!(
            app.live_connections().is_empty(),
            "挂载失败不该留下任何远程会话——这条路根本不建会话"
        );
    });
}

/// 平台原生的「在访达中显示」**只认本地路径**。
///
/// 远程条目（`/pub/x`）在本机磁盘上不存在，把这种路径交给系统文件管理器只会
/// 静默失败（访达不跳转），用户看到的就是「点了没反应」。所以这里直接挡掉，
/// UI 那边也不给菜单项——守卫见
/// `mo-ui::context_menu::remote_page_hides_host_only_actions`。
///
/// （「移到系统废纸篓」这条并列的通道已于 2026-09-24 移除：回收站只有一个入口，
/// macOS 生产模式删除本身就经系统废纸篓，见 `devlog/trash-unify.md`。）
#[test]
fn host_only_actions_refuse_remote_paths() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let _ = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");
        assert!(app.browsing_remote(), "前提：当前在看远程");

        let remote = vec![PathBuf::from("/remote.txt")];
        let reveal = app
            .reveal_in_file_manager(remote.clone())
            .await
            .expect_err("远程条目没法在系统文件管理器里显示");
        assert!(
            reveal.to_string().contains("远程"),
            "错误要说明原因（而不是丢一句系统报错）：{reveal}"
        );
    });
}

/// 反过来：**本地**路径在看远程时必须判成「本地」（不被上面那条门禁误伤）。
///
/// 去重 / 同步拿的就是本地路径，而它们经常在「当前页是远程」的时候调过来——
/// 与 `deleting_a_local_file_while_browsing_remote_still_uses_the_trash` 同一条边界。
///
/// ⚠️ 断言落在**判据本身**（`goes_through_remote`）而不是真的去调 AppKit：
/// `mo_platform::reveal` 要从后台线程 `dispatch_sync` 回主线程，而测试的主线程
/// 正被 `block_on` 占着、不再 drain 主队列——真调会直接挂死（真机行为见
/// `devlog/macos-platform.md`，那里主线程在跑事件循环，不会）。
#[test]
fn local_paths_stay_local_while_browsing_remote() {
    let dir = local_tree("remote-local-host-only");
    let app = tab(&Arc::new(SessionRegistry::new()));
    let _ = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");

        assert!(
            app.goes_through_remote(Path::new("/remote.txt")).await,
            "列表里那条是远程条目"
        );
        let local = dir.join("local.txt");
        assert!(
            !app.goes_through_remote(&local).await,
            "本机磁盘上的路径不该被当成远程条目——哪怕当前页在看远程"
        );
        assert!(
            !app.goes_through_remote(Path::new("/not-in-the-listing.txt"))
                .await,
            "列表里查不到的远程风格路径按本地处理（判据只认当前列表）"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// 远程端内复制，目标取**当前目录**：必须发给后端，不能判成本地。
///
/// 这是粘贴 / 同窗格拖拽的默认形态，也是模板里那个洞最容易漏的一条：目标就是当前
/// 目录，而**当前目录不是自己列表里的一行**——按「它是不是当前列表里那一行」
/// （`goes_through_remote`）去判，远程页上的粘贴会被当成「下载到本机」，
/// 于是把 `/pub/remote.txt` 交给 `std::fs`。
#[test]
fn copying_into_the_current_remote_dir_goes_through_the_backend() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let (_, _, log) = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/pub"))
            .await
            .expect("进入远程 /pub");
        assert!(app.browsing_remote(), "前提：当前页在远程");
        assert!(
            !app.goes_through_remote(Path::new("/pub")).await,
            "前提：当前目录自己不在列表里——正是这条判据答不出来的地方"
        );

        app.transfer(vec![PathBuf::from("/remote.txt")], Path::new("/pub"), false)
            .await;

        let written = format!(
            "write_file:{}",
            Path::new("/pub").join("remote.txt").display()
        );
        let mut seen = false;
        for _ in 0..200 {
            if log.lock().unwrap().contains(&written) {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(seen, "远程页内复制必须发给后端");
        assert!(
            !Path::new("/pub/remote.txt").exists(),
            "不该落到本机磁盘（判成本地时就会写向本机的 /pub）"
        );
    });
}

/// **传输的端点**由「拥有那一头的那一页」回答，不照路径猜。
///
/// 上面那条 `goes_through_remote` 的判据（「它是不是当前列表里的一行」）答不出传输
/// 需要的东西：粘贴到**当前目录**时 `dest` 自己不是列表里的行，分栏拖拽时目标那一头
/// 根本不在这个 `AppState` 的列表里。这条钉住 `Endpoint` 跟着页走。
#[test]
fn endpoint_follows_the_side_the_tab_is_browsing() {
    let app = tab(&Arc::new(SessionRegistry::new()));
    let dir = local_tree("endpoint-follows");

    runtime().block_on(async {
        assert!(
            matches!(app.endpoint(), Endpoint::Local),
            "没连远程时端点就是本机"
        );

        connect_fake(&app);
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");
        assert!(
            matches!(app.endpoint(), Endpoint::Remote(_)),
            "在看远程时端点必须是那条会话——否则远程条目会被交回本机管线"
        );

        app.open_local(&dir).await.expect("切回本地");
        assert!(
            matches!(app.endpoint(), Endpoint::Local),
            "切回本地后端点跟着回本机（连接还留着，但这一页不是它）"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// 本地 → **远程端点**：字节写进远程后端，**不**落到本机磁盘。
///
/// 分栏拖拽的典型样子：源窗格在看本机，目标窗格连着 FTP。若端点照路径猜
/// （`dest` 是远程服务器上的绝对路径 → `Path::exists()` 说「本机没有」→ 判成本地），
/// 这一步会交给 `std::fs` 去写。
///
/// ⚠️ 断言里那个 `dest` 目录**在本机是真实存在的**：这正是最危险的那种误判——
/// 远程路径 `/pub` 撞上本机真的有个 `/pub` 时，「以为在下载」会静悄悄写进本机。
#[test]
fn uploading_to_a_remote_endpoint_writes_through_the_backend() {
    let dir = local_tree("remote-upload");
    // 目标目录在本机真实存在：端点判错的话文件就会出现在这儿。
    let dest = dir.join("remote-side");
    std::fs::create_dir_all(&dest).unwrap();
    let src = dir.join("local.txt");

    let app = tab(&Arc::new(SessionRegistry::new()));
    let (_, _, log) = connect_fake_at(&app, TEST_URL);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");
        let remote = app.endpoint();

        app.transfer_between(vec![src.clone()], Endpoint::Local, &dest, remote, false)
            .await;

        let written = format!("write_file:{}", dest.join("local.txt").display());
        let mut seen = false;
        for _ in 0..200 {
            if log.lock().unwrap().contains(&written) {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            seen,
            "上传必须发给远程后端（记录里一条都没有 = 走了本机管线）"
        );
        assert!(
            !dest.join("local.txt").exists(),
            "不该落到本机磁盘：端点判错时文件会出现在这个真实存在的目录里"
        );
        assert!(
            !app.can_undo(),
            "远程传输刻意不记可逆项——撤销模型里的路径都是本地路径"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}

/// 远程端点 → 本地：字节落到本机目录（下载侧）。
///
/// 与上一条成对：传输的三条方向（上传 / 下载 / 远程内复制）都得真跑通一次。
#[test]
fn downloading_from_a_remote_endpoint_writes_locally() {
    let dir = local_tree("remote-download");
    let app = tab(&Arc::new(SessionRegistry::new()));
    connect_fake(&app);

    runtime().block_on(async {
        app.open_directory(Path::new("/"))
            .await
            .expect("进入远程 /");
        let remote = app.endpoint();

        app.transfer_between(
            vec![PathBuf::from("/remote.txt")],
            remote,
            &dir,
            Endpoint::Local,
            false,
        )
        .await;

        let dst = dir.join("remote.txt");
        for _ in 0..200 {
            if dst.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            std::fs::read(&dst).expect("下载应当在本机落盘"),
            b"from-remote",
            "落盘的必须是远程后端那份内容"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
}
