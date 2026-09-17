//! 窗口取回延迟测量：在元数据回填洪流进行时并发取窗口，量化锁竞争。
//!
//! 这是滚动闪烁诊断的探针测试：如果这里的延迟是毫秒级，
//! 说明数据层不是瓶颈，问题在 UI/GPUI 侧（任务派发或渲染循环）。
//! 用 `cargo test -p mo-app --test window_fetch_latency -- --nocapture` 查看。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mo_app::AppState;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-app-lat-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

#[tokio::test]
async fn window_fetch_stays_fast_during_metadata_backfill() {
    let dir = tmp("probe");
    for i in 0..27000 {
        std::fs::write(dir.join(format!("{i:08}")), b"x").unwrap();
    }

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");
    let total = app.visible_count().await;
    assert_eq!(total, 27000);

    // 模拟用户滚动：回填进行中，连续 3 秒以 ~30Hz 取不同位置的窗口。
    let mut samples: Vec<(usize, u128)> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut pos = 0usize;
    while Instant::now() < deadline {
        let start = Instant::now();
        let range = pos..(pos + 60).min(total);
        let (_, _, entries) = app.visible_window(range).await;
        let elapsed = start.elapsed().as_millis();
        samples.push((pos, elapsed));
        assert_eq!(entries.len(), (pos + 60).min(total) - pos);
        pos = (pos + 937) % total; // 跳跃式滚动
        tokio::time::sleep(Duration::from_millis(33)).await;
    }

    let mut worst = samples.iter().cloned().max_by_key(|(_, e)| *e).unwrap();
    let avg = samples.iter().map(|(_, e)| *e).sum::<u128>() / samples.len() as u128;
    let over_10ms = samples.iter().filter(|(_, e)| *e > 10).count();
    let over_50ms = samples.iter().filter(|(_, e)| *e > 50).count();
    println!(
        "样本数={} 平均={}ms 最大={}ms(位置{}) >10ms: {} 次 >50ms: {} 次",
        samples.len(),
        avg,
        worst.1,
        worst.0,
        over_10ms,
        over_50ms
    );
    let _ = &mut worst;

    let _ = std::fs::remove_dir_all(&dir);
}
