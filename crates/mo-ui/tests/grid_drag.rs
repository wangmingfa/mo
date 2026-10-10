//! 网格 / 画廊视图的应用内拖拽——补 devlog/windows-port.md §35 欠账 3：
//! §14 手拼的鼠标拖拽此前只挂在列表行上，`grid::cell` 没接。
//!
//! 派发形状与 `internal_drag.rs`（列表视图同款覆盖）一致：源单元 `MouseDown`
//! 记拖拽、目标单元 `MouseUp` 结算，Alt 抬起 = 移动。落盘判据墙钟轮询——传输
//! 在 Mo 的进程级 tokio runtime 上跑，不受 GPUI 测试调度器驱动。
//!
//! 视图切换走真按钮（`vcx.update(|w, cx| w.click("view-mode-grid", cx))`），
//! 单元定位用 `mo-grid-cell-{条目位}`（网格/画廊的单元序号就是窗口条目序号，
//! 行序仍由 `panel_row_paths_for_tests` 查出来，不靠猜排序规则）。
//!
//! 这里还钉一条列表测试没有的契约：**拖拽接线与 `on_click` 共存**——原地按下
//! 抬起后，选中照常由 click 那条路落地，拖拽状态不残留。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, Bounds, InputEvent, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    Pixels, Point, TestAppContext, VisualTestContext, WindowHandle,
};
use mo_app::AppState;
use mo_ui::RootView;

/// 与 `internal_drag.rs` 同款 fixture：`here/` 里一个目录行 `箱子/` + 若干文件。
struct Rig {
    base: PathBuf,
    here: PathBuf,
    bin: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("mo-gdrag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let here = base.join("here");
        let bin = here.join("箱子");
        std::fs::create_dir_all(&bin).unwrap();
        Self { base, here, bin }
    }

    /// 在 `here/` 里放一个文件，返回路径。**导航前**调。
    fn file(&self, name: &str) -> PathBuf {
        let p = self.here.join(name);
        std::fs::write(&p, b"payload").unwrap();
        p
    }

    fn trash_root(&self) -> PathBuf {
        self.base.join("trash")
    }
}

/// 建窗 + 导航 + 切到指定视图按钮，等到窗口快照正好 `rows` 行且第 0 个单元已渲染。
fn open_in_mode(
    rig: &Rig,
    rows: usize,
    mode_button: &'static str,
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
    // 先等启动时那次异步「按需打开 Home」落地，再导航（同 os_drop.rs 的坑位）。
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
    // 真点工具栏按钮切视图（不是改内部状态）。
    vcx.update(|window, cx| window.click(mode_button, cx));
    vcx.run_until_parked();
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        if vcx.debug_bounds("mo-grid-cell-0").is_some() {
            return (vcx, window);
        }
    }
    panic!("切到视图按钮 {mode_button} 后网格单元没渲染出来");
}

fn center(vcx: &mut VisualTestContext, selector: String) -> Point<Pixels> {
    let s: &'static str = Box::leak(selector.into_boxed_str());
    let b: Bounds<Pixels> = vcx
        .debug_bounds(s)
        .unwrap_or_else(|| panic!("{s} 没有出现在渲染帧里"));
    b.origin + point(b.size.width / 2.0, b.size.height / 2.0)
}

/// 当前渲染窗口每一行的 `(路径, 是否目录)`。⚠️ 读状态走外层 `TestAppContext`
/// （`vcx.update` 递进来的 `App` 查不到这个窗口，见 internal_drag.rs 同款注释）。
fn rows(tcx: &mut TestAppContext, window: &WindowHandle<RootView>) -> Vec<(PathBuf, bool)> {
    window
        .update(tcx, |root, _w, _cx| mo_ui::panel_row_paths_for_tests(root))
        .expect("读行")
}

fn sel_count(tcx: &mut TestAppContext, window: &WindowHandle<RootView>) -> usize {
    window
        .update(tcx, |root, _w, _cx| {
            mo_ui::panel_selection_count_for_tests(root)
        })
        .expect("读选区")
}

fn cell_at(i: usize) -> String {
    format!("mo-grid-cell-{i}")
}

/// 在 `from` 按下左键、在 `to` 抬起（`alt` = 按住 Alt 抬）。中间拍一帧。
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

fn dir_row(rs: &[(PathBuf, bool)]) -> usize {
    rs.iter()
        .position(|(_, d)| *d)
        .expect("fixture 的目录单元没在窗口快照里")
}

fn file_row(rs: &[(PathBuf, bool)], name: &str) -> usize {
    rs.iter()
        .position(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(name))
        .unwrap_or_else(|| panic!("文件单元 {name} 没在窗口快照里：{rs:?}"))
}

/// 同卷宗直拖文件单元到目录单元 = **移动**进去、源没了（§35-3 的接线 +
/// §43 的资源管理器默认）。
#[gpui_kit::test]
fn grid_drag_file_cell_onto_directory_cell_moves_it_in(cx: &mut TestAppContext) {
    let rig = Rig::new("move");
    let src = rig.file("note.txt");
    let (mut vcx, window) = open_in_mode(&rig, 2, "view-mode-grid", cx);

    let rs = rows(cx, &window);
    let from = center(&mut vcx, cell_at(file_row(&rs, "note.txt")));
    let to = center(&mut vcx, cell_at(dir_row(&rs)));
    drag(&mut vcx, from, to, false);

    let dest = rig.bin.join("note.txt");
    assert!(
        wait_until(false, &dest),
        "拖到目录单元后 {dest:?} 没出现：网格没接上行级拖放链"
    );
    assert!(
        wait_until(true, &src),
        "同卷宗直拖该是移动（§43）：网格的源文件还留在原地"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 按住 Alt 拖 = 移动（§43 保留的移动别名）：网格接线要把 `ev.modifiers`
/// 一路带到 `run_transfer`。
#[gpui_kit::test]
fn grid_drag_with_alt_moves_instead_of_copying(cx: &mut TestAppContext) {
    let rig = Rig::new("alt");
    let src = rig.file("中文.txt");
    let (mut vcx, window) = open_in_mode(&rig, 2, "view-mode-grid", cx);

    let rs = rows(cx, &window);
    let from = center(&mut vcx, cell_at(file_row(&rs, "中文.txt")));
    let to = center(&mut vcx, cell_at(dir_row(&rs)));
    drag(&mut vcx, from, to, true);

    let dest = rig.bin.join("中文.txt");
    assert!(
        wait_until(true, &src),
        "Alt 拖完源文件还在：网格的 Alt=移动没走起来"
    );
    assert!(dest.is_file(), "Alt 拖完目标位置没有文件");
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 同一单元按下抬起 = 点击：不传输，**且选中照常**（grid 的 `on_click` 与新的
/// down/up 接线共存），随后一次真拖照常工作。
#[gpui_kit::test]
fn grid_press_and_release_is_a_click_that_still_selects(cx: &mut TestAppContext) {
    let rig = Rig::new("click");
    let src = rig.file("note.txt");
    let (mut vcx, window) = open_in_mode(&rig, 2, "view-mode-grid", cx);

    let rs = rows(cx, &window);
    let on_file = center(&mut vcx, cell_at(file_row(&rs, "note.txt")));
    // 首点偶发丢失（headless 调度竞争，与 list_source_panel 同款 flake）：轮询选中态，
    // 确认仍 0（首次确已丢失）时才在 round 30 补点一次。点击是 toggle，不能无脑重复
    // 派发，否则 0→1→0 反而取消选中。
    drag(&mut vcx, on_file, on_file, false);
    let mut selected = false;
    for round in 0..100 {
        vcx.update(|window, cx| window.render_frame(cx));
        let sel = sel_count(cx, &window);
        if sel == 1 {
            selected = true;
            break;
        }
        if round == 30 && sel == 0 {
            drag(&mut vcx, on_file, on_file, false);
        }
        vcx.run_until_parked();
    }
    assert!(selected, "点击单元没落地选中——拖拽接线把 click 挤掉了");
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        std::fs::read_dir(&rig.bin).map(|d| d.count()).unwrap_or(0),
        0,
        "原地按下抬起被当成了拖拽传输"
    );
    assert!(src.exists(), "原地点击不该动文件");

    let to = center(&mut vcx, cell_at(dir_row(&rs)));
    drag(&mut vcx, on_file, to, false);
    assert!(
        wait_until(false, &rig.bin.join("note.txt")),
        "被点击消费过一次之后，网格拖拽链再也不工作了"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 拖到**文件单元**上：谁都不接（`drop_on_entry` 拒非目录，同窗格不回抛窗格兜底）。
#[gpui_kit::test]
fn grid_drag_onto_file_cell_transfers_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new("no-file-drop");
    let a = rig.file("a.txt");
    let b = rig.file("b.txt");
    let (mut vcx, window) = open_in_mode(&rig, 3, "view-mode-grid", cx);

    let rs = rows(cx, &window);
    let from = center(&mut vcx, cell_at(file_row(&rs, "a.txt")));
    let to = center(&mut vcx, cell_at(file_row(&rs, "b.txt")));
    drag(&mut vcx, from, to, false);

    std::thread::sleep(Duration::from_millis(250));
    assert!(a.exists() && b.exists(), "文件单元不该接住拖拽");
    assert_eq!(
        std::fs::read_dir(&rig.bin).map(|d| d.count()).unwrap_or(0),
        0,
        "落在文件单元上的拖拽漏进了窗格兜底"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 画廊与网格共用同一个 `grid::cell`——换到画廊按钮，拖放链必须照常。
#[gpui_kit::test]
fn gallery_shares_the_same_drag_wiring(cx: &mut TestAppContext) {
    let rig = Rig::new("gallery");
    let src = rig.file("note.txt");
    let (mut vcx, window) = open_in_mode(&rig, 2, "view-mode-gallery", cx);

    let rs = rows(cx, &window);
    let from = center(&mut vcx, cell_at(file_row(&rs, "note.txt")));
    let to = center(&mut vcx, cell_at(dir_row(&rs)));
    drag(&mut vcx, from, to, false);

    assert!(
        wait_until(false, &rig.bin.join("note.txt")),
        "画廊里拖到目录单元没落盘：两种视图不是同一条接线"
    );
    assert!(
        wait_until(true, &src),
        "同卷宗直拖该是移动（§43）：画廊里源文件还留着"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}
