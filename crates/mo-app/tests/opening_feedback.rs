//! 「正在打开的目录」这枚立即反馈信号。
//!
//! 用户报的观感是「切到下载（27k 条）会卡一下」——读目录那 100–300ms 里界面还停在
//! 上一处的内容上，看起来就是「点了没反应」。`AppState::opening_path()` 与
//! `AppEvent::OpeningChanged` 就是给 UI 做反馈用的（侧栏高亮先跟过去、中央显示
//! 「正在读取 …」），这里钉住它的语义：
//!
//! * 读之前置位、读完之后**一定**清掉——成功、失败、提前 `return` 都算；
//! * 事件按 `Some(目标)` → `None` 的顺序发，UI 不必猜。

use std::path::PathBuf;

use mo_app::AppState;
use mo_core::AppEvent;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mo-app-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

/// 读完之后必须是「空闲」：否则 UI 上会挂着一条永远不消失的「正在读取 …」。
#[tokio::test]
async fn opening_flag_clears_after_a_successful_read() {
    let dir = tmp("opening-ok");
    std::fs::write(dir.join("a.txt"), b"x").unwrap();

    let app = AppState::new();
    app.open_directory(&dir).await.expect("打开目录失败");
    assert_eq!(app.opening_path(), None, "读完应回到空闲");
}

/// 读**失败**也要清干净：不然点错一个路径，那条提示就再也退不掉了。
#[tokio::test]
async fn opening_flag_clears_when_the_read_fails() {
    let app = AppState::new();
    let missing = tmp("opening-missing").join("这一层不存在");
    assert!(app.open_directory(&missing).await.is_err(), "应当读失败");
    assert_eq!(app.opening_path(), None, "失败路径同样要收干净");
}

/// 事件顺序：`Some(目标)` 在前、`None` 在后——UI 靠这一对开 / 关提示。
#[tokio::test]
async fn opening_events_bracket_the_read() {
    let dir = tmp("opening-events");
    std::fs::write(dir.join("a.txt"), b"x").unwrap();

    let app = AppState::new();
    let mut rx = app.bus().subscribe();
    app.open_directory(&dir).await.expect("打开目录失败");

    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::OpeningChanged { path } = ev {
            seen.push(path);
        }
    }
    assert!(
        seen.len() >= 2,
        "至少要有一条「开始」和一条「结束」：{seen:?}"
    );
    assert_eq!(
        seen.first().unwrap().as_deref(),
        Some(dir.as_path()),
        "第一条要报出正在打开的目标"
    );
    assert_eq!(seen.last().unwrap(), &None, "最后一条必须是「读完了」");
}
