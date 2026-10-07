//! 全局搜索的查询不再压在主要板上（devlog/windows-port.md §46）。
//!
//! §45 收账时留下同一族的第三把刀：`Modal::GlobalSearch` 的按键处理里每敲一个字
//! 就同步跑一次 `AppState::global_search`——那是一条 `name_lower LIKE '%词%' OR
//! path LIKE '%词%'`，中缀通配用不上任何索引，在用户那份 58 万行的真索引上实测约
//! **150ms/次**。也就是说打字即卡顿，一次一下。现在查询整趟派发到 blocking 池，
//! 回来只对「趟号」：
//!
//! 1. **敲下那一拍不等 SQL**：状态立刻是「回显已跟上、有查询在途、结果还空着」，
//!    命中要等下一拍落地。
//! 2. **过期结果不许落地**：在途那趟回来后只对得上它出发时的趟号与模态。关掉模态
//!    又重新打开，不能看见上一次查询的行（那就是「搜索框是空的却列出一堆结果」）。
//! 3. **退格到空查询就地清空**：不派发、也不把上一轮的命中留在屏上。
//!
//! 断言打在状态而不是像素上（同 `drag_feedback.rs` 的取舍：承诺以状态为准，底色只是
//! 它的投影）。命中落地是墙钟轮询——爬索引与查询都跑在 Mo 进程级 tokio runtime 上，
//! 不受 GPUI 测试调度器驱动。索引库按测试进程共享，故 fixture 文件名带 tag，且断言
//! 只认「不止一条」而不是具体行数。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{px, size, TestAppContext, VisualTestContext, WindowHandle};
use mo_app::AppState;
use mo_ui::RootView;

/// `src/` 里三枚文件：一枚在子目录（跨目录才是这张索引的意义）。
fn tree(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("mo-gsearch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let src = base.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join(format!("qsolo-{tag}.txt")), b"x").unwrap();
    std::fs::write(src.join("sub").join(format!("qsdeep-{tag}.txt")), b"x").unwrap();
    std::fs::write(src.join(format!("zeta-{tag}.md")), b"x").unwrap();
    src
}

/// 开窗，并把 `src` 这棵树喂进索引（`index_root` 是派发即忘，要轮询到查得着）。
fn setup(
    tag: &str,
    cx: &mut TestAppContext,
) -> (PathBuf, VisualTestContext, WindowHandle<RootView>) {
    let src = tree(tag);
    mo_ui::isolate_user_dirs_for_tests();
    cx.update(gpui_kit::init);
    cx.dispatcher.allow_parking();
    let app = AppState::with_trash(std::env::temp_dir().join(format!("mo-gsearch-trash-{tag}")));
    app.index_root(src.clone(), 0);
    let win_app = app.clone();
    let window = cx.open_window(size(px(1000.), px(700.)), move |_, cx| {
        RootView::new(win_app, cx)
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);

    // 索引在进程级 runtime 上爬：先握**同步**门面轮询到「我这几枚都在」，再测异步那条路
    // ——同步门面在测试里只是量具，被测的是模态走的那条后台路。
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let hits = app.global_search(&format!("qsolo-{tag}"), 50);
        let deep = app.global_search(&format!("qsdeep-{tag}"), 50);
        if hits.iter().any(|h| h.name == format!("qsolo-{tag}.txt"))
            && deep.iter().any(|h| h.name == format!("qsdeep-{tag}.txt"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "索引没把 {tag} 这棵树收进来");
        std::thread::sleep(Duration::from_millis(20));
    }
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    (src, vcx, window)
}

/// 打开全局搜索模态（等价两个真入口：命令 `search.global` 与命令面板那条）。
fn open_modal(window: &WindowHandle<RootView>, cx: &mut TestAppContext) {
    window
        .update(cx, |root, _w, cx| {
            mo_ui::open_global_search_for_tests(root);
            cx.notify();
        })
        .expect("开模态失败");
}

fn state(window: &WindowHandle<RootView>, cx: &mut TestAppContext) -> (String, usize, bool) {
    window
        .update(cx, |root, _w, _cx| {
            mo_ui::global_search_state_for_tests(root)
        })
        .expect("读状态")
}

/// 泵 N 拍：跑调度 + 渲帧（在途的结果只在这时候落地）。
fn pump(vcx: &mut VisualTestContext, times: usize) {
    for _ in 0..times {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 泵到「有命中且不在途」，超时即失败。
fn settle(vcx: &mut VisualTestContext, window: &WindowHandle<RootView>, cx: &mut TestAppContext) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pump(vcx, 1);
        let (_q, hits, busy) = state(window, cx);
        if hits > 0 && !busy {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "后台那趟查询的结果一直没落地（hits={hits} busy={busy}）"
        );
    }
}

/// ①：敲第一个字的那一拍主线程不等 SQL，命中下一拍才来。
#[gpui_kit::test]
fn typing_shows_busy_first_and_the_hits_afterwards(cx: &mut TestAppContext) {
    let (src, mut vcx, window) = setup("echo", cx);
    open_modal(&window, cx);

    // 与 ② 同一条派发纪律：断言「这一拍还在途」必须用**不泵调度器**的 `press`。
    // 这里原先写的是 `simulate_keystrokes`，它会顺带跑一轮 parking，测试索引又小，
    // 后台那趟完全可能在同一个 parking 里就落了地——于是 `busy` 断言变成掷硬币
    // （全量闸门里真红过一次：hits 已回、busy 已清）。
    vcx.update(|window, app| window.press("z", app));
    // 还没泵过调度器：回显已跟上、在途标记已立、结果仍空——这一拍要是同步扫表，
    // 画面上就是「按下去什么都没发生，过一下整窗才跳出来」。
    let (query, hits, busy) = state(&window, cx);
    assert_eq!(query, "z", "回显应当立刻跟着按键");
    assert!(busy, "查询已派发、还没落地：这一拍必须在途");
    assert_eq!(hits, 0, "命中不能在这拍就出现（那说明还在主线程同步扫表）");

    settle(&mut vcx, &window, cx);
    let (_q, hits, _busy) = state(&window, cx);
    assert!(hits > 0, "后台那趟该把 zeta-echo.md 端回来");
    assert!(src.join("zeta-echo.md").is_file(), "fixture 还在");
}

/// ②：关掉模态又打开，看不见上一次查询的行（趟号作废在途那趟）。
#[gpui_kit::test]
fn a_reopened_modal_does_not_show_the_abandoned_querys_rows(cx: &mut TestAppContext) {
    let (_src, mut vcx, window) = setup("stale", cx);
    open_modal(&window, cx);

    // 按键用 `window.press`（同步派发，不泵调度器）：`simulate_keystrokes` 会顺带
    // 跑一轮 parking，那样在途那趟就可能在**关闭模态之前**落地，测不到「过期结果
    // 落在重开之后的模态里」这一格。
    vcx.update(|window, app| window.press("z", app));
    // 不泵调度器：这一趟还挂在池上（或回来了但续作还没轮到跑）。
    let (query, hits, busy) = state(&window, cx);
    assert!(
        busy && hits == 0,
        "先确认查询确实在途：query={query} hits={hits} busy={busy}"
    );

    vcx.update(|window, app| window.press("escape", app));
    open_modal(&window, cx);
    // 现在才泵：在途那趟回来时趟号已过期、模态也重开过——两头都不认它。
    pump(&mut vcx, 400);
    let (query, hits, busy) = state(&window, cx);
    assert_eq!(query, "", "重开的模态搜索框是空的");
    assert_eq!(hits, 0, "过期那趟的命中不许落进新模态：{hits} 行");
    assert!(!busy, "没有新查询在途");
}

/// ③：退格到空查询就地清空，不派发也不留在屏上。
#[gpui_kit::test]
fn backspacing_to_empty_clears_without_another_query(cx: &mut TestAppContext) {
    let (_src, mut vcx, window) = setup("empty", cx);
    open_modal(&window, cx);

    cx.simulate_keystrokes(*window, "z");
    settle(&mut vcx, &window, cx);

    cx.simulate_keystrokes(*window, "backspace");
    let (query, hits, busy) = state(&window, cx);
    assert_eq!(query, "");
    assert_eq!(hits, 0, "空查询不该继续显示上一轮的 {hits} 行");
    assert!(!busy, "空查询不派发新的一趟");

    // 再泵若干拍：确认没有「空查询也派一趟、回头把命中塞回来」。
    pump(&mut vcx, 20);
    let (_q, hits, busy) = state(&window, cx);
    assert_eq!(hits, 0);
    assert!(!busy);
}
