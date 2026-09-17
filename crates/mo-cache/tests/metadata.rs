use std::time::{Duration, UNIX_EPOCH};

use mo_cache::MetadataCache;
use mo_core::{FileId, FileMetadata, Permissions};

fn temp_db(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("mo-cache-test");
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    let path = dir.join(format!("{tag}.sqlite"));
    let _ = std::fs::remove_file(&path);
    path
}

fn meta(size: u64, secs: u64) -> FileMetadata {
    FileMetadata {
        size,
        modified: Some(UNIX_EPOCH + Duration::from_secs(secs)),
        created: Some(UNIX_EPOCH + Duration::from_secs(secs - 10)),
        permissions: Permissions {
            readonly: false,
            hidden: false,
        },
    }
}

#[test]
fn put_many_then_get_many_roundtrip() {
    let cache = MetadataCache::open(&temp_db("roundtrip")).expect("打开缓存失败");
    let items: Vec<(FileId, FileMetadata)> = (0..5)
        .map(|i| (FileId::new(1, i as u128), meta(i * 100, 1_700_000_000 + i)))
        .collect();
    cache.put_many(&items).expect("批量写入失败");

    let ids: Vec<FileId> = items.iter().map(|(id, _)| *id).collect();
    let got = cache.get_many(&ids).expect("批量读取失败");

    assert_eq!(got.len(), 5);
    for (id, m) in &items {
        let loaded = got.get(id).unwrap_or_else(|| panic!("缺少 {id}"));
        assert_eq!(loaded.size, m.size);
        assert_eq!(loaded.modified, m.modified);
    }
}

#[test]
fn put_many_overwrites_same_id() {
    let cache = MetadataCache::open(&temp_db("overwrite")).expect("打开缓存失败");
    let id = FileId::new(1, 1);
    cache.put(&id, &meta(10, 1_000)).unwrap();
    cache.put_many(&[(id, meta(99, 2_000))]).unwrap();

    let loaded = cache.get(&id).unwrap().expect("应能读回");
    assert_eq!(loaded.size, 99, "同 id 再次写入应覆盖");
}

#[test]
fn get_returns_none_for_unknown_id() {
    let cache = MetadataCache::open(&temp_db("unknown")).expect("打开缓存失败");
    assert!(cache.get(&FileId::new(42, 42)).unwrap().is_none());
}

#[test]
fn clear_empties_cache() {
    let cache = MetadataCache::open(&temp_db("clear")).expect("打开缓存失败");
    cache
        .put_many(&[(FileId::new(1, 1), meta(1, 1_000))])
        .unwrap();
    cache.clear().unwrap();
    assert!(cache.get(&FileId::new(1, 1)).unwrap().is_none());
}

#[test]
fn cache_is_shareable_across_threads() {
    // 元数据缓存会被后台任务并发写入，必须能放进 Arc 跨线程使用。
    let cache = std::sync::Arc::new(MetadataCache::open(&temp_db("threads")).unwrap());
    let mut handles = Vec::new();
    for t in 0..4u32 {
        let c = cache.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..25u32 {
                let id = FileId::new(1, (t * 100 + i) as u128);
                c.put(&id, &meta(i as u64, 1_000)).expect("写入失败");
            }
        }));
    }
    for h in handles {
        h.join().expect("线程 panic");
    }
    assert!(cache.get(&FileId::new(1, 0)).unwrap().is_some());
}
