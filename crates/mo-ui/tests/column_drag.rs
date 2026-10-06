//! 列视图（Miller 列）的拖拽——补 devlog/windows-port.md §37/§38 记的最后一块：
//! `columns.rs` 此前两种拖拽都没接（只有右键）。
//!
//! 与 `internal_drag.rs` / `grid_drag.rs` 同一套派发姿势：源行 `MouseDown` 记拖拽
//! （列视图走单源版 `begin_drag_single`，条目没有 FileId 选区）、目标行 `MouseUp`
//! 结算；OS 拖入投真的 `FileDropEvent::{Entered, Submit}`。落盘判据墙钟轮询。
//!
//! 行定位用 `mo-col-row-{pane}-{tab}-{列}-{行}`，第 0 列的条目表由
//! `column_rows_for_tests` 查（列视图数据不走主目录窗口，问不得快照行表）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, App, Bounds, ExternalPaths, FileDropEvent, InputEvent, Modifiers, MouseButton,
    MouseDownEvent, MouseUpEvent, Pixels, Point, TestAppContext, VisualTestContext, Window,
    WindowHandle,
};
use mo_app::AppState;
use mo_ui::RootView;

/// `here/`：一个目录 `箱子/` + 文件若干。
struct Rig {
    base: PathBuf,
    here: PathBuf,
    bin: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("mo-cdrag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let here = base.join("here");
        let bin = here.join("箱子");
        std::fs::create_dir_all(&bin).unwrap();
        Self { base, here, bin }
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

/// 建窗 + 导航 + 切列视图，等到第 0 列正好 `rows` 条且首行已渲染。
fn open_in_columns(
    rig: &Rig,
    rows: usize,
    cx: &mut TestAppContext,
) -> (VisualTestContext, WindowHandle<RootView>) {
    mo_ui::isolate_user_dirs_for_tests();
    cx.dispatcher.allow_parking();
    let app = AppState::with_trash(rig.trash_root());
    let window = cx.open_window(size(px(1000.), px(700.)), move |_, cx| {
        RootView::new(app.clone(), cx)
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    // 先等启动时异步「打开 Home」落地再导航（os_drop.rs 同款坑位）。
    for _ in 0..200 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
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
        vcx.update(|window, cx| window.render_frame(cx));
        let ready = window
            .update(cx, |root, _w, _cx| {
                mo_ui::panel_window_ready_for_tests(root, rows)
            })
            .unwrap_or(false);
        if ready && vcx.debug_bounds("mo-file-row-0").is_some() {
            break;
        }
    }
    // 真点工具栏按钮切列视图；`ensure_columns` 每帧补根列。
    vcx.update(|window, cx| window.click("view-mode-columns", cx));
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        let n = window
            .update(cx, |root, _w, _cx| mo_ui::column_rows_for_tests(root))
            .expect("读列");
        if n.len() == rows && vcx.debug_bounds("mo-col-row-0-0-0-0").is_some() {
            return (vcx, window);
        }
    }
    panic!("切列视图后第 0 列没等到 {rows} 行");
}

/// 第 0 列第 `i` 行的中心。
fn row_center(vcx: &mut VisualTestContext, i: usize) -> Point<Pixels> {
    let s: &'static str = Box::leak(format!("mo-col-row-0-0-0-{i}").into_boxed_str());
    let b: Bounds<Pixels> = vcx
        .debug_bounds(s)
        .unwrap_or_else(|| panic!("{s} 没有出现在渲染帧里"));
    b.origin + point(b.size.width / 2.0, b.size.height / 2.0)
}

/// 第 0 列的 `(路径, 是否目录)`。⚠️ 读状态走外层 `TestAppContext`（同其他拖拽测试）。
fn rows(tcx: &mut TestAppContext, window: &WindowHandle<RootView>) -> Vec<(PathBuf, bool)> {
    window
        .update(tcx, |root, _w, _cx| mo_ui::column_rows_for_tests(root))
        .expect("读列")
}

fn selector_row(rs: &[(PathBuf, bool)], name: &str) -> usize {
    rs.iter()
        .position(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(name))
        .unwrap_or_else(|| panic!("行 {name} 没在第 0 列里：{rs:?}"))
}

fn dir_row(rs: &[(PathBuf, bool)]) -> usize {
    rs.iter().position(|(_, d)| *d).expect("第 0 列没有目录行")
}

/// 按下→拍一帧→在 `to` 抬起。
fn drag(vcx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>, alt: bool) {
    vcx.update(|window, cx| {
        window.dispatch_event(
            InputEvent::to_platform_input(MouseDownEvent {
                button: MouseButton::Left,
                position: from,
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
                position: to,
                modifiers: Modifiers {
                    alt,
                    ..Default::default()
                },
                click_count: 1,
            }),
            cx,
        );
        window.render_frame(cx);
    });
    vcx.run_until_parked();
}

/// 模拟一次外部拖放（同 os_drop.rs）。
fn drop_files(vcx: &mut VisualTestContext, at: Point<Pixels>, paths: Vec<PathBuf>) {
    vcx.update(|window: &mut Window, cx: &mut App| {
        window.dispatch_event(
            FileDropEvent::Entered {
                position: at,
                paths: ExternalPaths(paths.into_iter().collect()),
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            FileDropEvent::Submit { position: at }.to_platform_input(),
            cx,
        );
    });
    vcx.run_until_parked();
}

fn wait_until(gone: bool, p: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if p.exists() != gone {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 拖文件行到目录行 = **移动**进去（§43：同卷宗直拖跟资源管理器一样是移动；
/// `begin_drag_single` → `drop_on_entry` → `run_transfer`，与列表 / 网格同一个结算机器）。
#[gpui_kit::test]
fn column_drag_file_row_onto_directory_row_moves_it_in(cx: &mut TestAppContext) {
    let rig = Rig::new("move");
    let src = rig.file("note.txt");
    let (mut vcx, window) = open_in_columns(&rig, 2, cx);

    let rs = rows(cx, &window);
    let from = row_center(&mut vcx, selector_row(&rs, "note.txt"));
    let to = row_center(&mut vcx, dir_row(&rs));
    drag(&mut vcx, from, to, false);

    let dest = rig.bin.join("note.txt");
    assert!(
        wait_until(false, &dest),
        "列视图拖到目录行后 {dest:?} 没出现：接线没挂上"
    );
    assert!(
        wait_until(true, &src),
        "同卷宗直拖该是移动（§43）：列视图的源文件还留在原地"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// Alt 抬起 = 移动**别名**（§43 保留了 §36 的肌肉记忆）：`ev.modifiers` 要从列行
/// 一路带到 `run_transfer`。
#[gpui_kit::test]
fn column_drag_with_alt_moves_instead_of_copying(cx: &mut TestAppContext) {
    let rig = Rig::new("alt");
    let src = rig.file("中文.txt");
    let (mut vcx, window) = open_in_columns(&rig, 2, cx);

    let rs = rows(cx, &window);
    let from = row_center(&mut vcx, selector_row(&rs, "中文.txt"));
    let to = row_center(&mut vcx, dir_row(&rs));
    drag(&mut vcx, from, to, true);

    assert!(
        wait_until(true, &src),
        "Alt 拖完源文件还在：列视图的 Alt=移动没走起来"
    );
    assert!(rig.bin.join("中文.txt").is_file(), "目标位置没有文件");
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 同行按下抬起 = 点击不是拖拽（同地判据），且拖拽状态没卡死（随后真拖照常）。
#[gpui_kit::test]
fn column_press_and_release_is_a_click_not_a_drag(cx: &mut TestAppContext) {
    let rig = Rig::new("click");
    let src = rig.file("note.txt");
    let (mut vcx, window) = open_in_columns(&rig, 2, cx);

    let rs = rows(cx, &window);
    let on_file = row_center(&mut vcx, selector_row(&rs, "note.txt"));
    drag(&mut vcx, on_file, on_file, false);

    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        std::fs::read_dir(&rig.bin).map(|d| d.count()).unwrap_or(0),
        0,
        "原地按下抬起被当成了拖拽传输"
    );
    assert!(src.exists(), "原地点击不该动文件");

    let to = row_center(&mut vcx, dir_row(&rs));
    drag(&mut vcx, on_file, to, false);
    assert!(
        wait_until(false, &rig.bin.join("note.txt")),
        "被点击消费过一次之后，列视图拖拽链再也不工作了"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 同窗格拖到**文件行**上：谁都不接，盘上不动。
#[gpui_kit::test]
fn column_drag_onto_file_row_transfers_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new("no-file-drop");
    let a = rig.file("a.txt");
    let b = rig.file("b.txt");
    let (mut vcx, window) = open_in_columns(&rig, 3, cx);

    let rs = rows(cx, &window);
    let from = row_center(&mut vcx, selector_row(&rs, "a.txt"));
    let to = row_center(&mut vcx, selector_row(&rs, "b.txt"));
    drag(&mut vcx, from, to, false);

    std::thread::sleep(Duration::from_millis(250));
    assert!(a.exists() && b.exists(), "文件行不该接住拖拽");
    assert_eq!(
        std::fs::read_dir(&rig.bin).map(|d| d.count()).unwrap_or(0),
        0,
        "落在文件行上的拖拽漏进了窗格兜底"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// OS 拖入落在列视图的目录行 = 复制进**那个目录**（行级监听先于窗格兜底）。
#[gpui_kit::test]
fn column_os_drop_on_directory_row_copies_into_that_directory(cx: &mut TestAppContext) {
    let rig = Rig::new("os-dir");
    rig.file("sibling.txt");
    let (mut vcx, window) = open_in_columns(&rig, 2, cx);

    let rs = rows(cx, &window);
    let at = row_center(&mut vcx, dir_row(&rs));
    let src = rig.base.join("payload.txt");
    std::fs::write(&src, b"payload").unwrap();
    drop_files(&mut vcx, at, vec![src.clone()]);

    let dest = rig.bin.join("payload.txt");
    assert!(
        wait_until(false, &dest),
        "拖到列视图目录行后 {dest:?} 没出现：目录行没挂行级 drop 监听"
    );
    assert!(
        !rig.here.join("payload.txt").exists(),
        "进了当前目录 = 事件漏到窗格兜底，行级监听没接住"
    );
    assert!(src.exists(), "复制不该动源文件");
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// OS 拖入落在列视图的**文件行**：不注册监听 → 冒到窗格兜底、进当前目录。
#[gpui_kit::test]
fn column_os_drop_on_file_row_bubbles_to_pane_fallback(cx: &mut TestAppContext) {
    let rig = Rig::new("os-file");
    rig.file("sibling.txt");
    let (mut vcx, window) = open_in_columns(&rig, 2, cx);

    let rs = rows(cx, &window);
    let at = row_center(&mut vcx, selector_row(&rs, "sibling.txt"));
    let src = rig.base.join("loose.txt");
    std::fs::write(&src, b"payload").unwrap();
    drop_files(&mut vcx, at, vec![src.clone()]);

    let dest = rig.here.join("loose.txt");
    assert!(
        wait_until(false, &dest),
        "拖到列视图文件行该冒泡进窗格兜底（当前目录）：{dest:?} 没出现——监听被吞了？"
    );
    assert!(
        !rig.bin.join("loose.txt").exists(),
        "落点该是当前目录，不该同时进目录行"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}
