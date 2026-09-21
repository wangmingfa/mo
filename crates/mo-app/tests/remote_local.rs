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
//! 为什么守卫落在 `mo-app` 而不是 UI 层：headless 的 GPUI 测试调度器会把「后台
//! tokio 线程唤醒测试任务」判成不确定性直接 panic（点击回调最终 `await` 到
//! `spawn_blocking`），UI 层写不出这条路径的自动化测试。见 `verify-gpui-layout-headless`。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mo_app::{AppState, SessionRegistry};
use mo_core::{EntryKind, FileId, FileMetadata, MoError};
use mo_fs::{FileSystem, ReadDirEntry};

/// 测试用的远程地址。
///
/// 刻意用 `127.0.0.1:1`：**万一复用的短路失效了**、真的去建连接，本机端口 1
/// 会立刻拒绝（而不是把测试卡在一条 SYN 上等超时），用例就会干脆地红掉。
const TEST_URL: &str = "ftp://127.0.0.1:1";

/// 只认识 `/` 与 `/pub` 的假远程后端：其它路径一律「远端没有这样的目录」，
/// 并记下被问过的路径——用来证明「切回来时读的是哪台机器的哪个目录」。
struct FakeRemoteFs {
    asked: Arc<Mutex<Vec<PathBuf>>>,
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

    fn read_dir_blocking(&self, path: &Path) -> Result<Vec<ReadDirEntry>, MoError> {
        self.asked.lock().unwrap().push(path.to_path_buf());
        match path.to_str() {
            Some("/") => Ok(Vec::new()),
            Some("/pub") => Ok(vec![ReadDirEntry::new(
                FileId::synthetic(Path::new("/pub/remote.txt")),
                "remote.txt".to_string(),
                EntryKind::File,
                PathBuf::from("/pub/remote.txt"),
            )]),
            other => Err(MoError::Other(format!(
                "远端没有这样的目录：{}",
                other.unwrap_or_default()
            ))),
        }
    }

    async fn metadata(&self, _path: &Path) -> Result<FileMetadata, MoError> {
        Err(Self::unsupported())
    }

    async fn create_dir(&self, _path: &Path) -> Result<(), MoError> {
        Err(Self::unsupported())
    }

    async fn write_file(&self, _path: &Path, _contents: &[u8]) -> Result<(), MoError> {
        Err(Self::unsupported())
    }

    async fn remove_file(&self, _path: &Path) -> Result<(), MoError> {
        Err(Self::unsupported())
    }

    async fn remove_dir(&self, _path: &Path) -> Result<(), MoError> {
        Err(Self::unsupported())
    }

    async fn rename(&self, _from: &Path, _to: &Path) -> Result<(), MoError> {
        Err(Self::unsupported())
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

/// 登入一条假连接，返回 `(编号, 被问过的路径)`。
fn connect_fake_at(app: &AppState, url: &str) -> (u64, Arc<Mutex<Vec<PathBuf>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    app.install_backend_for_test(
        Arc::new(FakeRemoteFs {
            asked: asked.clone(),
        }),
        url,
    );
    // 刚装上的那条就是当前生效的（最后一个编号）。
    let id = app
        .live_connections()
        .last()
        .expect("应当刚登入一条连接")
        .id;
    (id, asked)
}

fn connect_fake(app: &AppState) -> Arc<Mutex<Vec<PathBuf>>> {
    connect_fake_at(app, TEST_URL).1
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
    let (first, _) = connect_fake_at(&app, "ftp://127.0.0.1:1");
    let (second, _) = connect_fake_at(&app, "ftp://127.0.0.1:2");

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
