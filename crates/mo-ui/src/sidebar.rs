use std::path::Path;

use gpui_kit::*;
use mo_app::AppState;

use crate::RootView;

/// 侧边栏：快捷访问 / 书签。
///
/// 位置由 [`AppState::quick_locations`] 提供（基于 `dirs` 解析，存在才显示），
/// 点击即跳转；当前所在位置高亮（含子目录内）。
pub fn render(
    app: &AppState,
    current: &Option<std::path::PathBuf>,
    entity: &Entity<RootView>,
) -> impl IntoElement {
    let locations = app.quick_locations();
    // 当前位置落在哪个快捷位置里（取匹配最深的一个）。
    let active = current.as_deref().and_then(|cur| {
        locations
            .iter()
            .filter(|(_, root)| is_within(cur, root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(_, root)| root.clone())
    });

    let mut panel = div()
        .flex()
        .flex_col()
        .w(px(188.0))
        .flex_shrink_0()
        .p(px(8.0))
        .gap(px(1.0))
        .bg(crate::theme::container())
        .border_r_1()
        .border_color(crate::theme::separator())
        .text_color(crate::theme::text())
        // 测试用（release no-op）：tests/layout.rs 断言侧边栏在中央区左侧
        .debug_selector(|| "mo-sidebar".to_string());

    panel = panel.child(
        div()
            .px(px(10.0))
            .pb(px(6.0))
            .pt(px(2.0))
            .text_size(px(11.0))
            .text_color(crate::theme::muted())
            .child(text!("快捷访问")),
    );

    for (ix, (label, path)) in locations.into_iter().enumerate() {
        let is_active = active.as_ref() == Some(&path);
        let app_click = app.clone();
        let entity_click = entity.clone();
        // ⚠️ 必须有元素 ID：无 ID 的裸 div 拿不到 element_state，on_click 永远不触发。
        let mut item = div()
            .id(("sidebar-loc", ix))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .text_size(px(13.0))
            .text_color(crate::theme::text())
            // 测试用（release no-op）：点「本地位置」的回归测试靠它定位。
            .debug_selector(move || format!("mo-sidebar-loc-{ix}"));

        if is_active {
            item = item.bg(crate::theme::accent());
        } else {
            item = item.hover(|s| s.bg(crate::theme::hover_bg()));
        }

        // imperative API：`Div` 只实现 `InteractiveElement`，点击回调走这里。
        // on_click 要求 `Fn`（可多次调用），闭包内只克隆、不消耗捕获值。
        item.interactivity().on_click(move |_, _window, cx| {
            let app = app_click.clone();
            let target = path.clone();
            let entity = entity_click.clone();
            cx.spawn(async move |cx| {
                // ⚠️ 必须走 `open_local`（快捷访问全是**本地**位置）。连着远程时若直接
                // `open_directory`，会拿本地路径去远程后端读（FTP 上它不存在）→ 导航
                // 失败，而错误过去又被 `let _ =` 丢掉，界面毫无反应——用户报的正是这个。
                //
                // 这条接线**没有 UI 层自动化守卫**：headless 的 GPUI 测试调度器会把
                // 「后台 tokio 线程唤醒测试任务」判成不确定性直接 panic，所以测试放在
                // `crates/mo-app/tests/remote_local.rs`（锁 `open_local` / `open_directory`
                // 的语义差别）。改这里时请一并看那个文件。
                if let Err(e) = app.open_local(&target).await {
                    // 失败必须让人看见，别再 `let _ =` 吞掉。
                    // （`Entity::update` 返回 `()`，不要写 `let _ =`：clippy 会拦。）
                    entity.update(cx, |v, cx| {
                        v.notice(format!("打开「{}」失败：{e}", target.display()), None, cx);
                    });
                }
            })
            .detach();
        });

        panel = panel.child(
            item.child(crate::icons::icon(
                crate::icons::quick_access_icon(&label),
                16.0,
                crate::theme::text(),
            ))
            .child(text!(label)),
        );
    }

    // 远程连接区：「远程」标题右侧一个「＋」（连接到服务器…），下面是**每条活着的
    // 连接**一行——点一行切过去，行尾的电源图标断开它。
    //
    // 用 `live_connections()`（活着的连接）而不是「当前是否在看远程」：切到本地目录
    // 并不会断开连接，这些行仍然在。列表来自进程级注册表，所以**每个标签页看到的都是
    // 同一批连接**——关标签页不断开，只有退出应用才断开。
    let connections = app.live_connections();
    let active = app.active_connection_id();
    let entity_conn = entity.clone();
    let mut add = div()
        .id("sidebar-remote-add")
        .flex()
        .items_center()
        .justify_center()
        .size(px(18.0))
        .rounded(px(4.0))
        .hover(|s| s.bg(crate::theme::hover_bg()));
    add.interactivity().on_click(move |_, _window, cx| {
        entity_conn.update(cx, |v, cx| v.open_connect_dialog(cx));
    });
    panel = panel.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .px(px(10.0))
            .pb(px(6.0))
            .pt(px(10.0))
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.0))
                    .text_color(crate::theme::muted())
                    .child(text!("远程")),
            )
            // 「连接到服务器…」从原来的一整行挪到标题右侧的加号上：列表长的时候，
            // 入口不该跟着列表往下走。
            .child(add.child(crate::icons::icon(
                crate::icons::PLUS,
                13.0,
                crate::theme::muted(),
            ))),
    );

    for (ix, conn) in connections.into_iter().enumerate() {
        // 用户名保留在这里（与地址栏相反）：这一行表示「以谁的身份连着哪台机器」，
        // 换个账号登录时全靠它分辨；密码本来就不回显。
        let label = conn.url.display();
        let id = conn.id;
        let browsing_this = active == Some(id);
        let app_back = app.clone();
        let entity_back = entity.clone();
        let app_dc = app.clone();
        let entity_dc = entity.clone();
        let mut item = div()
            .id(("sidebar-remote", ix))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .text_size(px(13.0))
            .text_color(crate::theme::text());
        // 正在看这条 → 高亮；连接活着但当前在本地 → 普通底色 + hover，
        // 一眼能看出「它还在，只是我现在没在里面」（点它就是切回去）。
        if browsing_this {
            item = item.bg(crate::theme::accent());
        } else {
            item = item.hover(|s| s.bg(crate::theme::hover_bg()));
        }
        item = item.child(crate::icons::icon(
            crate::icons::HARD_DRIVE,
            16.0,
            crate::theme::text(),
        ));
        item = item.child(div().flex_1().min_w_0().truncate().child(text!(label)));
        // 点这一行 = 切到这条连接（不重新登录，并回到它上次待过的目录）。
        item.interactivity().on_click(move |_, _window, cx| {
            let app = app_back.clone();
            let entity = entity_back.clone();
            cx.spawn(async move |cx| {
                if let Err(e) = app.open_connection(id).await {
                    entity.update(cx, |v, cx| {
                        v.notice(format!("切换远程连接失败：{e}"), None, cx);
                    });
                }
                // 切过去之后标签页徽标 / 地址栏都变了，立刻重绘。
                entity.update(cx, |_, cx| cx.notify());
            })
            .detach();
        });
        // 行尾的「断开连接」。⚠️ 必须 `stop_propagation()`：点击监听器是在**冒泡阶段**
        // 触发的，不拦住的话外层那一行也会收到，于是「断开」会顺带先把自己切过去。
        let mut disconnect = div()
            .id(("sidebar-remote-disconnect", ix))
            .ml_auto()
            .flex_shrink_0()
            .rounded(px(4.0))
            .hover(|s| s.bg(crate::theme::hover_bg()));
        disconnect.interactivity().on_click(move |_, _window, cx| {
            cx.stop_propagation();
            let app_dc = app_dc.clone();
            let entity_dc = entity_dc.clone();
            cx.spawn(async move |cx| {
                if let Err(e) = app_dc.disconnect_connection(id).await {
                    entity_dc.update(cx, |v, cx| {
                        v.notice(format!("断开连接失败：{e}"), None, cx);
                    });
                }
                entity_dc.update(cx, |_, cx| cx.notify());
            })
            .detach();
        });
        panel = panel.child(item.child(disconnect.child(crate::icons::icon(
            crate::icons::POWER,
            14.0,
            crate::theme::muted(),
        ))));
    }

    // 书签区（`~/Library/Application Support/mo/config.json` 里的
    // `sidebar_bookmarks`，命令面板「添加 / 移除书签」维护）。
    let bookmarks = app.bookmarks();
    if !bookmarks.is_empty() {
        panel = panel.child(
            div()
                .px(px(10.0))
                .pb(px(6.0))
                .pt(px(10.0))
                .text_size(px(11.0))
                .text_color(crate::theme::muted())
                .child(text!("书签")),
        );
    }
    for (ix, path) in bookmarks.into_iter().enumerate() {
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let is_active = current.as_deref() == Some(path.as_path());
        let app_click = app.clone();
        let app_del = app.clone();
        let entity_del = entity.clone();
        let entity_click = entity.clone();
        let target = path.clone();
        let del = path.clone();

        let mut item = div()
            .id(format!("sidebar-bm-{ix}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .text_size(px(13.0))
            .text_color(crate::theme::text());
        if is_active {
            item = item.bg(crate::theme::accent());
        } else {
            item = item.hover(|s| s.bg(crate::theme::hover_bg()));
        }
        item.interactivity().on_click(move |_, _window, cx| {
            let app = app_click.clone();
            let target = target.clone();
            let entity = entity_click.clone();
            // 书签是配置里持久化的路径，但**远程会话里加的书签存的是远程路径**
            // （命令面板取「当前目录」）。分流：本地确实存在的 → `open_local`
            // （连远程时先切回本地，否则拿本地路径去远程后端读必然失败）；
            // 本地没有的 → 交给当前后端，保持「远程书签回远程」的语义。
            let local = target.is_dir();
            cx.spawn(async move |cx| {
                let opened = if local {
                    app.open_local(&target).await
                } else {
                    app.open_directory(&target).await
                };
                if let Err(e) = opened {
                    entity.update(cx, |v, cx| {
                        v.notice(format!("打开「{}」失败：{e}", target.display()), None, cx);
                    });
                }
            })
            .detach();
        });

        // 移除按钮：常显但弱化，避免为「悬停才出现」再引入一套 hover 状态。
        let mut remove = div()
            .id(format!("sidebar-bm-del-{ix}"))
            .ml_auto()
            .pl(px(6.0))
            .text_size(px(12.0))
            .text_color(crate::theme::muted())
            .child(text!("✕".to_string()));
        remove.interactivity().on_click(move |_, _window, cx| {
            // ⚠️ 同一个冒泡坑：不停传播的话，删书签会顺带把这一行也打开。
            cx.stop_propagation();
            app_del.remove_bookmark(&del);
            let e = entity_del.clone();
            e.update(cx, |_, cx| cx.notify());
        });

        panel = panel.child(
            item.child(crate::icons::icon(
                crate::icons::FOLDER,
                16.0,
                crate::theme::text(),
            ))
            .child(text!(label))
            .child(remove),
        );
    }

    panel
}

/// `current` 是否位于 `root` 之内（含相等）。
///
/// 用 `strip_prefix` 而不是字符串前缀，避免 `/Users/a/Downloads2`
/// 被误判为在 `/Users/a/Downloads` 里。
fn is_within(current: &Path, root: &Path) -> bool {
    if current == root {
        return true;
    }
    match current.strip_prefix(root) {
        Ok(rem) => !rem.as_os_str().is_empty(),
        Err(_) => false,
    }
}
