//! 「最近连接 / 服务器收藏」的引擎层账本：`SavedServer.favorite` 的持久化与排序。
//!
//! 密码永远不进 config（钥匙串负责），这里只钉三件事：收藏跟着地址走（重连不丢
//! 星标）、收藏的排最近之前、没连过的地址无从收藏。
//!
//! ⚠️ `config_path()` 每次现读 `MO_CONFIG_DIR`，而 `isolated` 的锁只在构造期间
//! 持有——所以**整个用例体**（不止 `AppState::new`）都得放进同一次 `isolated` 的
//! 闭包里，不然并行的兄弟用例会把环境变量改走，这边写 config 就写到别人家去了。

mod common;

use mo_app::AppState;

/// 收藏一台 → 重连（重记）一次 → 星标不能被抹掉。
#[test]
fn remembering_again_keeps_the_favorite() {
    common::isolated("saved-servers-keep", || {
        let app = AppState::new();
        app.remember_server("sftp://host-a", "alice", None).unwrap();
        app.set_server_favorite("sftp://host-a", true).unwrap();

        // 过一会儿同一台机器又连了一次：重新记一遍（时间戳刷新）。
        app.remember_server("sftp://host-a", "alice", None).unwrap();

        let servers = app.saved_servers();
        assert_eq!(servers.len(), 1);
        assert!(
            servers[0].favorite,
            "重连不该丢收藏：{}",
            servers[0].endpoint
        );
    });
}

/// 收藏的排「最近」之前——哪怕它更早用过。
#[test]
fn favorites_sort_before_recents() {
    common::isolated("saved-servers-sort", || {
        let app = AppState::new();
        app.remember_server("sftp://old-host", "u1", None).unwrap();
        // 后连的这台时间戳更新，不收藏时它排第一。
        app.remember_server("sftp://new-host", "u2", None).unwrap();
        app.set_server_favorite("sftp://old-host", true).unwrap();

        let servers = app.saved_servers();
        assert_eq!(servers[0].endpoint, "sftp://old-host", "收藏的排最前");
        assert_eq!(servers[1].endpoint, "sftp://new-host");
        assert!(servers[0].favorite);
        assert!(!servers[1].favorite);
    });
}

/// 星标真的落 config.json：重开应用（再建一个 AppState）之后还在。
#[test]
fn favorite_survives_an_app_restart() {
    common::isolated("saved-servers-restart", || {
        // 两次构造必须在**同一次** isolated 里：第二次调用会把目录清空重建，
        // 分开传 tag 就是「重启到了一台全新机器」。
        let app = AppState::new();
        app.remember_server("webdav://dav-host", "u", None).unwrap();
        app.set_server_favorite("webdav://dav-host", true).unwrap();

        let reopened = AppState::new();
        let servers = reopened.saved_servers();
        assert_eq!(servers.len(), 1, "重开应用后记录还在");
        assert!(servers[0].favorite, "重开应用后收藏还在");
    });
}

/// 没连过的地址无从收藏；重复点同一颗星幂等（第二次不写盘也不报错）。
#[test]
fn favoriting_an_unknown_endpoint_is_rejected_and_toggling_is_idempotent() {
    common::isolated("saved-servers-idem", || {
        let app = AppState::new();
        assert!(
            app.set_server_favorite("sftp://never-connected", true)
                .is_err(),
            "没连过的地址没有条目可标"
        );

        app.remember_server("sftp://host-b", "u", None).unwrap();
        app.set_server_favorite("sftp://host-b", true).unwrap();
        // 再点一次星（幂等）：不该报错，状态也不该翻回去。
        app.set_server_favorite("sftp://host-b", true).unwrap();
        assert!(app.saved_servers()[0].favorite);

        // 取消收藏落盘。
        app.set_server_favorite("sftp://host-b", false).unwrap();
        assert!(!app.saved_servers()[0].favorite);
    });
}
