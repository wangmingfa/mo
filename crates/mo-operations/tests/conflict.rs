//! 目标冲突处理的行为测试。
//!
//! 这一组测试守的是一条底线：**默认策略绝不静默覆盖已有文件**。
//! `std::fs::rename` / `fs::write` 在目标存在时都会直接替换，
//! 一次误操作就是不可逆的数据丢失，所以策略必须被测试锁住。

use std::path::PathBuf;

use mo_operations::{
    resolve_target, unique_path, ConflictPolicy, CopyOperation, MoveOperation, Operation,
    OperationStatus, Target,
};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-ops-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

#[test]
fn copy_rename_policy_never_overwrites() {
    let dir = tmp("rename");
    let src = dir.join("a.txt");
    let dst = dir.join("out.txt");
    std::fs::write(&src, b"new").unwrap();
    std::fs::write(&dst, b"old").unwrap();

    let op = CopyOperation::with_policy(1, src, dst.clone(), ConflictPolicy::Rename);
    op.run().expect("复制失败");

    // 原目标不动，新内容写到「out 2.txt」。
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "old");
    assert_eq!(
        std::fs::read_to_string(dir.join("out 2.txt")).unwrap(),
        "new"
    );
    assert_eq!(op.status(), OperationStatus::Completed);
}

#[test]
fn copy_skip_policy_keeps_target_and_writes_nothing() {
    let dir = tmp("skip");
    let src = dir.join("a.txt");
    let dst = dir.join("out.txt");
    std::fs::write(&src, b"new").unwrap();
    std::fs::write(&dst, b"old").unwrap();

    let op = CopyOperation::with_policy(2, src, dst.clone(), ConflictPolicy::Skip);
    op.run().expect("应视为成功");

    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "old");
    assert!(!dir.join("out 2.txt").exists());
}

#[test]
fn copy_overwrite_policy_replaces_target() {
    let dir = tmp("overwrite");
    let src = dir.join("a.txt");
    let dst = dir.join("out.txt");
    std::fs::write(&src, b"new").unwrap();
    std::fs::write(&dst, b"old").unwrap();

    let op = CopyOperation::with_policy(3, src, dst.clone(), ConflictPolicy::Overwrite);
    op.run().expect("复制失败");

    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "new");
}

#[test]
fn copy_abort_policy_fails_without_touching_target() {
    let dir = tmp("abort");
    let src = dir.join("a.txt");
    let dst = dir.join("out.txt");
    std::fs::write(&src, b"new").unwrap();
    std::fs::write(&dst, b"old").unwrap();

    let op = CopyOperation::with_policy(4, src, dst.clone(), ConflictPolicy::Abort);
    assert!(op.run().is_err());
    assert_eq!(op.status(), OperationStatus::Failed);
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "old");
}

#[test]
fn move_does_not_clobber_existing_target() {
    let dir = tmp("move");
    let src = dir.join("a.txt");
    let dst = dir.join("out.txt");
    std::fs::write(&src, b"moved").unwrap();
    std::fs::write(&dst, b"existing").unwrap();

    let op = MoveOperation::with_policy(5, src.clone(), dst.clone(), ConflictPolicy::Rename);
    op.run().expect("移动失败");

    assert!(!src.exists(), "源文件应已移走");
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "existing");
    assert_eq!(
        std::fs::read_to_string(dir.join("out 2.txt")).unwrap(),
        "moved"
    );
}

#[test]
fn resolve_target_honours_each_policy() {
    let dir = tmp("resolve");
    let existing = dir.join("a.txt");
    std::fs::write(&existing, b"x").unwrap();
    let missing = dir.join("nope.txt");

    // 目标不存在 → 一律直接写。
    for policy in [
        ConflictPolicy::Rename,
        ConflictPolicy::Skip,
        ConflictPolicy::Overwrite,
        ConflictPolicy::Abort,
    ] {
        assert!(matches!(resolve_target(&missing, policy), Target::Write(_)));
    }

    // 目标存在 → 各策略各走各的。
    assert!(matches!(
        resolve_target(&existing, ConflictPolicy::Skip),
        Target::Skip
    ));
    assert!(matches!(
        resolve_target(&existing, ConflictPolicy::Abort),
        Target::Abort
    ));
    assert!(matches!(
        resolve_target(&existing, ConflictPolicy::Overwrite),
        Target::Write(_)
    ));
    match resolve_target(&existing, ConflictPolicy::Rename) {
        Target::Write(p) => assert_eq!(p.file_name().unwrap(), "a 2.txt"),
        other => panic!("Rename 应给出新名字，实际：{other:?}"),
    }
}

#[test]
fn unique_path_finds_next_free_name() {
    let dir = tmp("unique");
    let target = dir.join("a.txt");
    std::fs::write(&target, b"1").unwrap();

    let p2 = unique_path(&target);
    assert_eq!(p2.file_name().unwrap(), "a 2.txt");

    std::fs::write(&p2, b"2").unwrap();
    assert_eq!(unique_path(&target).file_name().unwrap(), "a 3.txt");
}
