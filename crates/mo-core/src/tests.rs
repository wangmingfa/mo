use crate::*;
use std::path::Path;

fn fid(n: u128) -> FileId {
    FileId::new(1, n)
}

#[test]
fn selection_single_and_toggle() {
    let mut s = SelectionModel::new();
    s.select(fid(1));
    assert!(s.is_selected(&fid(1)));
    assert_eq!(s.count(), 1);

    // toggle off
    s.toggle(fid(1));
    assert!(!s.is_selected(&fid(1)));
    assert_eq!(s.count(), 0);
}

#[test]
fn selection_multi_toggle() {
    let mut s = SelectionModel::new();
    s.toggle(fid(1));
    s.toggle(fid(2));
    s.toggle(fid(3));
    assert_eq!(s.count(), 3);

    s.toggle(fid(2));
    assert_eq!(s.count(), 2);
    assert!(!s.is_selected(&fid(2)));
}

#[test]
fn selection_range_picks_contiguous() {
    let ids = vec![fid(1), fid(2), fid(3), fid(4), fid(5)];
    let mut s = SelectionModel::new();
    s.select(fid(2)); // anchor
    s.select_range(&ids, 1, 3); // indices 1..=3 → ids 2,3,4
    assert_eq!(s.count(), 3);
    assert!(s.is_selected(&fid(2)));
    assert!(s.is_selected(&fid(3)));
    assert!(s.is_selected(&fid(4)));
    assert!(!s.is_selected(&fid(5)));
}

#[test]
fn selection_select_all_and_clear() {
    let ids = vec![fid(1), fid(2), fid(3)];
    let mut s = SelectionModel::new();
    s.select_all(&ids);
    assert_eq!(s.count(), 3);
    s.clear();
    assert_eq!(s.count(), 0);
}

#[test]
fn navigation_back_and_forward() {
    let mut nav = NavigationState::new();
    nav.visit(Path::new("/a").to_path_buf());
    nav.visit(Path::new("/b").to_path_buf());
    nav.visit(Path::new("/c").to_path_buf());

    assert_eq!(nav.current, Some(Path::new("/c").to_path_buf()));
    assert!(nav.can_go_back());

    let b = nav.go_back().unwrap();
    assert_eq!(b, Path::new("/b").to_path_buf());
    assert!(nav.can_go_forward());

    let a = nav.go_back().unwrap();
    assert_eq!(a, Path::new("/a").to_path_buf());

    let b2 = nav.go_forward().unwrap();
    assert_eq!(b2, Path::new("/b").to_path_buf());
}

#[test]
fn navigation_bookmark_and_recent() {
    let mut nav = NavigationState::new();
    let loc = Path::new("/x").to_path_buf();
    nav.add_bookmark(loc.clone());
    nav.add_bookmark(loc.clone());
    assert_eq!(nav.bookmarks.len(), 1); // 去重

    nav.visit(Path::new("/y").to_path_buf());
    nav.visit(Path::new("/z").to_path_buf());
    assert_eq!(nav.recent.len(), 2);
    assert_eq!(nav.recent[0], Path::new("/z").to_path_buf());
}

#[test]
fn file_id_synthetic_is_path_dependent() {
    let a = FileId::synthetic(Path::new("/foo/a.txt"));
    let b = FileId::synthetic(Path::new("/foo/b.txt"));
    assert_ne!(a, b);
}

fn entry(name: &str, kind: EntryKind, n: u128) -> Entry {
    let path = Path::new("/d").join(name);
    Entry::new(fid(n), name.to_string(), kind, path)
}

#[test]
fn directory_view_dirs_first_and_natural_order() {
    let mut dir = Directory::new(fid(0), Path::new("/d").to_path_buf());
    dir.set_entries(vec![
        entry("file10.txt", EntryKind::File, 1),
        entry("file2.txt", EntryKind::File, 2),
        entry("zeta", EntryKind::Directory, 3),
        entry("alpha", EntryKind::Directory, 4),
    ]);

    let names: Vec<String> = (0..dir.visible_count())
        .map(|i| dir.visible_entry(i).unwrap().name.clone())
        .collect();
    // 目录在前（alpha、zeta 内部再按名称），文件按自然序：file2 在 file10 之前。
    assert_eq!(names, vec!["alpha", "zeta", "file2.txt", "file10.txt"]);
}

#[test]
fn directory_view_filter_maps_back_to_entry_index() {
    let mut dir = Directory::new(fid(0), Path::new("/d").to_path_buf());
    dir.set_entries(vec![
        entry("alpha.txt", EntryKind::File, 1),
        entry("beta.txt", EntryKind::File, 2),
        entry("gamma.md", EntryKind::File, 3),
    ]);

    dir.set_filter(Some("TXT".to_string())); // 大小写不敏感
    assert_eq!(dir.visible_count(), 2);
    assert_eq!(dir.visible_entry(0).unwrap().name, "alpha.txt");
    assert_eq!(dir.visible_entry(1).unwrap().name, "beta.txt");

    // 清空过滤后恢复全部。
    dir.set_filter(None);
    assert_eq!(dir.visible_count(), 3);
}

#[test]
fn directory_index_survives_insert_and_remove() {
    let mut dir = Directory::new(fid(0), Path::new("/d").to_path_buf());
    dir.set_entries(vec![
        entry("a", EntryKind::File, 1),
        entry("b", EntryKind::File, 2),
    ]);

    // 追加后可按 id O(1) 找到。
    dir.push_entry(entry("c", EntryKind::File, 3));
    assert!(dir.entry_mut(fid(3)).is_some());

    // 删除后索引同步失效。
    assert!(dir.remove_entry(Path::new("/d/a")));
    assert!(dir.entry_mut(fid(1)).is_none());
    assert!(dir.entry_mut(fid(2)).is_some());
    assert_eq!(dir.visible_count(), 2);
}

#[test]
fn thumbnail_support_is_image_files_only() {
    assert!(entry("a.PNG", EntryKind::File, 1).supports_thumbnail());
    assert!(entry("b.jpeg", EntryKind::File, 2).supports_thumbnail());
    assert!(!entry("c.txt", EntryKind::File, 3).supports_thumbnail());
    // 目录即使名字带后缀也不生成缩略图。
    assert!(!entry("d.png", EntryKind::Directory, 4).supports_thumbnail());
}

#[tokio::test]
async fn event_bus_publish_subscribe() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    bus.publish(AppEvent::NavigationChanged {
        path: Path::new("/tmp").to_path_buf(),
    });
    let ev = rx.recv().await.unwrap();
    match ev {
        AppEvent::NavigationChanged { path } => assert_eq!(path, Path::new("/tmp").to_path_buf()),
        _ => panic!("unexpected event"),
    }
}
