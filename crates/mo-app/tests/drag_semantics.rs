//! 拖放语义的判定表（devlog/windows-port.md §43）：应用内拖放的
//! 复制 / 移动由 `drag_resolves_to_move` 现算——资源管理器同款：
//! 同卷宗拖 = 移动、跨卷宗 = 复制，Ctrl 强制复制、Shift / Alt 强制移动，
//! 跨会话永远只敢复制。
//!
//! 这里是**纯判定**的单测（不碰磁盘、不起窗口）；派发与结算的端到端在
//! `crates/mo-ui/tests/` 的拖拽系列里。

use std::path::Path;
use std::sync::Arc;

use mo_app::{drag_resolves_to_move, Endpoint};
use mo_fs::LocalFileSystem;

fn local() -> Endpoint {
    Endpoint::Local
}

/// 「远程会话」的替身：判定只看 Arc 身份，不看里面是谁。
fn remote() -> Endpoint {
    Endpoint::Remote(Arc::new(LocalFileSystem))
}

const SRC: &str = "/home/u/notes.txt";
const DIR: &str = "/home/u/box";

/// 无修饰键 + 同会话 + 同卷宗 = **移动**（用户报的正是这条：同目录里
/// 把文件拖进子目录，此前恒为复制）。
#[test]
fn same_volume_without_modifiers_moves() {
    assert!(drag_resolves_to_move(
        false,
        false,
        false,
        &local(),
        Path::new(SRC),
        Path::new(DIR),
        &local()
    ));
}

/// 无修饰键 + 跨卷宗 = **复制**。`/Volumes/<名>` 是判得出来的形状
/// （macOS 外接卷；这条在 Windows 宿主上同样成立——判定走的是路径形状）。
#[test]
fn cross_volume_without_modifiers_copies() {
    assert!(!drag_resolves_to_move(
        false,
        false,
        false,
        &local(),
        Path::new(SRC),
        Path::new("/Volumes/USB/box"),
        &local(),
    ));
}

/// Ctrl = 强制复制——哪怕同卷宗（资源管理器的 Ctrl+拖）。
#[test]
fn ctrl_forces_copy() {
    assert!(!drag_resolves_to_move(
        true,
        false,
        false,
        &local(),
        Path::new(SRC),
        Path::new(DIR),
        &local()
    ));
    assert!(!drag_resolves_to_move(
        true,
        true,
        true,
        &local(),
        Path::new(SRC),
        Path::new(DIR),
        &local(),
    ));
}

/// Shift = 强制移动（资源管理器）；Alt 保留为移动别名（访达 ⌥=移动，
/// 也是 Mo §36 起的既有肌肉记忆）——跨卷宗、跨会话都照移。
#[test]
fn shift_and_alt_force_move() {
    assert!(drag_resolves_to_move(
        false,
        true,
        false,
        &local(),
        Path::new(SRC),
        Path::new("/Volumes/USB/box"),
        &local(),
    ));
    assert!(drag_resolves_to_move(
        false,
        false,
        true,
        &local(),
        Path::new(SRC),
        Path::new(DIR),
        &local()
    ));
    // 跨会话 + Alt：仍按用户明说的移动（Ctrl 优先级更高）。
    assert!(drag_resolves_to_move(
        false,
        false,
        true,
        &local(),
        Path::new(SRC),
        Path::new(DIR),
        &remote()
    ));
    assert!(!drag_resolves_to_move(
        true,
        false,
        true,
        &local(),
        Path::new(SRC),
        Path::new(DIR),
        &remote()
    ));
}

/// 跨会话（本地 ↔ 远程、两条不同远程）= 复制：默认动作不许跨会话搬走东西。
#[test]
fn cross_session_copies() {
    assert!(!drag_resolves_to_move(
        false,
        false,
        false,
        &local(),
        Path::new(SRC),
        Path::new(DIR),
        &remote()
    ));
    assert!(!drag_resolves_to_move(
        false,
        false,
        false,
        &remote(),
        Path::new(SRC),
        Path::new(DIR),
        &remote()
    ));
}

/// 同一条远程会话内 = 按同卷宗对待，默认移动（服务器端 rename）。
#[test]
fn same_remote_session_moves() {
    let session = remote();
    let same = session.clone();
    assert!(drag_resolves_to_move(
        false,
        false,
        false,
        &session,
        Path::new(SRC),
        Path::new(DIR),
        &same
    ));
}
