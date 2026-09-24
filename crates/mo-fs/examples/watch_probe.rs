//! 探针：把 watched 目录里的文件 rename 到目录外（= 回收站的真实路径），
//! 看 notify 在 macOS（FSEvents）上到底报什么事件。
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

fn main() {
    let base = std::env::temp_dir().join("mo-watch-probe");
    let _ = fs::remove_dir_all(&base);
    let dir = base.join("dir");
    let outside = base.join("outside");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&outside).unwrap();

    let f1 = dir.join("moved-out.txt");
    let f2 = dir.join("deleted.txt");
    fs::write(&f1, "a").unwrap();
    fs::write(&f2, "b").unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let _ = tx.send((format!("{:?}", ev.kind), ev.paths.clone()));
        }
    })
    .unwrap();
    w.watch(&dir, RecursiveMode::NonRecursive).unwrap();

    thread::sleep(Duration::from_millis(300));
    // 1) rename 到 watched 目录之外（回收站语义）
    fs::rename(&f1, outside.join("moved-out.txt")).unwrap();
    thread::sleep(Duration::from_millis(300));
    // 2) 直接删除
    fs::remove_file(&f2).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Ok((kind, paths)) = rx.recv_timeout(Duration::from_millis(200)) {
            let ps: Vec<String> = paths
                .iter()
                .map(|p: &PathBuf| p.display().to_string())
                .collect();
            println!("EVENT kind={} paths={}", kind, ps.join(" | "));
        }
    }
    let _ = fs::remove_dir_all(&base);
}
