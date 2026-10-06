//! 拖拽中的视觉反馈（§42）——补 Windows 上「拖动时没有任何反馈，只有松开才
//! 看到效果」这一体验缺口。契约两半：
//!
//! 1. **ghost 跟随光标**：位移过了阈值（按下起点算累计，`DRAG_ENGAGE_PX`）后
//!    渲染 `mo-drag-ghost` 浮层，位置随每次鼠标移动更新；原地按下抬起
//!    （普通点击）**永不**出现 ghost。
//! 2. **落点认领**：指针悬在目录行 / 目录单元上时 `DragState.hover` 指向它
//!    （判据与 `drop_on_entry` 对齐——亮起来的落点抬起必定传输）；移到
//!    空白处后一拍即清（新鲜度指纹：根处理器是冒泡最后一站，这一拍没人
//!    认领就过期）。
//!
//! 派发形状与 `internal_drag.rs` / `grid_drag.rs` 同款：中央虚拟化区按
//! `debug_bounds` 中心坐标派发；本文件第一次把 **MouseMove** 也打进这条链
//! （dispatch + render_frame）。状态判据走 `mo_ui::drag_probe_for_tests`
//! 直读 `DragState`——高亮的「承诺」以状态为准，底色只是它的渲染投影。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, Bounds, InputEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle,
};
use mo_app::AppState;
use mo_ui::RootView;

/// `here/`：目录 `box/` + 文件 `note.txt`、`keep.txt`（共 3 行）。
struct Rig {
    base: PathBuf,
    here: PathBuf,
    bin: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("mo-dragfb-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let here = base.join("here");
        let bin = here.join("box");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(here.join("note.txt"), b"payload").unwrap();
        std::fs::write(here.join("keep.txt"), b"payload").unwrap();
        Self { base, here, bin }
    }

    fn trash_root(&self) -> PathBuf {
        self.base.join("trash")
    }
}

/// 建窗 + 导航 + （可选）切视图按钮，等窗口快照正好 `rows` 行且首行已渲染。
/// `mode_button` 传 `"view-mode-grid"` / `"view-mode-columns"` 之类，`None` 停在列表。
fn open_here(
    rig: &Rig,
    rows: usize,
    mode_button: Option<&'static str>,
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
    // 先等启动时那次异步「按需打开 Home」落地，再导航（os_drop.rs 同款坑位）。
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
    if let Some(mode_button) = mode_button {
        vcx.update(|window, cx| window.click(mode_button, cx));
        // 等目标视图的首个可寻址单元渲染出来（网格 / 列视图各有各的 selector）。
        let probe_selector: &'static str = if mode_button == "view-mode-grid" {
            "mo-grid-cell-0"
        } else {
            "mo-col-row-0-0-0-0"
        };
        for _ in 0..100 {
            vcx.run_until_parked();
            vcx.update(|window, cx| window.render_frame(cx));
            if vcx.debug_bounds(probe_selector).is_some() {
                return (vcx, window);
            }
        }
        panic!("切到 {mode_button} 后 {probe_selector} 没渲染出来");
    }
    (vcx, window)
}

fn center(vcx: &mut VisualTestContext, selector: String) -> Point<Pixels> {
    let s: &'static str = Box::leak(selector.into_boxed_str());
    let b: Bounds<Pixels> = vcx
        .debug_bounds(s)
        .unwrap_or_else(|| panic!("{s} 没有出现在渲染帧里"));
    b.origin + point(b.size.width / 2.0, b.size.height / 2.0)
}

fn rows(tcx: &mut TestAppContext, window: &WindowHandle<RootView>) -> Vec<(PathBuf, bool)> {
    window
        .update(tcx, |root, _w, _cx| mo_ui::panel_row_paths_for_tests(root))
        .expect("读行")
}

fn probe(tcx: &mut TestAppContext, window: &WindowHandle<RootView>) -> Option<mo_ui::DragProbe> {
    window
        .update(tcx, |root, _w, _cx| mo_ui::drag_probe_for_tests(root))
        .expect("读拖拽状态")
}

fn dispatch_down(vcx: &mut VisualTestContext, at: Point<Pixels>) {
    vcx.update(|window, cx| {
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
    });
    vcx.run_until_parked();
}

fn dispatch_move(vcx: &mut VisualTestContext, at: Point<Pixels>, alt: bool) {
    vcx.update(|window, cx| {
        window.dispatch_event(
            InputEvent::to_platform_input(MouseMoveEvent {
                position: at,
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers {
                    alt,
                    ..Default::default()
                },
            }),
            cx,
        );
        window.render_frame(cx);
    });
    vcx.run_until_parked();
}

fn dispatch_up(vcx: &mut VisualTestContext, at: Point<Pixels>, alt: bool) {
    vcx.update(|window, cx| {
        window.dispatch_event(
            InputEvent::to_platform_input(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
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

fn ghost(vcx: &mut VisualTestContext) -> Option<Bounds<Pixels>> {
    vcx.debug_bounds("mo-drag-ghost")
}

/// 目录行行号 / 文件行行号（行序不靠猜，internal_drag 同款）。
fn dir_row(rs: &[(PathBuf, bool)]) -> usize {
    rs.iter()
        .position(|(_, d)| *d)
        .expect("测试目录里应有目录行")
}

fn file_row(rs: &[(PathBuf, bool)], name: &str) -> usize {
    rs.iter()
        .position(|(p, _)| {
            p.file_name()
                .map(|n| n.to_string_lossy() == name)
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("找不到文件行 {name}"))
}

/// 墙钟轮询等文件出现 / 消失（传输跑在 Mo 的进程级 tokio 上，拍帧咬不住）。
fn wait_until(exists: bool, p: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if p.exists() == exists {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// ghost 过了阈值才出现，位置逐拍跟随光标，抬起即消失。
#[gpui_kit::test]
fn ghost_follows_the_cursor_once_the_drag_engages(cx: &mut TestAppContext) {
    let rig = Rig::new("ghost-follow");
    let (mut vcx, window) = open_here(&rig, 3, None, cx);
    let rs = rows(cx, &window);
    let src = center(
        &mut vcx,
        format!("mo-file-row-{}", file_row(&rs, "note.txt")),
    );

    dispatch_down(&mut vcx, src);
    // 按下还没位移：拖拽状态记了源，但 ghost 不出现（普通点击不该闪）。
    let p = probe(cx, &window).expect("按下应记下拖拽源");
    assert!(!p.engaged, "未位移不该 engaged");
    assert!(ghost(&mut vcx).is_none(), "未位移不该有 ghost");

    let far = point(px(450.), px(420.));
    dispatch_move(&mut vcx, far, false);
    let p = probe(cx, &window).expect("拖拽进行中");
    assert!(p.engaged, "位移过阈值后应 engaged");
    assert_eq!(p.cursor, (450.0, 420.0), "光标应逐拍更新");
    let g = ghost(&mut vcx).expect("engaged 后 ghost 应渲染");
    assert!(
        g.origin.x > px(450.) && g.origin.x < px(480.) && g.origin.y > px(420.),
        "ghost 应落在光标右下方: {:?}",
        g.origin
    );

    // 再挪一拍：ghost 跟着走。
    dispatch_move(&mut vcx, point(px(300.), px(360.)), false);
    let g2 = ghost(&mut vcx).expect("ghost 应持续渲染");
    assert!(
        g2.origin.x < g.origin.x && g2.origin.y < g.origin.y,
        "ghost 应跟随"
    );

    dispatch_up(&mut vcx, point(px(300.), px(360.)), false);
    assert!(probe(cx, &window).is_none(), "抬起后拖拽状态应清空");
    assert!(ghost(&mut vcx).is_none(), "抬起后 ghost 应消失");
    // 空白处抬起（同窗格）：什么都不传输。
    assert!(rig.here.join("note.txt").exists() && !rig.bin.join("note.txt").exists());

    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 悬在目录行上：hover 认领给高亮供数据；移到空白处一拍即清。
#[gpui_kit::test]
fn directory_row_under_the_pointer_claims_the_drop_target(cx: &mut TestAppContext) {
    let rig = Rig::new("claim");
    let (mut vcx, window) = open_here(&rig, 3, None, cx);
    let rs = rows(cx, &window);
    let drow = dir_row(&rs);
    let src = center(
        &mut vcx,
        format!("mo-file-row-{}", file_row(&rs, "note.txt")),
    );
    let dst = center(&mut vcx, format!("mo-file-row-{drow}"));

    dispatch_down(&mut vcx, src);
    dispatch_move(&mut vcx, dst, false);
    let p = probe(cx, &window).expect("拖拽进行中");
    assert_eq!(
        p.hover.as_deref(),
        Some(format!("entry:{}", rig.bin.display()).as_str()),
        "目录行应认领落点"
    );

    // 移到文件行自身所在行：文件不是有效落点 → 不认领。
    dispatch_move(&mut vcx, src, false);
    assert!(
        probe(cx, &window).expect("拖拽中").hover.is_none(),
        "文件行不该被认领"
    );

    // 移到窗口空白：一拍之后没有任何认领者 → 高亮过期。
    dispatch_move(&mut vcx, point(px(450.), px(420.)), false);
    assert!(
        probe(cx, &window).expect("拖拽中").hover.is_none(),
        "空白处不该有落点"
    );

    // 回到目录行再抬起：高亮承诺的传输真的发生。
    dispatch_move(&mut vcx, dst, false);
    dispatch_up(&mut vcx, dst, false);
    assert!(
        wait_until(true, &rig.bin.join("note.txt")),
        "亮着的目录行抬起应复制进去"
    );

    let _ = std::fs::remove_dir_all(&rig.base);
}

/// Alt 的复制 / 移动语义逐拍流进拖拽状态（ghost 的文案跟着换）。
#[gpui_kit::test]
fn alt_flows_into_the_drag_state(cx: &mut TestAppContext) {
    let rig = Rig::new("alt");
    let (mut vcx, window) = open_here(&rig, 3, None, cx);
    let rs = rows(cx, &window);
    let src = center(
        &mut vcx,
        format!("mo-file-row-{}", file_row(&rs, "note.txt")),
    );
    let dst = center(&mut vcx, format!("mo-file-row-{}", dir_row(&rs)));

    dispatch_down(&mut vcx, src);
    dispatch_move(&mut vcx, dst, false);
    assert!(!probe(cx, &window).expect("拖拽中").alt, "未按 Alt = 复制");
    dispatch_move(&mut vcx, dst, true);
    assert!(probe(cx, &window).expect("拖拽中").alt, "按 Alt 应翻动 alt");

    // Alt 态抬起 = 移动：源消失、目标出现。
    dispatch_up(&mut vcx, dst, true);
    assert!(
        wait_until(true, &rig.bin.join("note.txt")),
        "Alt 抬应移动进目录"
    );
    assert!(
        wait_until(false, &rig.here.join("note.txt")),
        "移动后源应消失"
    );

    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 原地按下抬起是普通点击：ghost 全程不出现，选择照常。
#[gpui_kit::test]
fn plain_click_never_raises_the_ghost(cx: &mut TestAppContext) {
    let rig = Rig::new("click");
    let (mut vcx, window) = open_here(&rig, 3, None, cx);
    let rs = rows(cx, &window);
    let src = center(
        &mut vcx,
        format!("mo-file-row-{}", file_row(&rs, "note.txt")),
    );

    dispatch_down(&mut vcx, src);
    assert!(ghost(&mut vcx).is_none(), "按下未位移不该有 ghost");
    dispatch_up(&mut vcx, src, false);
    assert!(ghost(&mut vcx).is_none(), "抬起后更不该有 ghost");
    assert!(probe(cx, &window).is_none(), "点击后拖拽状态应清空");
    // 点击的本职没被抢走：这一行被选中了。
    let sel = window
        .update(cx, |root, _w, _cx| {
            mo_ui::panel_selection_count_for_tests(root)
        })
        .expect("读选区");
    assert_eq!(sel, 1, "原地按下抬起应照常完成单击选中");

    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 网格视图：目录单元同样认领落点（hover 状态 + 抬起兑现传输）。
#[gpui_kit::test]
fn grid_directory_cell_claims_the_drop_target(cx: &mut TestAppContext) {
    let rig = Rig::new("grid");
    let (mut vcx, window) = open_here(&rig, 3, Some("view-mode-grid"), cx);
    let rs = rows(cx, &window);
    let src = center(
        &mut vcx,
        format!("mo-grid-cell-{}", file_row(&rs, "note.txt")),
    );
    let dst = center(&mut vcx, format!("mo-grid-cell-{}", dir_row(&rs)));

    dispatch_down(&mut vcx, src);
    dispatch_move(&mut vcx, dst, false);
    let p = probe(cx, &window).expect("拖拽进行中");
    assert!(p.engaged);
    assert_eq!(
        p.hover.as_deref(),
        Some(format!("entry:{}", rig.bin.display()).as_str()),
        "目录格应认领落点"
    );
    assert!(ghost(&mut vcx).is_some(), "网格拖拽也该有 ghost");

    dispatch_up(&mut vcx, dst, false);
    assert!(
        wait_until(true, &rig.bin.join("note.txt")),
        "亮着的目录格抬起应复制进去"
    );

    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 列视图：目录行认领落点（起点走单源版 `begin_drag_single` 也要有反馈）。
#[gpui_kit::test]
fn columns_directory_row_claims_the_drop_target(cx: &mut TestAppContext) {
    let rig = Rig::new("cols");
    let (mut vcx, window) = open_here(&rig, 3, Some("view-mode-columns"), cx);
    let cs = window
        .update(cx, |root, _w, _cx| mo_ui::column_rows_for_tests(root))
        .expect("读列行");
    let src = center(
        &mut vcx,
        format!("mo-col-row-0-0-0-{}", file_row(&cs, "note.txt")),
    );
    let dst = center(&mut vcx, format!("mo-col-row-0-0-0-{}", dir_row(&cs)));

    dispatch_down(&mut vcx, src);
    dispatch_move(&mut vcx, dst, false);
    let p = probe(cx, &window).expect("拖拽进行中");
    assert_eq!(
        p.hover.as_deref(),
        Some(format!("entry:{}", rig.bin.display()).as_str()),
        "列视图目录行应认领落点"
    );
    assert!(ghost(&mut vcx).is_some(), "列视图拖拽也该有 ghost");

    dispatch_up(&mut vcx, dst, false);
    assert!(
        wait_until(true, &rig.bin.join("note.txt")),
        "亮着的列行抬起应复制进去"
    );

    let _ = std::fs::remove_dir_all(&rig.base);
}
