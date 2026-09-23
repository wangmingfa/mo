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
    // 索引现在**落在盘上**（`~/Library/Caches/mo/search.sqlite`），不隔离就会往开发者
    // 机器上的真实索引里写测试目录，而且那些记录会一直留在那儿、把后续搜索的前 50
    // 条挤掉。测试一律把库钉到临时目录。
    let dir = std::env::temp_dir().join(format!("mo-index-prod-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建索引目录");
    std::env::set_var("MO_CACHE_DIR", &dir);

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

/// type-ahead：键盘输入即定位——在可见条目里跳到文件名以输入串开头的第一条。
#[test]
fn focus_by_prefix_jumps_to_first_matching_name() {
    let base = tree("typeahead");
    let app = AppState::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();
        let entries = app.current_entries().await;
        let alpha_id = entries.iter().find(|e| e.name == "alpha.txt").unwrap().id;
        let beta_id = entries.iter().find(|e| e.name == "beta.log").unwrap().id;

        // "al" 跳到 alpha.txt：返回其可见位置，且单选它。
        let idx = app.focus_by_prefix("al").await.expect("应跳到 alpha.txt");
        assert_eq!(
            app.selection_ids().await,
            vec![alpha_id],
            "跳选应单选 alpha.txt"
        );

        // "be" 跳到 beta.log：可见序排在 alpha.txt 之后，且选中它。
        let idx_b = app.focus_by_prefix("be").await.expect("应跳到 beta.log");
        assert!(idx_b > idx, "beta.log 在可见序中应排在 alpha.txt 之后");
        assert_eq!(app.selection_ids().await, vec![beta_id]);

        // 大小写不敏感："AL" 同样命中 alpha.txt（同一可见位置）。
        let idx_a2 = app.focus_by_prefix("AL").await.expect("大写也应命中");
        assert_eq!(idx_a2, idx, "大写前缀应命中同一个 alpha.txt");
        assert_eq!(app.selection_ids().await, vec![alpha_id]);

        // 无匹配 → 返回 None，且不改变已有选择。
        let before = app.selection_ids().await;
        assert!(
            app.focus_by_prefix("zzz").await.is_none(),
            "无匹配应返回 None"
        );
        assert_eq!(app.selection_ids().await, before, "无匹配时不应改动选择集");

        // 空串 → 直接 None，不动作。
        assert!(app.focus_by_prefix("").await.is_none());
    });
    let _ = std::fs::remove_dir_all(&base);
}

/// 反选：可见集内翻转让未选中的变成选中、已选中的取消，且不碰隐藏 / 过滤掉的条目。
#[test]
fn invert_selection_flips_visible_set_only() {
    let base = tree("invert");
    let app = AppState::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();
        let ids = app
            .current_entries()
            .await
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 3);

        // 先选第一个，再反选 → 应剩下两个未选中的。
        app.select(ids[0]).await;
        app.select_invert_visible().await;
        let sel = app.selection_ids().await;
        assert_eq!(sel.len(), 2, "反选后应剩 2 个");
        assert!(!sel.contains(&ids[0]), "原选中项应被取消");
        assert!(
            sel.contains(&ids[1]) && sel.contains(&ids[2]),
            "另两项应被选中"
        );

        // 全选后反选 → 空集。
        app.select_all_visible().await;
        app.select_invert_visible().await;
        assert!(app.selection_ids().await.is_empty(), "全选后反选应为空");
    });
    let _ = std::fs::remove_dir_all(&base);
}

/// 列表分组：行流 = 组头 + 条目交错；行区间选择跳过组头；type-ahead 返回行下标。
///
/// 行空间（含组头）与条目空间是两套下标——这里钉住三者的换算关系，
/// 任何一处漏乘（渲染 count、框选、键盘导航）都会在这条测试上翻车。
#[test]
fn grouping_rows_headers_and_row_range_selection() {
    let base = tree("grouping");
    let app = AppState::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        app.open_directory(&base).await.unwrap();
        assert_eq!(app.visible_count().await, 3, "sub + alpha.txt + beta.log");
        // 无分组时行数 = 条目数（行=条目恒等，既有路径不能变）。
        assert_eq!(app.list_row_count().await, 3);

        app.set_grouping(mo_app::Grouping::Kind).await;
        // 名字排序且目录在前：sub(0) → 文件夹；alpha.txt(1) → 文稿（txt）；
        // beta.log(2) → 其他（log 不在任何类目表里）。行流：
        // [头-文件夹, sub, 头-文稿, alpha, 头-其他, beta]。
        let rows = app.list_row_count().await;
        assert_eq!(rows, 6, "3 条目 + 3 个非空组头");

        let (_, start, win) = app.list_window(0..rows, true).await;
        assert_eq!(start, 0);
        assert_eq!(win.len(), rows, "窗口行数 = 行流总数");
        assert!(
            matches!(win[0], mo_app::WindowRow::Header(_)),
            "第 0 行应是「文件夹」组头"
        );
        assert!(matches!(win[1], mo_app::WindowRow::Entry(_)));
        assert_eq!(
            win.iter()
                .filter(|r| matches!(r, mo_app::WindowRow::Entry(_)))
                .count(),
            3,
            "行流里的条目行恰好 3 条"
        );

        // 行区间选择（区间故意盖住组头行）：行 2..=6 只含 alpha 与 beta 两条条目。
        app.clear_selection().await;
        app.select_rows_range(2, 6).await;
        assert_eq!(
            app.selection_ids().await.len(),
            2,
            "行区间选择应跳过组头、选中 2 条条目"
        );

        // type-ahead 返回**行**下标：alpha.txt 在「文稿」组里，行号 = 3。
        let row = app.focus_by_prefix("al").await.unwrap();
        assert_eq!(row, 3, "alpha.txt 的行号应把组头行算进去");
        assert_eq!(app.selection_ids().await.len(), 1, "跳选应为单选");

        // 切回不分组：行数回到条目数。
        app.set_grouping(mo_app::Grouping::None).await;
        assert_eq!(app.list_row_count().await, 3);
    });
    let _ = std::fs::remove_dir_all(&base);
}
