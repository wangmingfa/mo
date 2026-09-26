//! 「从系统拖文件进来」这条接线的 headless 测试。
//!
//! 真实平台上是操作系统把 CF_HDROP / file URL 递进窗口，gpui 翻成
//! `FileDropEvent::{Entered, Submit}`（`Entered` 挂上一个 `ExternalPaths` 的
//! `active_drag`，`Submit` 合成一次左键 MouseUp）。这里照着 gpui 自己的做法
//! （`gpui/src/window.rs` 的 FileDrop 测试）直接 `dispatch_event` 那两个平台事件，
//! 于是**从「事件进窗口」到「文件真的落盘」整条路都是真的**：命中测试、drop 监听、
//! `RootView` 的结算、`AppState::transfer_between`、后台拷贝。
//!
//! 覆盖面只到能安全落盘的目标：目录行、窗格空白处、侧栏回收站。侧栏「快捷访问」
//! 那几条指向**真实** Home 目录（`quick_locations` 用 `dirs` 现推，
//! `isolate_user_dirs_for_tests` 只钉配置与缓存），往里写东西等于动用户的文件，
//! 所以那条留给人工验证。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, App, Bounds, ExternalPaths, FileDropEvent, InputEvent, Pixels, Point,
    TestAppContext, VisualTestContext, Window, WindowHandle,
};
use mo_app::AppState;
use mo_ui::RootView;

/// 一套互不干扰的临时目录：`here/`（窗格正在看的）、`here/inbox/`（唯一的目录行）、
/// `src/`（被拖进来的东西待在那）。
struct Rig {
    base: PathBuf,
    here: PathBuf,
    inbox: PathBuf,
    src: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("mo-os-drop-{tag}-{}", std::process::id()));
        // 上一轮的同名残留必须先清（pid 会被复用，否则读到旧文件）。
        let _ = std::fs::remove_dir_all(&base);
        let here = base.join("here");
        let inbox = here.join("inbox");
        let src = base.join("src");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        Self {
            base,
            here,
            inbox,
            src,
        }
    }

    /// 在 `src/` 里放一个文件，返回它的路径。
    fn source_file(&self, name: &str) -> PathBuf {
        let p = self.src.join(name);
        std::fs::write(&p, b"payload").unwrap();
        p
    }

    fn trash_root(&self) -> PathBuf {
        self.base.join("trash")
    }
}

fn open_at(
    dir: &Path,
    rig: &Rig,
    rows: usize,
    cx: &mut TestAppContext,
) -> (VisualTestContext, WindowHandle<RootView>) {
    mo_ui::isolate_user_dirs_for_tests();
    // 真 IO + 确定性调度器的固有冲突：见 tests/layout.rs 里同一段注释。
    cx.dispatcher.allow_parking();
    let app = AppState::with_trash(rig.trash_root());
    let window = cx.open_window(size(px(1000.), px(700.)), move |_, cx| {
        RootView::new(app.clone(), cx)
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    // ⚠️ 先等启动时那次「按需打开 Home」落地（`tab_loop` 的 `open_home`）。它是异步的、
    // 比我们这次导航**晚**：不等就直接导航，会被它盖回来——实测面板路径停在测试目录、
    // 条目却是 Home 的 9 个（`panel.path` 与新列表分两步回灌，看上去像导航没生效）。
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
    // 真导航到测试目录（别注入假行：后台元数据事件会把注入的快照清掉，测试随机红）。
    let target = dir.to_path_buf();
    window
        .update(cx, |root, _window, cx| {
            mo_ui::navigate_for_tests(root, target, cx)
        })
        .expect("导航失败");
    // 等到「条目数正好是这几条」（挡住任何一次 Home 盖回）且第一行画出来了（要坐标投事件）。
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
    panic!(
        "导航到 {} 后没等到 {rows} 行：面板停在 {:?}",
        dir.display(),
        window
            .update(cx, |root, _w, _cx| mo_ui::panel_path_for_tests(root))
            .ok()
            .flatten()
    );
}

fn center(vcx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    let b: Bounds<Pixels> = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} 没有出现在渲染帧里"));
    b.origin + point(b.size.width / 2.0, b.size.height / 2.0)
}

/// 模拟一次外部拖放：文件进入窗口、停在 `at` 上、松手。
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

/// 等文件出现 / 消失。拷贝在 Mo 的进程级 tokio runtime 上跑，不受 GPUI
/// 测试调度器驱动，所以只能按墙钟轮询。
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

/// 拖到目录行上 = 复制进那个目录。
#[gpui_kit::test]
fn os_drop_on_directory_row_copies_into_it(cx: &mut TestAppContext) {
    let rig = Rig::new("row");
    let (mut vcx, _window) = open_at(&rig.here, &rig, 1, cx);
    // `here/` 里只有 `inbox/` 这一条，所以第 0 行必是目录行。
    let at = center(&mut vcx, "mo-file-row-0");
    let src = rig.source_file("note.txt");
    drop_files(&mut vcx, at, vec![src.clone()]);

    let dest = rig.inbox.join("note.txt");
    assert!(
        wait_until(false, &dest),
        "拖到目录行上之后 {dest:?} 没出现：外部拖放没接到行级 drop 监听"
    );
    assert!(src.exists(), "复制不该动源文件");
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 拖到窗格空白处 = 复制进窗格当前显示的目录（行级监听没命中时的兜底）。
#[gpui_kit::test]
fn os_drop_on_pane_background_copies_into_current_directory(cx: &mut TestAppContext) {
    let rig = Rig::new("pane");
    let (mut vcx, _window) = open_at(&rig.here, &rig, 1, cx);
    // 列表只有一条条目，下面全是补白行——它不是目录行，事件必须冒泡到窗格容器。
    let at = center(&mut vcx, "mo-file-ph-8");
    let src = rig.source_file("loose.txt");
    drop_files(&mut vcx, at, vec![src.clone()]);

    let dest = rig.here.join("loose.txt");
    assert!(
        wait_until(false, &dest),
        "拖到窗格空白处之后 {dest:?} 没出现：窗格容器的兜底 drop 监听没生效"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 拖到侧栏「回收站」= 移走（源文件不再在原位）。
#[gpui_kit::test]
fn os_drop_on_sidebar_row_moves_into_trash(cx: &mut TestAppContext) {
    let rig = Rig::new("trash");
    let (mut vcx, _window) = open_at(&rig.here, &rig, 1, cx);
    let at = center(&mut vcx, "mo-sidebar-trash");
    let src = rig.source_file("gone.txt");
    drop_files(&mut vcx, at, vec![src.clone()]);

    assert!(
        wait_until(true, &src),
        "拖到侧栏回收站后 {src:?} 还在原地：侧栏行没接住外部拖放"
    );
    assert!(
        rig.trash_root().exists(),
        "回收站根没被建出来：条目没走 `trash_paths` 那条记账链路"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}
