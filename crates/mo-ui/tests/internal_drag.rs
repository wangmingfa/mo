//! 应用内拖拽（鼠标手拼那一套）的 headless 测试——补 devlog/windows-port.md §35
//! 点的欠账 2：`begin_drag` / `drop_on_entry` 这条链此前**零**直接测试，底下引擎
//! （mo-app 的 `transfer_between`）有覆盖，缺的是 UI 派发这一段。
//!
//! 派发形状与真机一致：源行上 `MouseDown`（gpui 记选中 + `begin_drag`），目标行上
//! `MouseUp`（`drop_on_entry` 结算；Alt 抬起 = 移动）。传输在 Mo 的进程级 tokio
//! runtime 上跑（不受 GPUI 测试调度器驱动），所以落盘判据一律墙钟轮询——与
//! tests/os_drop.rs 同一条理由。
//!
//! 行序**不靠猜**（目录是否排前面受排序规则影响）：每条测试先用
//! `panel_row_paths_for_tests` 把「目录行 / 文件行是第几行」查出来，再摆鼠标。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, Bounds, InputEvent, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    Pixels, Point, TestAppContext, VisualTestContext, WindowHandle,
};
use mo_app::AppState;
use mo_ui::RootView;

/// 一套互不干扰的临时目录：`here/` 里一个目录行 `箱子/` + 若干文件行。
struct Rig {
    base: PathBuf,
    here: PathBuf,
    /// 唯一的目录行（拖放的靶子）。
    bin: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("mo-idrag-{tag}-{}", std::process::id()));
        // 上一轮的同名残留必须先清（pid 会被复用，否则读到旧文件）。
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

/// 建窗 + 真导航到 `here/`，等到窗口快照正好 `rows` 行且第 0 行已渲染。
/// （启动时那次异步「按需打开 Home」会盖导航——先等它落地，tests/os_drop.rs 同款。）
fn open_here(
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
            return (vcx, window);
        }
    }
    panic!("导航到 {:?} 后没等到 {rows} 行", rig.here);
}

fn center(vcx: &mut VisualTestContext, selector: String) -> Point<Pixels> {
    // `debug_bounds` 收 `&'static str`；测试进程里漏几个 selector 串无所谓。
    let s: &'static str = Box::leak(selector.into_boxed_str());
    let b: Bounds<Pixels> = vcx
        .debug_bounds(s)
        .unwrap_or_else(|| panic!("{s} 没有出现在渲染帧里"));
    b.origin + point(b.size.width / 2.0, b.size.height / 2.0)
}

/// 当前渲染窗口每一行的 `(路径, 是否目录)`。
///
/// ⚠️ 读状态走外层的 `TestAppContext`（layout.rs 一以贯之的姿势）：
/// `VisualTestContext::update` 递进来的那个 `App` 在 `from_window` 这条路上查不到
/// 这个窗口（「window not found」）——派发鼠标事件用 vcx、查状态用 cx，两套并存。
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

/// 行号 → selector。
fn selector(i: usize) -> String {
    format!("mo-file-row-{i}")
}

/// 在 `from` 按下左键、在 `to` 抬起（`alt` = 按住 Alt 抬）。中间拍一帧，
/// 让按下那次的副作用（选中、`begin_drag`）先落地。
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

/// 等文件出现 / 消失。传输在进程级 tokio runtime 上跑，墙钟轮询。
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

/// 目录行的行号（fixture 只有一个目录，取第一条目录行）。
fn dir_row(rs: &[(PathBuf, bool)]) -> usize {
    rs.iter()
        .position(|(_, d)| *d)
        .expect("fixture 的目录行没在窗口快照里")
}

/// 名字所在行的行号。
fn file_row(rs: &[(PathBuf, bool)], name: &str) -> usize {
    rs.iter()
        .position(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(name))
        .unwrap_or_else(|| panic!("文件行 {name} 没在窗口快照里：{rs:?}"))
}

/// 拖文件行到目录行 = 复制进去，源留着（`begin_drag` → `drop_on_entry` →
/// `run_transfer(move_=false)` 整条 UI 派发链）。
#[gpui_kit::test]
fn drag_file_row_onto_directory_row_copies_it_in(cx: &mut TestAppContext) {
    let rig = Rig::new("copy");
    let src = rig.file("note.txt");
    let (mut vcx, window) = open_here(&rig, 2, cx);

    let rs = rows(cx, &window);
    let from = center(&mut vcx, selector(file_row(&rs, "note.txt")));
    let to = center(&mut vcx, selector(dir_row(&rs)));
    drag(&mut vcx, from, to, false);

    let dest = rig.bin.join("note.txt");
    assert!(
        wait_until(false, &dest),
        "拖到目录行后 {dest:?} 没出现：行级拖放链没接上"
    );
    assert!(src.exists(), "复制不该动源文件");
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 按住 Alt 拖到目录行 = **移动**：源没了、对面有了。`drop_on_entry` 收的是
/// `ev.modifiers.alt`，这条把「Alt 语义」从鼠标事件一路钉到落盘。
#[gpui_kit::test]
fn drag_with_alt_moves_instead_of_copying(cx: &mut TestAppContext) {
    let rig = Rig::new("alt");
    let src = rig.file("中文.txt");
    let (mut vcx, window) = open_here(&rig, 2, cx);

    let rs = rows(cx, &window);
    let from = center(&mut vcx, selector(file_row(&rs, "中文.txt")));
    let to = center(&mut vcx, selector(dir_row(&rs)));
    drag(&mut vcx, from, to, true);

    let dest = rig.bin.join("中文.txt");
    assert!(
        wait_until(true, &src),
        "Alt 拖完源文件还在：移动没走 `move_ = true` 那一支"
    );
    assert!(dest.is_file(), "Alt 拖完目标位置没有文件");
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 同一行按下再抬起 = 普通点击，**不是**拖拽（`drop_on_entry` 的原地判据）。
/// 顺带钉「拖拽状态被这条点击消费干净」：之后再正常拖一次，照常工作。
#[gpui_kit::test]
fn press_and_release_on_the_same_row_is_a_click_not_a_drag(cx: &mut TestAppContext) {
    let rig = Rig::new("click");
    let src = rig.file("note.txt");
    let (mut vcx, window) = open_here(&rig, 2, cx);

    let rs = rows(cx, &window);
    let on_file = center(&mut vcx, selector(file_row(&rs, "note.txt")));
    drag(&mut vcx, on_file, on_file, false);

    // 给一次「本该异步落盘」的时间窗，箱子必须还是空的。
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        std::fs::read_dir(&rig.bin).map(|d| d.count()).unwrap_or(0),
        0,
        "原地按下抬起被当成了拖拽传输"
    );
    assert!(src.exists(), "原地点击不该动文件");

    // 状态没被卡住：随后一次真拖照常结算。
    let to = center(&mut vcx, selector(dir_row(&rs)));
    drag(&mut vcx, on_file, to, false);
    assert!(
        wait_until(false, &rig.bin.join("note.txt")),
        "被点击消费过一次之后，拖拽链再也不工作了"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 同窗格拖到**文件行**上：谁都不接（`drop_on_entry` 拒非目录、同窗格不回抛给
/// 窗格级），盘上必须一动不动。
#[gpui_kit::test]
fn drag_onto_a_file_row_in_the_same_pane_transfers_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new("no-file-drop");
    let a = rig.file("a.txt");
    let b = rig.file("b.txt");
    let (mut vcx, window) = open_here(&rig, 3, cx);

    let rs = rows(cx, &window);
    let from = center(&mut vcx, selector(file_row(&rs, "a.txt")));
    let to = center(&mut vcx, selector(file_row(&rs, "b.txt")));
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

/// 多选拖拽：点选第一条、⇧↓ 连第二条，从选中行起拖 = **整个选中集**一起复制
/// （`begin_drag` 的 `selection.count() > 1` 分支）。
#[gpui_kit::test]
fn multi_selected_drag_copies_the_whole_set(cx: &mut TestAppContext) {
    let rig = Rig::new("multi");
    rig.file("a1.txt");
    let b = rig.file("a2.txt");
    let (mut vcx, window) = open_here(&rig, 3, cx);

    let rs = rows(cx, &window);
    let i_b = file_row(&rs, "a2.txt");
    let i_a = file_row(&rs, "a1.txt");
    // ⇧↓ 是「往下连一行」：这两条文件行必须紧挨着，否则连出来的不是预期集合
    //（排序规则变了就该改测试，别静默连到目录行上）。
    assert_eq!(
        i_a + 1,
        i_b,
        "fixture 假设两条文件行相邻，实际 a1={i_a} a2={i_b}：{rs:?}"
    );

    let on_a = center(&mut vcx, selector(i_a));
    drag(&mut vcx, on_a, on_a, false); // 普通点击选中 a1
    vcx.simulate_keystrokes("shift-down");
    vcx.run_until_parked();
    vcx.update(|window, cx| window.render_frame(cx));
    assert_eq!(sel_count(cx, &window), 2, "前置：⇧↓ 之后应连着选中两条");

    let to = center(&mut vcx, selector(dir_row(&rs)));
    drag(&mut vcx, on_a, to, false);

    assert!(
        wait_until(false, &rig.bin.join("a2.txt")),
        "多选拖拽只搬了按下那一条（或一条没搬）：a2 没进箱子"
    );
    assert!(
        rig.bin.join("a1.txt").exists() && b.exists(),
        "整个选中集都该复制过去、源都留着"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}
