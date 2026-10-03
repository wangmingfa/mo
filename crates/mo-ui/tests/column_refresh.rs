//! 列视图的同目录刷新——补 devlog/windows-port.md §40 缺口 3（P1）：
//! `ensure_columns` 此前只按「第 0 列路径 ≠ 当前目录」判过期，目录**没换、
//! 内容变了**（新增 / 删除 / 改名落在正显示的列里）时列永远显示旧条目。
//!
//! 接的是事件侧：`tab_loop` 收到 `AppEvent::DirectoryChanged`（watcher /
//! 刷新链路本就在发，列表视图靠它回灌）且该路径正被某一列显示 →
//! `mark_column_stale` 竖牌 → 下一拍 `ensure_columns` 走 `reload_stale_columns`
//! **原地重读**——列栈结构、下钻深度、cursor 都保住。
//!
//! 测试用 `app.bus().publish(...)` 投这个事件：headless 里首标签页的 watcher 泵
//! 不在（生产由 `run()` 起），投的是**发布方语义不变**的事实信号，被测的是
//! 消费侧这条新链。列视图数据面用 `column_rows_for_tests` /
//! `column_rows_at_for_tests` / `column_cursor_for_tests`；点行按坐标派发
//! （`window.click` 够不着虚拟化区，§40 harness 记）。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, App, Bounds, InputEvent, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    Pixels, Point, TestAppContext, VisualTestContext, Window, WindowHandle,
};
use mo_app::AppState;
use mo_core::AppEvent;
use mo_ui::RootView;

/// `here/`：目录 `box/`、`sub/`（内含 `deep.txt`）+ 文件 `note.txt`、`keep.txt`。
struct Rig {
    base: PathBuf,
    here: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("mo-colref-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let here = base.join("here");
        std::fs::create_dir_all(here.join("box")).unwrap();
        let sub = here.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("deep.txt"), b"deep").unwrap();
        std::fs::write(here.join("note.txt"), b"payload").unwrap();
        std::fs::write(here.join("keep.txt"), b"payload").unwrap();
        Self { base, here }
    }

    fn file(&self, name: &str) -> PathBuf {
        let p = self.here.join(name);
        std::fs::write(&p, b"payload").unwrap();
        p
    }

    fn trash_root(&self) -> PathBuf {
        self.base.join("trash")
    }
}

/// 建窗 + 导航 + 切列视图，等第 0 列正好 `rows` 条且首行已渲染。
/// 把 `AppState` 一并交回：投 `DirectoryChanged` 要握着它的总线。
fn open_in_columns(
    rig: &Rig,
    rows: usize,
    cx: &mut TestAppContext,
) -> (VisualTestContext, WindowHandle<RootView>, AppState) {
    mo_ui::isolate_user_dirs_for_tests();
    cx.dispatcher.allow_parking();
    let app = AppState::with_trash(rig.trash_root());
    let app_init = app.clone();
    let window = cx.open_window(size(px(1000.), px(700.)), move |_, cx| {
        RootView::new(app_init.clone(), cx)
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    for _ in 0..200 {
        vcx.run_until_parked();
        vcx.update(|window: &mut Window, cx: &mut App| window.render_frame(cx));
        if window
            .update(cx, |root, _w, _cx| mo_ui::panel_path_for_tests(root))
            .ok()
            .flatten()
            .is_some()
        {
            break;
        }
    }
    let target = rig.here.clone();
    window
        .update(cx, |root, _window, cx| {
            mo_ui::navigate_for_tests(root, target, cx)
        })
        .expect("导航失败");
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window: &mut Window, cx: &mut App| window.render_frame(cx));
        let ready = window
            .update(cx, |root, _w, _cx| {
                mo_ui::panel_window_ready_for_tests(root, rows)
            })
            .unwrap_or(false);
        if ready && vcx.debug_bounds("mo-file-row-0").is_some() {
            break;
        }
    }
    vcx.update(|window: &mut Window, cx: &mut App| window.click("view-mode-columns", cx));
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window: &mut Window, cx: &mut App| window.render_frame(cx));
        let n = window
            .update(cx, |root, _w, _cx| mo_ui::column_rows_for_tests(root))
            .expect("读列");
        if n.len() == rows && vcx.debug_bounds("mo-col-row-0-0-0-0").is_some() {
            return (vcx, window, app);
        }
    }
    panic!("切列视图后第 0 列没等到 {rows} 行");
}

/// 第 0 列第 `i` 行按坐标单击（中央虚拟化区，`window.click` 够不着）。
fn click_row(vcx: &mut VisualTestContext, selector: &str) {
    let s: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let b: Bounds<Pixels> = vcx
        .debug_bounds(s)
        .unwrap_or_else(|| panic!("{s} 没有出现在渲染帧里"));
    let at: Point<Pixels> = b.origin + point(b.size.width / 2.0, b.size.height / 2.0);
    vcx.update(|window: &mut Window, cx: &mut App| {
        window.dispatch_event(
            InputEvent::to_platform_input(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.render_frame(cx);
        window.dispatch_event(
            InputEvent::to_platform_input(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
        window.render_frame(cx);
    });
    vcx.run_until_parked();
}

fn col0(tcx: &mut TestAppContext, window: &WindowHandle<RootView>) -> Vec<(PathBuf, bool)> {
    window
        .update(tcx, |root, _w, _cx| mo_ui::column_rows_for_tests(root))
        .expect("读第 0 列")
}

fn has(rows: &[(PathBuf, bool)], name: &str) -> bool {
    rows.iter()
        .any(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(name))
}

fn row_of(rows: &[(PathBuf, bool)], name: &str) -> usize {
    rows.iter()
        .position(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(name))
        .unwrap_or_else(|| panic!("行 {name} 没在第 0 列里：{rows:?}"))
}

/// 拍帧 + 墙钟轮询：第 0 列出现 / 消失某条目为止（重读的落地要跨调度器）。
fn wait_col0(
    vcx: &mut VisualTestContext,
    tcx: &mut TestAppContext,
    window: &WindowHandle<RootView>,
    name: &str,
    want: bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        vcx.run_until_parked();
        vcx.update(|window: &mut Window, cx: &mut App| window.render_frame(cx));
        let rows = col0(tcx, window);
        if has(&rows, name) == want {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 契约 1：目录没换、磁盘多了个文件——投一条 `DirectoryChanged`，
/// 第 0 列要**原地**把它读出来（此前的行为：永远看不见）。
#[gpui_kit::test]
fn directory_changed_event_brings_new_file_into_the_column(cx: &mut TestAppContext) {
    let rig = Rig::new("add");
    let (mut vcx, window, app) = open_in_columns(&rig, 4, cx);

    rig.file("extra.txt");
    app.bus().publish(AppEvent::DirectoryChanged {
        path: rig.here.clone(),
    });
    assert!(
        wait_col0(&mut vcx, cx, &window, "extra.txt", true),
        "收到目录变更广播后第 0 列没把新增的 extra.txt 读出来"
    );
    // 旧条目一根不少：是「重读」，不是清台。
    let rows = col0(cx, &window);
    assert_eq!(rows.len(), 5, "重读后应是 5 条：{rows:?}");
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 契约 2：重读发生在**下钻之后**——列栈结构和深层列都得保住，被删行的
/// cursor 钳进新表；且事件路径只有和显示中的列对得上才竖牌（对照组：
/// 广播别的目录不惊动这一页）。
#[gpui_kit::test]
fn refresh_keeps_drill_down_and_clamps_cursor(cx: &mut TestAppContext) {
    let rig = Rig::new("drill");
    let (mut vcx, window, app) = open_in_columns(&rig, 4, cx);

    // 下钻：点 `sub` 目录行 → 第 1 列读出来。
    let rows = col0(cx, &window);
    let sub = row_of(&rows, "sub");
    click_row(&mut vcx, &format!("mo-col-row-0-0-0-{sub}"));
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window: &mut Window, cx: &mut App| window.render_frame(cx));
        let deep = window
            .update(cx, |root, _w, _cx| mo_ui::column_rows_at_for_tests(root, 1))
            .expect("读深层列");
        if !deep.is_empty() {
            break;
        }
    }
    // 对照：广播一个没在显示的目录——不该触发重读（extra2 不该冒出来）。
    // 要**按墙钟按住**再判：竖牌错了路，重读的异步也要时间落地才看得见红。
    std::fs::write(rig.here.join("extra2.txt"), b"payload").unwrap();
    app.bus().publish(AppEvent::DirectoryChanged {
        path: rig.base.join("没在看的目录"),
    });
    let hold = Instant::now() + Duration::from_millis(800);
    while Instant::now() < hold {
        vcx.run_until_parked();
        vcx.update(|window: &mut Window, cx: &mut App| window.render_frame(cx));
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !has(&col0(cx, &window), "extra2.txt"),
            "没在显示的目录的广播不该惊动这一页"
        );
    }

    // 真广播：删掉两枚文件，重读后第 0 列缩、第 1 列还在、cursor 钳进新表。
    // 点**最后一行**（note.txt），删完它必须被钳——不钳就越界。
    let rows = col0(cx, &window);
    let note = row_of(&rows, "note.txt");
    click_row(&mut vcx, &format!("mo-col-row-0-0-0-{note}"));
    assert_eq!(
        note,
        rows.len() - 1,
        "fixture 排序里 note.txt 必须落在最后一行（clamp 前置自检）"
    );
    let cur = window
        .update(cx, |root, _w, _cx| mo_ui::column_cursor_for_tests(root, 0))
        .expect("读 cursor")
        .expect("有第 0 列");
    assert_eq!(cur, note, "点行后 cursor 就该停在那一行（前置自检）");
    std::fs::remove_file(rig.here.join("note.txt")).unwrap();
    std::fs::remove_file(rig.here.join("keep.txt")).unwrap();
    app.bus().publish(AppEvent::DirectoryChanged {
        path: rig.here.clone(),
    });
    assert!(
        wait_col0(&mut vcx, cx, &window, "note.txt", false),
        "同目录删除后第 0 列还留着旧行"
    );
    let rows = col0(cx, &window);
    // extra2.txt 是「没在看的目录」广播期间写进磁盘的，重读把它如实带出来。
    assert!(
        has(&rows, "extra2.txt") && has(&rows, "box") && has(&rows, "sub"),
        "重读结果不对：{rows:?}"
    );
    let deep = window
        .update(cx, |root, _w, _cx| mo_ui::column_rows_at_for_tests(root, 1))
        .expect("读深层列");
    assert!(
        has(&deep, "deep.txt"),
        "原地重读不该拆了下钻出来的第 1 列：{deep:?}"
    );
    let cur = window
        .update(cx, |root, _w, _cx| mo_ui::column_cursor_for_tests(root, 0))
        .expect("读 cursor")
        .expect("有第 0 列");
    assert_eq!(
        cur,
        rows.len() - 1,
        "点掉的最后一行该被钳到新表末行（{} 条，实际 cursor={cur}）",
        rows.len()
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 契约 3：根列过期（目录换了）走重建，竖着的过期牌随旧列栈一起作废——
/// 重建读的就是此刻磁盘，不该再多刷一拍。
#[gpui_kit::test]
fn root_column_rebuild_supersedes_a_pending_refresh(cx: &mut TestAppContext) {
    let rig = Rig::new("rebuild");
    let (mut vcx, window, app) = open_in_columns(&rig, 4, cx);

    // 先竖牌（第 0 列正显示 here），再立刻换目录：here2。
    rig.file("ghost.txt");
    app.bus().publish(AppEvent::DirectoryChanged {
        path: rig.here.clone(),
    });
    let here2 = rig.base.join("here2");
    std::fs::create_dir_all(&here2).unwrap();
    std::fs::write(here2.join("only.txt"), b"payload").unwrap();
    let target = here2.clone();
    window
        .update(cx, |root, _window, cx| {
            mo_ui::navigate_for_tests(root, target, cx)
        })
        .expect("导航失败");
    assert!(
        wait_col0(&mut vcx, cx, &window, "only.txt", true),
        "换目录后第 0 列没重建到 here2 的内容"
    );
    assert!(
        !has(&col0(cx, &window), "ghost.txt"),
        "重建后的第 0 列不该混进旧目录（here）重读的结果"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}
