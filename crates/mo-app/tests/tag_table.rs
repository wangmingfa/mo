//! 颜色标签表的缓存语义：缓存绝不能让「打了标签看不见」。
//!
//! `AppState::tag_table` 是渲染路径上的签名缓存（行循环每帧取一次、行内只查表），
//! 代价是答案可能比磁盘旧。这里钉住它能自愈的两种情形：同一个 `AppState` 自己写过、
//! 以及**另一个标签页**（另一个 `AppState`）写过。
//!
//! 两种都做成「看不见 mtime 变化」的形态：写完把 mtime 按回原值，颜色名取等长的两个
//! （`red` / `tan`），于是文件大小也不变——只靠 mtime + 长度是发现不了的，必须靠
//! `CONFIG_WRITES` 那个进程内计数。**反向验证**：把计数从签名里拿掉，这两条都会红。

mod common;

use mo_app::AppState;
use std::path::PathBuf;
use std::time::SystemTime;

/// 标签的键：只是个路径字符串，`set_tag` 不碰文件系统，无需真实存在。
fn target() -> PathBuf {
    PathBuf::from("/mo-tag-fixture/entry.txt")
}

/// `isolated` 的闭包期间 env 锁是持有的（`common` 里锁到 `build` 返回），所以这里
/// 读到的 `MO_CONFIG_DIR` 就是本用例那一档。
fn config_file() -> PathBuf {
    PathBuf::from(std::env::var("MO_CONFIG_DIR").expect("MO_CONFIG_DIR 应已设好"))
        .join("config.json")
}

/// 把配置文件的 mtime 按回 `t`，模拟「文件系统只给秒级精度、这一秒内的改动看不见」。
fn freeze_config_mtime(t: SystemTime) {
    let f = std::fs::File::options()
        .write(true)
        .open(config_file())
        .expect("打开配置文件");
    f.set_modified(t).expect("回拨 mtime");
}

/// 配置此刻的「看得见的那半个签名」：长度 + mtime。
fn config_stamp() -> (u64, SystemTime) {
    let m = std::fs::metadata(config_file()).expect("配置文件应存在");
    (m.len(), m.modified().expect("mtime"))
}

/// 自己写过之后，自己那份缓存必须作废。
///
/// ⚠️ 起手不能是空表：`config.json` 从「不存在」变成「存在」时，长度与 mtime 本身就
/// 变了，那条平凡路径用不上写入计数——反向验证时这条会假绿（实测踩过）。所以先写
/// 一次把文件造出来，再看**等长改色**（`red` → `tan`）能不能被发现。
#[test]
fn tag_table_reflects_own_write() {
    let path = target();
    common::isolated("tag-own", || {
        let app = AppState::new();
        app.set_tag(path.clone(), "red".to_string());
        let (len, mtime) = config_stamp();
        let _ = app.tag_table(); // 缓存此刻是 `{path: red}`

        app.set_tag(path.clone(), "tan".to_string());
        freeze_config_mtime(mtime);
        assert_eq!(
            config_stamp(),
            (len, mtime),
            "前置条件：red 与 tan 等长、mtime 已回拨，看得见的半个签名没变"
        );

        assert_eq!(
            app.tag_of(&path).as_deref(),
            Some("tan"),
            "自己刚改的标签必须立刻可见——缓存不能压过这一次写入"
        );
    });
}

/// 另一个标签页（另一个 `AppState`）写过之后，这一份缓存也必须作废。
///
/// 每个标签页各有一份 `AppState`、也就各有一份缓存，所以光靠「自己写过」那条路径
/// 不够：`A` 打标签时 `B` 的计数器不会动，只能靠进程级写入计数。
#[test]
fn tag_table_reflects_another_appstate_write() {
    let path = target();
    common::isolated("tag-other", || {
        let a = AppState::new();
        a.set_tag(path.clone(), "red".to_string());
        let (len, mtime) = config_stamp();
        // a 的缓存此刻是 `{path: red}`；把「看得见的变化」抹掉，只留计数。
        let _ = a.tag_of(&path);
        freeze_config_mtime(mtime);

        let b = AppState::new();
        b.set_tag(path.clone(), "tan".to_string());
        freeze_config_mtime(mtime);
        assert_eq!(
            config_stamp().0,
            len,
            "前置条件：red 与 tan 等长，文件大小看不出变化"
        );

        assert_eq!(
            a.tag_of(&path).as_deref(),
            Some("tan"),
            "另一个标签页改的标签，这一页下一帧就该看到"
        );
    });
}
