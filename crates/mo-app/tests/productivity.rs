//! 第三阶段生产力能力集成测试：全局搜索、批量删除、文件预览、操作历史、撤销 / 重做。

use mo_app::AppState;
use mo_preview::PreviewKind;
use std::path::PathBuf;
use std::time::Duration;

fn tree(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("mo-prod-{}-{}-{}", tag, std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sub")).unwrap();
    std::fs::write(base.join("alpha.txt"), b"a").unwrap();
    std::fs::write(base.join("beta.log"), b"b").unwrap();
    std::fs::write(base.join("sub").join("gamma.md"), b"# g").unwrap();
    base
}

/// 一个隔离的临时回收站根目录。
fn trash_root(tag: &str) -> PathBuf {
    let t = std::env::temp_dir().join(format!("mo-trash-{}-{}-{}", tag, std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&t);
    t
}

/// 轮询直到 `f()` 为 true（最多约 4 秒）。
async fn wait_for<F: Fn() -> bool>(f: F) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[test]
fn global_search_finds_files_across_subdirs() {
    let base = tree("search");
    let app = AppState::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();
        app.index_root(base.clone(), 0);

        // 等待后台爬取完成。
        for _ in 0..200 {
            if app.index_count() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let hits = app.global_search("alpha", 50);
        assert!(!hits.is_empty(), "应索引到 alpha.txt");

        // 子目录里的文件也能搜到。
        let md = app.global_search("gamma", 50);
        assert!(md.iter().any(|h| h.path.ends_with("gamma.md")));

        // 按扩展名子串匹配。
        let dots = app.global_search(".md", 50);
        assert!(dots.iter().any(|h| h.path.ends_with("gamma.md")));
    });
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn delete_selection_removes_the_file() {
    let base = tree("del");
    let trash = trash_root("del");
    let target = base.join("beta.log");
    let app = AppState::with_trash(trash.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();

        let entries = app.current_entries().await;
        let id = entries
            .iter()
            .find(|e| e.path == target)
            .expect("找到 beta.log")
            .id;
        app.select(id).await;

        let ids = app
            .delete_selection()
            .await
            .expect("本地删除应当走回收站成功");
        assert_eq!(ids.len(), 1, "应提交一条删除操作");

        // 删除走回收站：原路径消失但文件被保留在回收站。
        wait_for(|| !target.exists()).await;
        assert!(!target.exists(), "删除后原路径应不存在");
    });
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&trash);
}

#[test]
fn preview_text_file_reports_kind() {
    let base = tree("pv");
    let app = AppState::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pv = app.preview(&base.join("alpha.txt")).unwrap();
        assert_eq!(pv.kind, PreviewKind::Text);
        let dir_pv = app.preview(&base.join("sub")).unwrap();
        assert_eq!(dir_pv.kind, PreviewKind::Directory);
    });
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn history_records_executed_operations() {
    let base = tree("hist");
    let trash = trash_root("hist");
    let app = AppState::with_trash(trash.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();
        let entries = app.current_entries().await;
        let id = entries
            .iter()
            .find(|e| e.path == base.join("beta.log"))
            .unwrap()
            .id;
        app.select(id).await;
        let _ = app.delete_selection().await;
        let snap = app.history_snapshot();
        assert!(snap.iter().any(|h| h.kind == "删除"));
    });
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&trash);
}

#[test]
fn undo_restores_a_deleted_file_from_trash() {
    let base = tree("undo-del");
    let trash = trash_root("undo-del");
    let sub = base.join("sub");
    let target = sub.join("gamma.md");
    let app = AppState::with_trash(trash.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&sub).await.unwrap();
        let entries = app.current_entries().await;
        let id = entries.iter().find(|e| e.path == target).unwrap().id;
        app.select(id).await;

        let _ = app.delete_selection().await;
        wait_for(|| !target.exists()).await;
        assert!(!target.exists(), "删除后原路径应消失");
        assert!(app.can_undo(), "删除后应可撤销");

        app.undo();
        wait_for(|| target.exists()).await;
        assert!(target.exists(), "撤销后应从回收站还原");
    });
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&trash);
}

#[test]
fn trash_list_purge_and_empty() {
    let base = tree("trash-panel");
    let trash = trash_root("trash-panel");
    let app = AppState::with_trash(trash.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();

        // 删除 beta.log → 进回收站。
        let entries = app.current_entries().await;
        let id = entries
            .iter()
            .find(|e| e.path == base.join("beta.log"))
            .unwrap()
            .id;
        app.select(id).await;
        let _ = app.delete_selection().await;
        wait_for(|| app.trash_count() == 1).await;
        assert_eq!(app.trash_count(), 1);
        let list = app.trash_list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].original, base.join("beta.log"));

        // 永久删除单条。
        app.purge_trash_entry(list[0].clone());
        wait_for(|| app.trash_count() == 0).await;
        assert_eq!(app.trash_count(), 0, "永久删除后回收站应为空");

        // 再删一个后清空。
        let entries = app.current_entries().await;
        let id = entries
            .iter()
            .find(|e| e.path == base.join("alpha.txt"))
            .unwrap()
            .id;
        app.select(id).await;
        let _ = app.delete_selection().await;
        wait_for(|| app.trash_count() == 1).await;
        app.empty_trash();
        wait_for(|| app.trash_count() == 0).await;
        assert_eq!(app.trash_count(), 0, "清空后回收站应为空");
    });
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&trash);
}

#[test]
fn undo_reverts_a_copy() {
    let base = tree("undo-copy");
    let trash = trash_root("undo-copy");
    let dst = base.join("dst");
    std::fs::create_dir_all(&dst).unwrap();
    let src = base.join("alpha.txt");
    let copied = dst.join("alpha.txt");

    let app = AppState::with_trash(trash.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();
        let entries = app.current_entries().await;
        let id = entries.iter().find(|e| e.path == src).unwrap().id;
        app.select(id).await;

        app.copy_selection(&dst).await;
        wait_for(|| copied.exists()).await;
        assert!(copied.exists(), "复制后应出现副本");

        app.undo();
        wait_for(|| !copied.exists()).await;
        assert!(!copied.exists(), "撤销复制应把副本移入回收站");
        assert!(src.exists(), "源文件应仍在原地");
    });
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&trash);
}

#[test]
fn undo_reverts_a_move() {
    let base = tree("undo-move");
    let trash = trash_root("undo-move");
    let dst = base.join("dst");
    std::fs::create_dir_all(&dst).unwrap();
    let src = base.join("beta.log");
    let moved = dst.join("beta.log");

    let app = AppState::with_trash(trash.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();
        let entries = app.current_entries().await;
        let id = entries.iter().find(|e| e.path == src).unwrap().id;
        app.select(id).await;

        app.move_selection(&dst).await;
        wait_for(|| moved.exists() && !src.exists()).await;
        assert!(
            moved.exists() && !src.exists(),
            "移动后应出现在目标且原位置消失"
        );

        app.undo();
        wait_for(|| src.exists() && !moved.exists()).await;
        assert!(
            src.exists() && !moved.exists(),
            "撤销移动应把文件移回原位置"
        );
    });
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&trash);
}

/// 键盘 ↑↓ 移动焦点：单步移动是单选，Shift（extend=true）是连选。
#[test]
fn move_cursor_moves_focus_and_extends_selection() {
    let base = tree("cursor");
    let app = AppState::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();
        assert_eq!(app.visible_count().await, 3, "base 下应有 3 个可见条目");

        // 没有焦点时 ↓ 聚焦第一项（Finder / Explorer 习惯）。
        let first = app.move_cursor(1, false).await.expect("应有焦点下标");
        assert_eq!(first, 0, "无焦点时 ↓ 应聚焦第一项");
        let ids = app.selection_ids().await;
        assert_eq!(ids.len(), 1, "单步移动应为单选");

        // ↓ 连选：anchor 到新焦点之间全部选中。
        let next = app.move_cursor(1, true).await.expect("应有焦点下标");
        assert_eq!(next, first + 1, "焦点应前进一步");
        let ids = app.selection_ids().await;
        assert_eq!(ids.len(), 2, "Shift 连选应选中 2 项");

        // ↑ 单步：回到单选，只剩 1 项。
        app.move_cursor(-1, false).await;
        let ids = app.selection_ids().await;
        assert_eq!(ids.len(), 1, "非 extend 移动应重置为单选");

        // 边界：连续 ↑ 不会越界。
        for _ in 0..10 {
            app.move_cursor(-1, false).await;
        }
        let first = app.move_cursor(-1, false).await.expect("应有焦点下标");
        assert_eq!(first, 0, "焦点应停在第一项");
    });
    let _ = std::fs::remove_dir_all(&base);
}

/// 侧边栏快捷位置：至少有主目录，且路径都存在。
#[test]
fn quick_locations_contains_home_with_existing_paths() {
    let app = AppState::new();
    let locs = app.quick_locations();
    assert!(!locs.is_empty(), "至少应解析出主目录");
    assert!(
        locs.iter().any(|(label, _)| label.contains("主目录")),
        "应包含「主目录」: {locs:?}"
    );
    for (label, p) in &locs {
        assert!(p.exists(), "「{label}」的路径应存在: {}", p.display());
    }
}

/// 新建文本文件（右键菜单「新建文本文件」的落点）。
///
/// 三条不变量：新文件是**空的**；重名时加序号且序号插在**扩展名前**；
/// 已有文件的内容**绝不**被动到（去重 + 底层 `create_new` 双保险）。
#[test]
fn create_file_is_empty_and_never_overwrites() {
    let base = tree("newfile");
    let app = AppState::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let first = app.create_file(&base, "新建文本.txt").await.unwrap();
        assert_eq!(first.file_name().unwrap(), "新建文本.txt");
        assert_eq!(
            std::fs::read(&first).unwrap(),
            b"",
            "新建的文本文件应当是空的"
        );

        // 重名 → 序号插在扩展名之前（`新建文本 2.txt`，而不是 `新建文本.txt 2`）。
        let second = app.create_file(&base, "新建文本.txt").await.unwrap();
        assert_eq!(second.file_name().unwrap(), "新建文本 2.txt");

        // 手工往第一个文件里写点东西，再新建：它必须原封不动。
        std::fs::write(&first, b"keep me").unwrap();
        let third = app.create_file(&base, "新建文本.txt").await.unwrap();
        assert_eq!(third.file_name().unwrap(), "新建文本 3.txt");
        assert_eq!(
            std::fs::read(&first).unwrap(),
            b"keep me",
            "新建操作把已有文件覆盖了"
        );

        // 名字为空 / 全是空白 → 回落到默认名，而不是建一个叫 "   " 的文件。
        let blank = app.create_file(&base, "   ").await.unwrap();
        assert_eq!(blank.file_name().unwrap(), "新建文本 4.txt");

        // 名字本来不冲突时不要平白加序号（`unique_path` 总从 2 起编号，
        // 若无条件调用，首次新建就会变成「新建文件夹 2」）。
        let dir = app.create_folder(&base, "").await.unwrap();
        assert_eq!(dir.file_name().unwrap(), "新建文件夹");
        assert!(dir.is_dir());
    });
    let _ = std::fs::remove_dir_all(&base);
}
