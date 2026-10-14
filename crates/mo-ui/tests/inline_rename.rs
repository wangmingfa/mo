//! 编辑类交互 × 视图覆盖——devlog/windows-port.md §40：
//! 行内改名的编辑器此前**只在列表视图渲染**（`file_list.rs` / `file_item::view`），
//! F2 / 右键「重命名」却不分视图都能置编辑态——在网格 / 画廊 / 列视图按 F2，
//! 焦点进了一个没挂载的隐形输入框，打字看不见、改名落不了盘。
//! 列视图还有个更危险的：条目不走主目录模型、点行从不写 app 选择，
//! F2 / 复制 / 删除打的全是列表视图遗留的**隐形选区**。
//!
//! 本文件钉两条契约：
//! 1. 网格 / 画廊 / 列视图都能**看见并编辑**行内改名输入框，Enter 提交、
//!    点别处提交，与列表语义一致；
//! 2. 列视图单击把 app 选择**整替**成点的那一行（`select_path_exclusive`）：
//!    第 0 列能命中、删除打中新点的行而不是旧选区；深层列不在主模型，
//!    清完为空——命令宁可「没有选中文件」。
//!
//! 派发姿势与 `grid_drag.rs` / `column_drag.rs` 一致：**中央内容里的单元
//! （网格单元 / 列表行 / 列行）要按 `debug_bounds` 坐标派发 down/up**——kit 的
//! `window.click` 只认元素路径注册表里的元素（工具栏 / 侧边栏 / 状态栏），虚拟
//! 化滚动区里的单元不在册（`missing ElementId ... in scope []`）；工具栏按钮
//! （`view-mode-*`）照旧走 `window.click`。按键走 `simulate_keystrokes` + 平台
//! 键常量（`RENAME_KEY` / `TRASH_KEY`），输入走 `window.press` / `window.input`
//! （真实按键派发，裸 `simulate` 到不了输入组件——layout.rs :2148 的教训）。
//! 落盘判据墙钟轮询：改名走的是 Mo 的进程级 runtime，不受 GPUI 测试调度器驱动。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, Bounds, InputEvent, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    Pixels, TestAppContext, VisualTestContext, WindowHandle,
};
use mo_app::AppState;
use mo_ui::RootView;

/// 「重命名」的平台默认键（同 layout.rs:101：Finder=Enter，资源管理器=F2）。
const RENAME_KEY: &str = if cfg!(target_os = "macos") {
    "enter"
} else {
    "f2"
};

/// 「移入回收站」的平台默认键（mac ⌘⌫；其它平台裸 Delete，见 keys.rs 1235）。
const TRASH_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-backspace"
} else {
    "delete"
};

/// 输入框「全选」的平台键（mac ⌘A；其它 Ctrl+A——写死 cmd-a 在 Win 派发的是
/// Win 键组合，预填名不会被覆盖，layout.rs 的坑位）。
const SELECT_ALL_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-a"
} else {
    "ctrl-a"
};

/// fixture：`here/` = 目录 `box/` + 目录 `sub/`（内含 `deep.txt`）+ 文件两枚。
struct Rig {
    base: PathBuf,
    here: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("mo-irename-{tag}-{}", std::process::id()));
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

    fn trash_root(&self) -> PathBuf {
        self.base.join("trash")
    }
}

/// 建窗 + 导航（列表视图下完成，可选先点选 `list_click` 这个文件），
/// 再点工具栏按钮切到目标视图，等该视图的首元素渲染出来。
fn open_in_view(
    rig: &Rig,
    rows: usize,
    mode_button: &'static str,
    list_click: Option<&str>,
    probe: &'static str,
    cx: &mut TestAppContext,
) -> (VisualTestContext, WindowHandle<RootView>) {
    mo_ui::isolate_user_dirs_for_tests();
    // 行内改名的 `Input` 是 gpui-component 组件，吃 `Theme` 等全局（生产由
    // `run()` 注册，测试要自己补——layout.rs :2064 同款）。
    cx.update(gpui_kit::init);
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
            break;
        }
    }
    if let Some(name) = list_click {
        // 在列表视图里先点选某个文件——「旧选区」就是这么留下的。
        let idx = list_row_index(cx, &window, name);
        click_at(&mut vcx, &format!("mo-file-row-{idx}"));
    }
    // 真点工具栏按钮切视图。
    vcx.update(|window, cx| window.click(mode_button, cx));
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        if vcx.debug_bounds(probe).is_some() {
            return (vcx, window);
        }
    }
    panic!("切到 {mode_button} 后探针 {probe} 没渲染出来");
}

/// 主目录窗口快照里 `name` 的行号（不赌排序规则）。
fn list_row_index(tcx: &mut TestAppContext, window: &WindowHandle<RootView>, name: &str) -> usize {
    let rs = window
        .update(tcx, |root, _w, _cx| mo_ui::panel_row_paths_for_tests(root))
        .expect("读行");
    rs.iter()
        .position(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(name))
        .unwrap_or_else(|| panic!("{name} 没在窗口快照里：{rs:?}"))
}

/// 列视图第 0 列里 `name` 的行号。
fn col0_row_index(tcx: &mut TestAppContext, window: &WindowHandle<RootView>, name: &str) -> usize {
    let rs = tcx_window_rows(tcx, window);
    rs.iter()
        .position(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(name))
        .unwrap_or_else(|| panic!("{name} 没在第 0 列里：{rs:?}"))
}

fn tcx_window_rows(
    tcx: &mut TestAppContext,
    window: &WindowHandle<RootView>,
) -> Vec<(PathBuf, bool)> {
    window
        .update(tcx, |root, _w, _cx| mo_ui::column_rows_for_tests(root))
        .expect("读列")
}

/// 按坐标单击一个中央内容里的单元（网格单元 / 列表行 / 列行）：
/// down→拍帧→up 同一点 = 一次点击，`on_click` / `on_mouse_down` 照常收到。
/// `window.click` 对它没用——注册表里没有虚拟化区的元素（文件头注释）。
fn click_at(vcx: &mut VisualTestContext, selector: &str) {
    let s: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let b: Bounds<Pixels> = vcx
        .debug_bounds(s)
        .unwrap_or_else(|| panic!("{s} 没有出现在渲染帧里"));
    let at = b.origin + point(b.size.width / 2.0, b.size.height / 2.0);
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

/// 渲染帧直到 `mo-inline-rename`（行内改名的输入框）出现。
fn wait_editor_visible(vcx: &mut VisualTestContext, where_: &str) {
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        if vcx.debug_bounds("mo-inline-rename").is_some() {
            return;
        }
    }
    panic!("{where_}：{RENAME_KEY} 之后行内改名输入框没渲染出来");
}

/// 全选预填名 → 敲新名 → Enter 提交（提交键单独派发，被表单拦截器接走）。
fn type_and_commit(
    vcx: &mut VisualTestContext,
    cx: &mut TestAppContext,
    window: &WindowHandle<RootView>,
    new_name: &str,
) {
    vcx.update(|window, cx| {
        window.press(SELECT_ALL_KEY, cx);
        window.input(new_name, cx);
    });
    cx.simulate_keystrokes((*window).into(), "enter");
    vcx.run_until_parked();
}

fn wait_exists(gone: bool, p: &Path) -> bool {
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

/// 网格：点单元 → F2 → 输入框出现 → 改名提交落盘。
/// （§40 缺口 2：此前网格按 F2 只是把焦点塞进一个没挂载的输入框。）
#[gpui_kit::test]
fn grid_f2_opens_the_inline_editor_and_enter_renames_on_disk(cx: &mut TestAppContext) {
    let rig = Rig::new("grid-commit");
    let (mut vcx, window) = open_in_view(&rig, 4, "view-mode-grid", None, "mo-grid-cell-0", cx);

    let idx = {
        let rs = window
            .update(cx, |root, _w, _cx| mo_ui::panel_row_paths_for_tests(root))
            .expect("读行");
        rs.iter()
            .position(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some("note.txt"))
            .expect("note.txt 在窗口快照里")
    };
    click_at(&mut vcx, &format!("mo-grid-cell-{idx}"));

    cx.simulate_keystrokes(window.into(), RENAME_KEY);
    wait_editor_visible(&mut vcx, "网格");

    type_and_commit(&mut vcx, cx, &window, "renamed.txt");
    assert!(
        wait_exists(false, &rig.here.join("renamed.txt")),
        "网格里提交行内改名后 renamed.txt 没出现"
    );
    assert!(
        wait_exists(true, &rig.here.join("note.txt")),
        "改名落盘后旧路径还在"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 网格：编辑中**点别的单元** = 提交收场（与列表 :598 同款 mouse_down 接线）。
#[gpui_kit::test]
fn grid_click_another_cell_commits_the_rename(cx: &mut TestAppContext) {
    let rig = Rig::new("grid-click-commit");
    let (mut vcx, window) = open_in_view(&rig, 4, "view-mode-grid", None, "mo-grid-cell-0", cx);

    let (a, b) = {
        let rs = window
            .update(cx, |root, _w, _cx| mo_ui::panel_row_paths_for_tests(root))
            .expect("读行");
        let at = |n: &str| {
            rs.iter()
                .position(|(p, _)| p.file_name().and_then(|x| x.to_str()) == Some(n))
                .expect("fixture 文件在窗口里")
        };
        (at("note.txt"), at("keep.txt"))
    };
    click_at(&mut vcx, &format!("mo-grid-cell-{a}"));
    cx.simulate_keystrokes(window.into(), RENAME_KEY);
    wait_editor_visible(&mut vcx, "网格（提交用例）");

    vcx.update(|window, cx| {
        window.press(SELECT_ALL_KEY, cx);
        window.input("moved.txt", cx);
    });
    // 不敲 Enter：直接点另一个单元。
    click_at(&mut vcx, &format!("mo-grid-cell-{b}"));

    assert!(
        wait_exists(false, &rig.here.join("moved.txt")),
        "点别的单元没触发提交：moved.txt 没出现"
    );
    assert!(
        wait_exists(true, &rig.here.join("note.txt")),
        "提交后旧路径该没了"
    );
    // 编辑态也该收干净（否则切列表会弹出幽灵输入框）。
    vcx.update(|window, cx| window.render_frame(cx));
    assert!(
        vcx.debug_bounds("mo-inline-rename").is_none(),
        "提交后编辑器没收掉"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 画廊与网格共用 `grid::cell`：F2 的编辑器在画廊也要出现。
#[gpui_kit::test]
fn gallery_shares_the_inline_editor(cx: &mut TestAppContext) {
    let rig = Rig::new("gallery");
    let (mut vcx, window) = open_in_view(&rig, 4, "view-mode-gallery", None, "mo-grid-cell-0", cx);
    click_at(&mut vcx, "mo-grid-cell-0");
    cx.simulate_keystrokes(window.into(), RENAME_KEY);
    wait_editor_visible(&mut vcx, "画廊");
    // Esc 收场，不落盘。
    cx.simulate_keystrokes(window.into(), "escape");
    vcx.run_until_parked();
    assert!(
        rig.here.join("note.txt").exists() && rig.here.join("keep.txt").exists(),
        "fixture 文件不该被动过"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 列视图：点第 0 列的文件行 → F2（选择已由单击同步过来）→ 编辑器出现 → 落盘。
#[gpui_kit::test]
fn columns_click_then_f2_renames_the_clicked_row(cx: &mut TestAppContext) {
    let rig = Rig::new("columns-commit");
    let (mut vcx, window) =
        open_in_view(&rig, 4, "view-mode-columns", None, "mo-col-row-0-0-0-0", cx);

    let idx = col0_row_index(cx, &window, "note.txt");
    click_at(&mut vcx, &format!("mo-col-row-0-0-0-{idx}"));

    cx.simulate_keystrokes(window.into(), RENAME_KEY);
    wait_editor_visible(&mut vcx, "列视图");

    type_and_commit(&mut vcx, cx, &window, "renamed.txt");
    assert!(
        wait_exists(false, &rig.here.join("renamed.txt")),
        "列视图提交行内改名后 renamed.txt 没出现"
    );
    assert!(
        wait_exists(true, &rig.here.join("note.txt")),
        "改名落盘后旧路径还在"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 删除现在带退场动画（收缩 + 下方上顶，180ms 后才真正提交 `delete_paths`）。
/// 动画的定时器走 GPUI 的**可控时钟**，headless 下必须显式 `advance_clock` 才会
/// 到点；真实落盘又发生在 mo 的真实 runtime 上，所以要「推时钟 + 泵帧 + 让出
/// 真实时间」三件事一起做。
fn wait_gone_with_frames(vcx: &mut VisualTestContext, p: &Path) -> bool {
    for _ in 0..60 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        if !p.exists() {
            return true;
        }
        // 退场定时器（180ms）走可控时钟，推过去它才 ready。
        vcx.executor().advance_clock(Duration::from_millis(50));
        // 真磁盘删除在 mo 的进程级 runtime 上跑，让出一点真实时间给它。
        std::thread::sleep(Duration::from_millis(10));
    }
    !p.exists()
}

/// §40 缺口 1 的硬判据：列表里选中 `keep.txt` 切进列视图，再点 `note.txt` 行、
/// 按删除——进回收站的必须是**刚点的 note.txt**，`keep.txt` 一根毛都不能掉。
/// 接同步之前这里删的是看不见选中的旧文件。
#[gpui_kit::test]
fn columns_click_replaces_stale_selection_before_trashing(cx: &mut TestAppContext) {
    let rig = Rig::new("columns-stale");
    let (mut vcx, window) = open_in_view(
        &rig,
        4,
        "view-mode-columns",
        Some("keep.txt"), // 旧选区：列表视图点一下 keep.txt
        "mo-col-row-0-0-0-0",
        cx,
    );

    let idx = col0_row_index(cx, &window, "note.txt");
    click_at(&mut vcx, &format!("mo-col-row-0-0-0-{idx}"));

    cx.simulate_keystrokes(window.into(), TRASH_KEY);
    assert!(
        wait_gone_with_frames(&mut vcx, &rig.here.join("note.txt")),
        "列视图按删除没把刚点的 note.txt 移走——多半还打在旧选区上"
    );
    assert!(
        rig.here.join("keep.txt").exists(),
        "旧选区的 keep.txt 被误删了：选择整替没生效"
    );
    assert!(
        rig.trash_root().exists(),
        "回收站根没建出来：删除没走 trash 那条记账链路"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 删除退场动画的两段式判据（列表视图）：
/// ① 按下 ⌘⌫ 之后条目**立刻**进入退场态，而盘上那条还在——真正删除被推迟到
///    动画时长走完（否则动画期间一旦选区变了，删的就不是当初那几条）；
/// ② 把动画时长推过去，条目才真被删掉、且退场态**自动清空**（不清的话条目
///    永远占着收缩后的空槽，同一面板后续删除还会被「动画进行中」挡掉）。
///
/// ⚠️ 动画定时器走 GPUI 的可控时钟：`run_until_parked()` 不会推进时间，
/// 所以断言 ① 之前不能 `advance_clock`，否则 180ms 会被瞬间跨过去。
#[gpui_kit::test]
fn trash_defers_the_delete_until_the_collapse_animation_ends(cx: &mut TestAppContext) {
    let rig = Rig::new("trash-defer");
    let (mut vcx, window) = open_in_view(
        &rig,
        4,
        "view-mode-list",
        Some("note.txt"),
        "mo-file-row-0",
        cx,
    );

    cx.simulate_keystrokes(window.into(), TRASH_KEY);

    // ① 退场态建立（`selection_paths` 是真 IO，要泵一会儿；这里不推时钟）。
    let mut exits = 0;
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        exits = window
            .update(cx, |root, _w, _cx| mo_ui::exit_anim_for_tests(root))
            .expect("读退场态")
            .0;
        if exits > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(exits, 1, "按删除后应先进退场动画（1 项），而不是立刻删");
    assert!(
        rig.here.join("note.txt").exists(),
        "动画没走完就删了：删除应推迟到退场动画结束"
    );

    // ② 推过动画时长 → 真正删除 + 自动清空退场态。
    assert!(
        wait_gone_with_frames(&mut vcx, &rig.here.join("note.txt")),
        "退场动画走完后应真正把条目删掉"
    );
    let mut cleared = false;
    for _ in 0..40 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        let n = window
            .update(cx, |root, _w, _cx| mo_ui::exit_anim_for_tests(root))
            .expect("读退场态")
            .0;
        if n == 0 {
            cleared = true;
            break;
        }
        vcx.executor().advance_clock(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        cleared,
        "删除落地后退场态没清空（条目会永远占着收缩后的空槽）"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

/// 选择整替的另一半：深层列的条目不在主目录模型里——点它之后 app 选择必须是
/// **空**（宁空勿误伤），而点第 0 列的行能命中（选择数 1）。
#[gpui_kit::test]
fn columns_deep_column_click_empties_selection(cx: &mut TestAppContext) {
    let rig = Rig::new("columns-deep");
    let (mut vcx, window) = open_in_view(
        &rig,
        4,
        "view-mode-columns",
        Some("keep.txt"),
        "mo-col-row-0-0-0-0",
        cx,
    );

    // ① 第 0 列点文件行 → 选择整替成那一条（1）。
    let idx = col0_row_index(cx, &window, "note.txt");
    click_at(&mut vcx, &format!("mo-col-row-0-0-0-{idx}"));
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        if sel_count(cx, &window) == 1 {
            break;
        }
    }
    assert_eq!(
        sel_count(cx, &window),
        1,
        "点第 0 列的行之后 app 选择应整替成那一条"
    );

    // ② 点目录行展开子列，再点**深层列**的行 → 主模型里没有它 → 清空。
    let d = col0_row_index(cx, &window, "sub");
    click_at(&mut vcx, &format!("mo-col-row-0-0-0-{d}"));
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        let deep = window
            .update(cx, |root, _w, _cx| mo_ui::column_rows_at_for_tests(root, 1))
            .expect("读深层列");
        if !deep.is_empty() && vcx.debug_bounds("mo-col-row-0-0-1-0").is_some() {
            break;
        }
    }
    click_at(&mut vcx, "mo-col-row-0-0-1-0");
    for _ in 0..100 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.render_frame(cx));
        if sel_count(cx, &window) == 0 {
            break;
        }
    }
    assert_eq!(
        sel_count(cx, &window),
        0,
        "深层列的条目不在主模型里，点它之后选择必须清空——不是留着旧的那条"
    );
    let _ = std::fs::remove_dir_all(&rig.base);
}

fn sel_count(tcx: &mut TestAppContext, window: &WindowHandle<RootView>) -> usize {
    window
        .update(tcx, |root, _w, _cx| {
            mo_ui::panel_selection_count_for_tests(root)
        })
        .expect("读选区")
}
