//! 侧边栏：五个区（快捷访问〔含回收站入口〕/ 远程 / 网络 / 位置 / 书签）。
//!
//! ## 先算数据，再画
//!
//! 原来每个区各自手写一份 `div()` 链，六份共 ~600 行。代价不是行数：样式改一处要改
//! 六处；四个区的行在 headless 里**根本点不到**（只有快捷访问与回收站两行接了
//! `debug_selector` + `test_support`）；而「加一个侧栏项」等于抄 40 行 div。
//! 现在分两层：
//!
//! * **数据层**——[`Section`] / [`Row`] / [`Activate`] / [`Trailing`] / [`DropOn`] 与
//!   [`sections`]。纯函数：不碰 gpui、不读 `AppState`（五区的输入是 [`Sources`] 里
//!   投影过的小结构，见那儿的注释）。每行是什么、点它干什么、行尾挂不挂按钮、接不接受
//!   从资源管理器拖进来，全在这里回答，也因此能在普通 `#[test]` 里断言。
//! * **渲染层**——[`render`] / [`render_row`]。一种行、一处样式；点击与拖放的接线集中成
//!   对 [`Activate`] / [`DropOn`] 的 `match`（编译期穷尽：加一类语义就必须在这里补一臂，
//!   不会静默地「数据有了、点了没反应」）。
//!
//! P2 的插件清单 `sidebar` 字段落在这张表上：插件加一项 = 多一条 [`Row`] 数据。

use std::path::{Path, PathBuf};

use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_app::{AppState, SessionId};

use crate::RootView;

// ---------------------------------------------------------------------------
// 数据层
// ---------------------------------------------------------------------------

/// 失败时怎么说。两种措辞各归各的语境，别在渲染层拼一句对不上号的话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// 「打开「路径」失败：原因」——回显路径。快捷访问与书签一屏好几条，用户点的
    /// 是哪一条只有路径说得清。
    EchoPath,
    /// 「打开网络盘失败：原因」——这一区当时只可能指它自己，不必回显。
    Fixed(&'static str),
}

/// 点一行的语义。渲染层据此接线（见 [`render_row`] 里那个 `match`）。
#[derive(Debug, Clone)]
pub(crate) enum Activate {
    /// 打开一个目录。
    ///
    /// `fallback_to_current_backend`：路径在本地不存在时交给**当前后端**。书签是配置里
    /// 持久化的路径，而在远程会话里加的书签存的就是远程路径——那些必须回远程打开
    /// （命令面板取「当前目录」时它本来就在远程）。其余各区恒 `false`：快捷访问 /
    /// 网络盘 / 卷宗都是**本地**路径，连着远程时直接走当前后端会拿本地路径去远程读，
    /// 必然失败，而错误过去又被 `let _ =` 丢掉，界面毫无反应——用户报的正是这个。
    Open {
        path: PathBuf,
        fallback_to_current_backend: bool,
        failure: Failure,
    },
    /// 切到一条活着的远程连接（先保活 / 重连；服务器拒了凭据就弹认证框）。
    Connection { id: SessionId },
    /// 打开回收站面板。它是模态面板（`Modal::Trash`），不占任何路径，所以不走
    /// [`Activate::Open`]，也**不**「离开次级视图」——它本身就是进次级视图。
    TrashPanel,
}

/// 行尾那个小按钮按下去做什么。
#[derive(Debug, Clone)]
pub(crate) enum PowerAction {
    /// 断开这条远程连接（Mo 自己建的会话）。
    Disconnect(SessionId),
    /// 卸载一个网络盘（操作系统挂的，走 `umount`）。
    Unmount(PathBuf),
    /// 推出一块卷宗。⚠️ 只对 `ejectable` 的盘存在（内置硬盘不画这个按钮，画了只会
    /// 让人点出一句「推出失败」）。
    Eject(PathBuf),
}

/// 行尾按钮。
#[derive(Debug, Clone)]
pub(crate) enum Trailing {
    /// 电源图标（断开 / 卸载 / 推出三种语义共用外观）。
    Power(PowerAction),
    /// 「✕」：从配置里移除这条书签。
    RemoveBookmark(PathBuf),
}

/// 这一行接不接受「从资源管理器把文件拖进来」。
#[derive(Debug, Clone)]
pub(crate) enum DropOn {
    /// 落在这一行 = 把文件放进这个目录（快捷访问的位置）。
    Location(PathBuf),
    /// 落在这一行 = 送回收站（与资源管理器里拖到回收站图标同语义）。
    Trash,
}

/// 标题右侧的按钮。今天只有远程区的「＋ 连接到服务器…」——入口挂在标题上，列表长的
/// 时候才不会跟着列表往下走。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Heading {
    OpenConnectDialog,
}

/// 侧边栏的一行。
#[derive(Debug, Clone)]
pub(crate) struct Row {
    /// 元素 ID。**必须唯一**：两个可点击元素撞了同一个 ID，gpui 会把点击路由到其中
    /// 一个，另一个永远点不动，而且一声不吭。`tests` 里有一条全侧栏查重。
    ///
    /// headless 选择器由它派生：`mo-` + `Display`（`("sidebar-loc", 0)` →
    /// `mo-sidebar-loc-0`，`"sidebar-trash"` → `mo-sidebar-trash`），与收口前手写的
    /// 那两份 `debug_selector` 逐字符相同。行尾按钮挂在这一行下面
    /// （`ElementId::NamedChild`），所以天然不会跨行撞车。
    id: ElementId,
    label: String,
    icon: &'static [u8],
    /// 当前正看着这一行。三种判据在各自的构造器里算（见 [`sections`]）：快捷访问取
    /// 「包含当前路径的最深那个」，网络 / 位置 / 书签是精确相等，远程看会话编号，
    /// 回收站看 `trash_active`。
    active: bool,
    /// 标签吃不满时截断（长地址 / 长卷名要，短标签不要）。
    truncate: bool,
    activate: Activate,
    trailing: Option<Trailing>,
    drop: Option<DropOn>,
}

/// 一个区：标题（可省）+ 若干行。
#[derive(Debug, Clone)]
pub(crate) struct Section {
    title: &'static str,
    /// 一行都没有时是否仍画标题。快捷访问与远程恒画（远程还要靠标题右侧的「＋」
    /// 发起新连接）；网络 / 位置 / 书签空了就整区不出现，别留一句光杆标题。
    title_when_empty: bool,
    heading: Option<Heading>,
    rows: Vec<Row>,
}

impl Section {
    /// 这个区今天要不要出现（含只有标题的情况）。
    pub(crate) fn shows(&self) -> bool {
        !self.rows.is_empty() || self.title_when_empty
    }
}

/// 活着的远程连接所需的**最小**信息（从 `mo_app::LiveConnection` 投影）。
#[derive(Debug, Clone)]
pub(crate) struct Connection {
    id: SessionId,
    /// 含用户名的地址：这一行表示「以谁的身份连着哪台机器」，换个账号登录时全靠它
    /// 分辨；密码本来就不回显。
    label: String,
    /// 画哪个协议的图标（`smb` / `ftp` / `webdav` …）。
    scheme: String,
}

/// 系统已挂好的网络盘（从 `mo_remote::mount::NetworkShare` 投影）。
#[derive(Debug, Clone)]
pub(crate) struct Share {
    label: String,
    path: PathBuf,
    scheme: String,
}

/// 本机已挂载的卷宗（从 `mo_platform::Volume` 投影）。
#[derive(Debug, Clone)]
pub(crate) struct Drive {
    name: String,
    path: PathBuf,
    /// 这块盘推得动吗（判据在 `mo_platform::volumes`，UI 只照办）。
    ejectable: bool,
}

/// 五区的数据源。字段全是投影过的小结构，而不是 `AppState` 原样交出来的类型——
/// 那样数据层就 depend 上了远程那两个 crate（mo-ui 并不依赖 `mo-remote`），测试里
/// 还得会构造一个合法的远程 URL。投影之后数据层只认字符串与路径，代价是 [`render`]
/// 里几个 `.map()`。
#[derive(Debug, Clone, Default)]
pub(crate) struct Sources {
    /// 快捷访问（`AppState::quick_locations`）：显示名 + 本地目录。
    locations: Vec<(String, PathBuf)>,
    /// 活着的连接（`AppState::live_connections`）。
    connections: Vec<Connection>,
    /// 本标签页正在浏览的那条（`AppState::active_connection_id`），据此高亮对应行。
    active_connection: Option<SessionId>,
    /// 已挂好的网络盘（`AppState::network_shares`）。
    shares: Vec<Share>,
    /// 本机卷宗（`AppState::volumes`）。
    drives: Vec<Drive>,
    /// 用户书签（`AppState::bookmarks`）。
    bookmarks: Vec<PathBuf>,
}

/// 五区的数据，顺序即渲染顺序。
///
/// `current`：当前目录。回收站面板开着时调用方把它置空，改由 `trash_active` 高亮
/// 回收站入口——两者不并存。
pub(crate) fn sections(src: &Sources, current: Option<&Path>, trash_active: bool) -> Vec<Section> {
    vec![
        quick_access_section(&src.locations, current, trash_active),
        remote_section(&src.connections, src.active_connection),
        plain_section("网络", share_rows(&src.shares, current)),
        plain_section("位置", drive_rows(&src.drives, current)),
        plain_section("书签", bookmark_rows(&src.bookmarks, current)),
    ]
}

/// 快捷访问 + 回收站入口。快捷访问按「套住当前路径的最深那个」高亮
/// （`/Users/a/Desktop` 同时在「主目录」和「桌面」里，认桌面）。
fn quick_access_section(
    locations: &[(String, PathBuf)],
    current: Option<&Path>,
    trash_active: bool,
) -> Section {
    let deepest = current.and_then(|cur| {
        locations
            .iter()
            .filter(|(_, root)| is_within(cur, root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(_, root)| root.clone())
    });
    let mut rows: Vec<Row> = locations
        .iter()
        .enumerate()
        .map(|(ix, (label, path))| Row {
            id: ("sidebar-loc", ix).into(),
            label: label.clone(),
            icon: crate::icons::quick_access_icon(label),
            active: deepest.as_deref() == Some(path.as_path()),
            truncate: false,
            activate: Activate::Open {
                path: path.clone(),
                fallback_to_current_backend: false,
                failure: Failure::EchoPath,
            },
            trailing: None,
            // 把文件拖到一个快捷访问位 = 放进那个目录。
            drop: Some(DropOn::Location(path.clone())),
        })
        .collect();

    // 回收站入口：不占快捷访问那张表（那套按**路径**匹配高亮、点击走 `open_local`），
    // 回收站是模态面板，语义不同，所以单独一行、点开即看。
    rows.push(Row {
        id: "sidebar-trash".into(),
        label: "回收站".to_string(),
        icon: crate::icons::TRASH,
        active: trash_active,
        truncate: false,
        activate: Activate::TrashPanel,
        trailing: None,
        drop: Some(DropOn::Trash),
    });

    Section {
        title: "快捷访问",
        title_when_empty: true,
        heading: None,
        rows,
    }
}

/// 远程区：每条**活着的**连接一行。用 `live_connections()` 而不是「当前是否在看远程」：
/// 切回本地并不会断开连接，这些行仍然在。列表来自进程级注册表，所以每个标签页看到的
/// 都是同一批连接——关标签页不断开，只有退出应用才断开。
fn remote_section(connections: &[Connection], active: Option<SessionId>) -> Section {
    let rows = connections
        .iter()
        .map(|c| Row {
            id: ("sidebar-remote", c.id).into(),
            label: c.label.clone(),
            // 按协议画各自的图标（与「已记住的服务器」列表同一套映射）：一眼分清哪台是
            // 共享文件夹、哪台是 FTP、哪台是网盘。原来一律画硬盘，跟「此电脑」里的盘符
            // 撞成一个意思——硬盘表示的是「一块本地卷」，不是「一条网络连接」。
            icon: crate::icons::scheme_icon(&c.scheme),
            // 正在看这条 → 高亮；连接活着但当前在本地 → 普通底色 + hover，
            // 一眼能看出「它还在，只是我现在没在里面」（点它就是切回去）。
            active: active == Some(c.id),
            truncate: true,
            activate: Activate::Connection { id: c.id },
            trailing: Some(Trailing::Power(PowerAction::Disconnect(c.id))),
            drop: None,
        })
        .collect();

    Section {
        title: "远程",
        title_when_empty: true,
        heading: Some(Heading::OpenConnectDialog),
        rows,
    }
}

/// 网络区：系统里**已经挂好**的网络盘（SMB / NFS）。
///
/// 与远程区的区别：那里是 Mo 自己建的会话（FTP / SFTP / WebDAV，点一下走连接 / 重连），
/// 这里是操作系统挂好的目录——Mo 不持有任何连接，读写就是普通本地 IO。所以点它是
/// `open_local`，行尾的「断开」是 `umount`。
fn share_rows(shares: &[Share], current: Option<&Path>) -> Vec<Row> {
    shares
        .iter()
        .enumerate()
        .map(|(ix, s)| Row {
            id: format!("sidebar-net-{ix}").into(),
            label: s.label.clone(),
            icon: crate::icons::scheme_icon(&s.scheme),
            // 挂载点是一个具体目录：只有正好在里面浏览才高亮（不做「最深匹配」——
            // 在子目录里的人认的是地址栏，不是侧栏某行亮着）。
            active: current == Some(s.path.as_path()),
            truncate: true,
            activate: Activate::Open {
                path: s.path.clone(),
                fallback_to_current_backend: false,
                failure: Failure::Fixed("打开网络盘失败"),
            },
            trailing: Some(Trailing::Power(PowerAction::Unmount(s.path.clone()))),
            drop: None,
        })
        .collect()
}

/// 位置区：本机**已挂载的卷宗**（外接磁盘 / DMG / Time Machine 盘 / U 盘）。
///
/// 与网络区的区别：网络盘是操作系统按 SMB / NFS 挂的、文件系统类型是网络型，这里列的是
/// **本地**卷宗（macOS 按 `statfs` 的 `f_fstypename` 过滤、Windows 按 `GetDriveTypeW`
/// 把映射盘剔掉，都在 `mo_platform::volumes` 里办妥）。点它是 `open_local`，行尾「推出」
/// 走 `eject_volume`（先问平台、推不动的网络盘再退回系统的卸载命令）。
fn drive_rows(drives: &[Drive], current: Option<&Path>) -> Vec<Row> {
    drives
        .iter()
        .enumerate()
        .map(|(ix, v)| Row {
            id: format!("sidebar-vol-{ix}").into(),
            label: v.name.clone(),
            icon: crate::icons::HARD_DRIVE,
            active: current == Some(v.path.as_path()),
            truncate: true,
            activate: Activate::Open {
                path: v.path.clone(),
                fallback_to_current_backend: false,
                failure: Failure::Fixed("打开卷宗失败"),
            },
            // **只有推得动的盘才画这个按钮**（判据见 [`PowerAction::Eject`]）。
            trailing: v
                .ejectable
                .then(|| Trailing::Power(PowerAction::Eject(v.path.clone()))),
            drop: None,
        })
        .collect()
}

/// 书签区（`<配置目录>/mo/config.json` 里的 `sidebar_bookmarks`，
/// 命令面板「添加 / 移除书签」维护）。
fn bookmark_rows(bookmarks: &[PathBuf], current: Option<&Path>) -> Vec<Row> {
    bookmarks
        .iter()
        .enumerate()
        .map(|(ix, path)| Row {
            id: format!("sidebar-bm-{ix}").into(),
            label: crate::path_label::folder_label(path),
            icon: crate::icons::FOLDER,
            active: current == Some(path.as_path()),
            truncate: true,
            activate: Activate::Open {
                path: path.clone(),
                fallback_to_current_backend: true,
                failure: Failure::EchoPath,
            },
            // 常显但弱化，避免为「悬停才出现」再引入一套 hover 状态。
            trailing: Some(Trailing::RemoveBookmark(path.clone())),
            drop: None,
        })
        .collect()
}

/// 网络 / 位置 / 书签：没有标题按钮、空了就整区不出现。
fn plain_section(title: &'static str, rows: Vec<Row>) -> Section {
    Section {
        title,
        title_when_empty: false,
        heading: None,
        rows,
    }
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

// ---------------------------------------------------------------------------
// 渲染层
// ---------------------------------------------------------------------------

/// 侧边栏：五区从上到下。两层各干什么见模块头。
pub fn render(
    app: &AppState,
    current: &Option<PathBuf>,
    trash_active: bool,
    entity: &Entity<RootView>,
) -> impl IntoElement {
    let src = Sources {
        locations: app.quick_locations(),
        connections: app
            .live_connections()
            .into_iter()
            .map(|c| Connection {
                id: c.id,
                label: c.url.display(),
                scheme: c.url.scheme.clone(),
            })
            .collect(),
        active_connection: app.active_connection_id(),
        shares: app
            .network_shares()
            .into_iter()
            .map(|s| Share {
                label: s.label,
                path: s.path,
                scheme: s.scheme,
            })
            .collect(),
        drives: app
            .volumes()
            .into_iter()
            .map(|v| Drive {
                name: v.name,
                path: v.path,
                ejectable: v.ejectable,
            })
            .collect(),
        bookmarks: app.bookmarks(),
    };
    // 回收站面板开着时「当前位置」不该再点亮快捷访问：调用方传进来的 `current` 已经
    // 是空的（见 app.rs 的 A 类视图分支），这里不另设判据。
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
        // 内容（快捷访问 / 位置 / 书签 / 远程 / 网络）可能比窗口高：在栏内滚动，
        // 别撑高整行（那样会拖累中央文件列表，见 tests/layout.rs 的
        // file_list_height_tracks_the_window）。
        .overflow_y_scrollbar()
        // 测试用（release no-op）：tests/layout.rs 断言侧边栏在中央区左侧
        .debug_selector(|| "mo-sidebar".to_string());

    for (ix, section) in sections(&src, current.as_deref(), trash_active)
        .into_iter()
        .enumerate()
    {
        if !section.shows() {
            continue;
        }
        panel = panel.child(render_heading(&section, ix == 0, entity));
        for row in section.rows {
            panel = panel.child(render_row(row, app, entity));
        }
    }

    panel
}

/// 区标题。第一个区的上边距紧一些（它顶上就是窗口圆角，别再留一段空白）。
///
/// 只有带按钮的那一种走 flex 行（标题占满、按钮贴右）；普通标题就是一句裸文本——
/// 与收口前那三份手写标题逐字符同款，别为了「统一」给它们套一层 flex 容器。
///
/// ⚠️ 标题那句 `text!` **必须显式给 ID**（见下面两处 `id =`）：收口前五个区各写一份，
/// 五句字面量是五个不同的调用点，天然不撞；现在合成一个循环里的一个调用点，而
/// `text!` 的默认 ID 是「调用点位置的哈希」——标题外层那些 div 又都没有元素 ID（它们
/// 不可交互，加 ID 只会多出五份无用的 element_state），于是四五个标题的 a11y NodeId
/// 全等于同一个。平时看不出来，屏幕朗读或检查器一挂上就 debug panic
/// （`0xc0000409`，见 devlog 与 memory 里的同一条：循环里的 `text!` 一律显式给 ID）。
/// 行内的标签没这个问题——它们的外层行 div 各带一个唯一 ID。
fn render_heading(section: &Section, first: bool, entity: &Entity<RootView>) -> AnyElement {
    let head = div()
        .px(px(10.0))
        .pb(px(6.0))
        .pt(if first { px(2.0) } else { px(10.0) })
        .text_size(px(11.0))
        .text_color(crate::theme::muted());

    let Some(button) = section.heading else {
        return head
            .child(text!(
                id = format!("mo-head-{}", section.title),
                section.title
            ))
            .into_any_element();
    };

    // 「连接到服务器…」从原来的一整行挪到标题右侧的加号上：列表长的时候，
    // 入口不该跟着列表往下走。
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
    match button {
        Heading::OpenConnectDialog => head
            .flex()
            .flex_row()
            .items_center()
            // 这一格自己带上字号与颜色：与收口前手写的那版同款。样式继承不在这次
            // 重构的范围内，别顺手拿它当「统一写法」的试验田。
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.0))
                    .text_color(crate::theme::muted())
                    .child(text!(
                        id = format!("mo-head-{}", section.title),
                        section.title
                    )),
            )
            .child(add.child(crate::icons::icon(
                crate::icons::PLUS,
                13.0,
                crate::theme::muted(),
            )))
            .into_any_element(),
    }
}

/// 一行。样式只有这一处——以前六个区各写一份、改一处要改六份的那些
/// （`px(10)` / `py(5)` / `rounded(6)` / `text_size(13)`、active 上 accent 底否则 hover）
/// 现在都在下面。
fn render_row(row: Row, app: &AppState, entity: &Entity<RootView>) -> impl IntoElement {
    let Row {
        id,
        label,
        icon,
        active,
        truncate,
        activate,
        trailing,
        drop,
    } = row;

    let mut item = div()
        .id(id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .text_size(px(13.0))
        .text_color(crate::theme::text())
        // ⚠️ 上面那个 `.id` 不是可选的：无 ID 的裸 div 拿不到 element_state，
        // on_click 永远不触发（第一次接侧栏点击时就踩过）。
        //
        // 测试用（release no-op）：选择器从元素 ID 派生，两边永不分叉。
        // 这里**不** `move`——`id` 后面还要给行尾按钮当父 ID 用。
        .debug_selector(|| format!("mo-{id}"));

    if active {
        item = item.bg(crate::theme::accent());
    } else {
        item = item.hover(|s| s.bg(crate::theme::hover_bg()));
    }

    // 从资源管理器拖进来：只有快捷访问与回收站接（网络盘 / 卷宗 / 书签拖了没语义）。
    if let Some(target) = drop {
        item = item.drag_over::<ExternalPaths>(|style, _, _window, _cx| {
            style.bg(crate::theme::hover_bg())
        });
        let entity_os = entity.clone();
        item.interactivity()
            .on_drop::<ExternalPaths>(move |paths, _window, cx| {
                let paths = paths.paths().to_vec();
                match &target {
                    DropOn::Location(dest) => {
                        let dest = dest.clone();
                        entity_os.update(cx, |v, cx| {
                            v.drop_os_paths_on_location(paths.clone(), dest, cx)
                        });
                    }
                    DropOn::Trash => {
                        entity_os.update(cx, |v, cx| v.drop_os_paths_on_trash(paths.clone(), cx));
                    }
                }
            });
    }

    let app_click = app.clone();
    let entity_click = entity.clone();
    item.interactivity().on_click(move |_, _window, cx| {
        match &activate {
            Activate::TrashPanel => {
                entity_click.update(cx, |v, cx| v.open_trash_panel(cx));
            }
            Activate::Connection { id } => {
                let app = app_click.clone();
                let entity = entity_click.clone();
                let id = *id;
                // 导航 = 离开次级视图（回收站 / 全局搜索），不然「去了那个目录、
                // 人还留在回收站里」（用户报）。下面 `Open` 那一臂同理。
                entity.update(cx, |v, cx| v.leave_secondary_view(cx));
                cx.spawn(async move |cx| {
                    // `open_connection` 会先保活 / 重连（闲置被服务器掐掉的连接在这里
                    // 静默恢复），所以走不到 `Err` 就已经是「真的不行了」。
                    match app.open_connection(id).await {
                        Ok(()) => {}
                        // 服务器拒了凭据（比如闲置期间那边改了密码）：直接弹认证框，
                        // 别让用户对着一句错误发呆。
                        Err(f @ mo_app::ConnectFailure::NeedsCredentials { .. }) => {
                            entity.update(cx, |v, cx| v.on_connect_result(Err(f), cx));
                        }
                        Err(e) => {
                            entity.update(cx, |v, cx| {
                                v.notice(format!("切换远程连接失败：{e}"), None, cx);
                            });
                        }
                    }
                    // 切过去之后标签页徽标 / 地址栏都变了，立刻重绘。
                    entity.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
            Activate::Open {
                path,
                fallback_to_current_backend,
                failure,
            } => {
                let app = app_click.clone();
                let entity = entity_click.clone();
                let target = path.clone();
                let fallback = *fallback_to_current_backend;
                let failure = *failure;
                entity.update(cx, |v, cx| v.leave_secondary_view(cx));
                cx.spawn(async move |cx| {
                    // 分流规则见 [`Activate::Open`] 的 `fallback_to_current_backend`。
                    let opened = if fallback && !target.is_dir() {
                        app.open_directory(&target).await
                    } else {
                        // 快捷访问全是**本地**位置；挂载点也是本地目录——连着远程时都要
                        // 先切回本地（拿本地路径去远程后端读必然失败）。
                        app.open_local(&target).await
                    };
                    if let Err(e) = opened {
                        // 失败必须让人看见，别再 `let _ =` 吞掉。
                        // （`Entity::update` 返回 `()`，不要写 `let _ =`：clippy 会拦。）
                        let msg = match failure {
                            Failure::EchoPath => {
                                format!("打开「{}」失败：{e}", target.display())
                            }
                            Failure::Fixed(prefix) => format!("{prefix}：{e}"),
                        };
                        entity.update(cx, |v, cx| v.notice(msg, None, cx));
                    }
                    entity.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
        }
    });

    // 标签。快捷访问那几行是短词（主目录 / 桌面 / 下载…），不截断；长地址、长卷名才要
    // `flex_1 + truncate`，否则一行撑出去把行尾按钮挤出侧栏。
    let label_el = if truncate {
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .child(text!(label))
            .into_any_element()
    } else {
        text!(label).into_any_element()
    };
    item = item
        .child(crate::icons::icon(icon, 16.0, crate::theme::text()))
        .child(label_el);

    if let Some(trailing) = trailing {
        item = item.child(render_trailing(trailing, &id, app, entity));
    }

    // headless 测试要点这一行（`click("mo-sidebar-…")` 只认**被观察**的元素；非 test
    // 构建是恒等包装）。必须**最后**包，见 progress_panel 里同款的说明。
    item.test_support()
}

/// 行尾按钮。⚠️ 两种外观各不同（图标按钮有 hover 底、✕ 是钝的），但**都必须
/// `stop_propagation()`**：点击监听在冒泡阶段触发，不拦住的话外层那一行也会收到，
/// 于是「断开 / 卸载 / 推出 / 移除」会顺带先把自己这一项打开——刚卸掉的盘又被打开一次。
///
/// `row_id` 是这一行的元素 ID：按钮挂在它下面（`NamedChild`），所以每行一个按钮命名
/// 空间，不需要再手工编号，也不可能跨行撞车。
fn render_trailing(
    trailing: Trailing,
    row_id: &ElementId,
    app: &AppState,
    entity: &Entity<RootView>,
) -> AnyElement {
    match trailing {
        Trailing::Power(action) => {
            let id: ElementId = (row_id.clone(), "power").into();
            let mut btn = div()
                .id(id.clone())
                .ml_auto()
                .flex_shrink_0()
                .rounded(px(4.0))
                .hover(|s| s.bg(crate::theme::hover_bg()))
                .debug_selector(move || format!("mo-{id}"));

            let app_btn = app.clone();
            let entity_btn = entity.clone();
            btn.interactivity().on_click(move |_, _window, cx| {
                cx.stop_propagation();
                let app = app_btn.clone();
                let entity = entity_btn.clone();
                match &action {
                    PowerAction::Disconnect(id) => {
                        let id = *id;
                        cx.spawn(async move |cx| {
                            if let Err(e) = app.disconnect_connection(id).await {
                                entity.update(cx, |v, cx| {
                                    v.notice(format!("断开连接失败：{e}"), None, cx);
                                });
                            }
                            entity.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                    }
                    PowerAction::Unmount(path) => {
                        let path = path.clone();
                        cx.spawn(async move |cx| {
                            if let Err(e) = app.unmount_share(path).await {
                                entity.update(cx, |v, cx| {
                                    v.notice(format!("卸载失败：{e}"), None, cx);
                                });
                            }
                            entity.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                    }
                    PowerAction::Eject(path) => {
                        let path = path.clone();
                        cx.spawn(async move |cx| {
                            if let Err(e) = app.eject_volume(path).await {
                                entity.update(cx, |v, cx| {
                                    // 原样显示：`eject_volume` 交回来的已经是一句完整的话
                                    // （平台的否决理由形如「推出失败：E:\（被 xxx 占用）」），
                                    // 这里再加前缀就成了「推出失败：推出失败：…」。
                                    v.notice(e.to_string(), None, cx);
                                });
                            }
                            entity.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                    }
                }
            });
            btn.child(crate::icons::icon(
                crate::icons::POWER,
                14.0,
                crate::theme::muted(),
            ))
            .test_support()
            .into_any_element()
        }
        Trailing::RemoveBookmark(path) => {
            let id: ElementId = (row_id.clone(), "del").into();
            let mut btn = div()
                .id(id.clone())
                .ml_auto()
                .pl(px(6.0))
                .text_size(px(12.0))
                .text_color(crate::theme::muted())
                .debug_selector(move || format!("mo-{id}"));
            let app_del = app.clone();
            let entity_del = entity.clone();
            btn.interactivity().on_click(move |_, _window, cx| {
                cx.stop_propagation();
                app_del.remove_bookmark(&path);
                entity_del.update(cx, |_, cx| cx.notify());
            });
            btn.child(text!("✕".to_string()))
                .test_support()
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    // ⚠️ 这个模块**不能** `use super::*`：super 里有 `use gpui_kit::*`，那里面带一个
    // 同名的属性宏 `test`，一挂上来 `#[test]` 就自我展开到递归上限（本文件第一次收口
    // 时实测：`error: recursion limit reached while expanding #[test]`）。
    use super::{
        drive_rows, sections, share_rows, Activate, Connection, Drive, DropOn, Failure, Heading,
        PowerAction, Share, Sources, Trailing,
    };
    use gpui_kit::ElementId;
    use std::path::{Path, PathBuf};

    fn conn(id: u64) -> Connection {
        Connection {
            id,
            label: format!("ftp://u{id}@h{id}/"),
            scheme: "ftp".to_string(),
        }
    }

    fn loc(label: &str, path: &str) -> (String, PathBuf) {
        (label.to_string(), PathBuf::from(path))
    }

    fn only_locs(paths: Vec<&str>) -> Sources {
        Sources {
            locations: paths
                .iter()
                .enumerate()
                .map(|(i, p)| loc(&format!("位置{i}"), p))
                .collect(),
            ..Default::default()
        }
    }

    fn share(label: &str, path: &str) -> Share {
        Share {
            label: label.to_string(),
            path: PathBuf::from(path),
            scheme: "smb".to_string(),
        }
    }

    fn drive(name: &str, path: &str, ejectable: bool) -> Drive {
        Drive {
            name: name.to_string(),
            path: PathBuf::from(path),
            ejectable,
        }
    }

    /// 每个区各一条数据时：区序、行归属、标题按钮。
    #[test]
    fn sections_have_a_fixed_order_and_owners() {
        let s = Sources {
            locations: vec![loc("桌面", "/home/me/Desktop")],
            connections: vec![conn(7)],
            active_connection: Some(7),
            shares: vec![share("NAS", "/mnt/nas")],
            drives: vec![drive("备份盘", "/Volumes/backup", true)],
            bookmarks: vec![PathBuf::from("/home/me/proj")],
        };
        let got = sections(&s, None, false);
        let titles: Vec<&str> = got.iter().map(|x| x.title).collect();
        assert_eq!(titles, ["快捷访问", "远程", "网络", "位置", "书签"]);
        assert_eq!(got[0].rows.len(), 2, "一行快捷访问 + 回收站");
        assert_eq!(got[0].rows[1].label, "回收站");
        assert_eq!(got[1].heading, Some(Heading::OpenConnectDialog));
        assert!(got[1].rows[0].active, "正在看的那条连接要高亮");
        // 图标各归各的表：协议图标不能与本地图标混为一谈。（比内容不比 `as_ptr()`——
        // `icons.rs` 里那些是 `pub const &[u8] = br##"…"##`，字面量在每个使用点各自
        // 落成一份，跨函数比指针恒为假。）
        assert_eq!(got[1].rows[0].icon, crate::icons::scheme_icon("ftp"));
        assert_eq!(got[3].rows[0].icon, crate::icons::HARD_DRIVE);
        assert_ne!(
            got[1].rows[0].icon,
            crate::icons::HARD_DRIVE,
            "远程行一律画硬盘，就跟「此电脑」里的盘符撞成一个意思了"
        );
    }

    /// 空区整区不出现；快捷访问与回收站入口恒在（远程也是——它靠标题上的「＋」发起
    /// 连接，一条连接都没有时更得留着）。
    #[test]
    fn empty_areas_disappear_but_the_permanent_ones_stay() {
        let got = sections(&Sources::default(), None, false);
        let shown: Vec<&str> = got.iter().filter(|x| x.shows()).map(|x| x.title).collect();
        assert_eq!(shown, ["快捷访问", "远程"]);
        assert_eq!(got[0].rows.len(), 1, "只剩回收站那一行");
        assert!(got[1].rows.is_empty() && got[1].shows());
    }

    /// 快捷访问按「套住当前路径的最深那个」高亮：`/home/me/Downloads2` 不在 `Downloads`
    /// 里（按路径组件比，不是字符串前缀），但它确实还在 `主目录` 里，于是亮的是主目录。
    #[test]
    fn quick_access_highlights_the_deepest_match_only() {
        let s = Sources {
            locations: vec![
                loc("主目录", "/home/me"),
                loc("下载", "/home/me/Downloads"),
                loc("图片", "/home/me/Pictures"),
            ],
            ..Default::default()
        };
        let active = |cur: &str| -> Vec<String> {
            sections(&s, Some(Path::new(cur)), false)
                .remove(0)
                .rows
                .into_iter()
                .filter(|r| r.active)
                .map(|r| r.label)
                .collect()
        };
        assert_eq!(active("/home/me/Downloads/2026"), ["下载"]);
        assert_eq!(active("/home/me"), ["主目录"]);
        assert_eq!(
            active("/home/me/Downloads2"),
            ["主目录"],
            "字符串前缀不算包含：`Downloads2` 不在 `Downloads` 里"
        );
        assert_eq!(active("/elsewhere"), Vec::<String>::new());
    }

    /// 网络 / 位置 / 书签是**精确相等**：在挂载点的子目录里不该把它们点亮。
    #[test]
    fn exact_areas_highlight_only_their_own_path() {
        let s = Sources {
            shares: vec![share("NAS", "/mnt/nas")],
            drives: vec![drive("U 盘", "/Volumes/usb", true)],
            bookmarks: vec![PathBuf::from("/home/me/proj")],
            ..Default::default()
        };
        let on = |title: &str, cur: &str| -> usize {
            sections(&s, Some(Path::new(cur)), false)
                .into_iter()
                .find(|x| x.title == title)
                .unwrap()
                .rows
                .iter()
                .filter(|r| r.active)
                .count()
        };
        assert_eq!(on("网络", "/mnt/nas"), 1);
        assert_eq!(on("网络", "/mnt/nas/photos"), 0);
        assert_eq!(on("网络", "/mnt/other"), 0);
        assert_eq!(on("位置", "/Volumes/usb"), 1);
        assert_eq!(on("位置", "/Volumes/usb/dcim"), 0);
        assert_eq!(on("书签", "/home/me/proj"), 1);
        assert_eq!(on("书签", "/home/me/proj/crates"), 0);
    }

    /// 回收站开着时高亮落在回收站那一行，其余行不跟着亮（调用方把 `current` 置空）。
    #[test]
    fn trash_active_highlights_only_its_own_row() {
        let s = only_locs(vec!["/home/me"]);
        let rows = &sections(&s, None, true)[0].rows;
        assert!(!rows[0].active, "快捷访问不该跟着亮");
        assert!(rows[1].active, "回收站那一行要亮");
    }

    /// 只有快捷访问与回收站接受从资源管理器拖进来。
    #[test]
    fn only_locations_and_trash_take_a_drop() {
        let s = Sources {
            locations: vec![loc("桌面", "/home/me/Desktop")],
            connections: vec![conn(1)],
            shares: vec![share("NAS", "/mnt/nas")],
            drives: vec![drive("U 盘", "/Volumes/usb", true)],
            bookmarks: vec![PathBuf::from("/home/me/proj")],
            ..Default::default()
        };
        let got = sections(&s, None, false);
        assert!(matches!(
            &got[0].rows[0].drop,
            Some(DropOn::Location(p)) if p == Path::new("/home/me/Desktop")
        ));
        assert!(matches!(got[0].rows[1].drop, Some(DropOn::Trash)));
        for section in &got[1..] {
            for row in &section.rows {
                assert!(
                    row.drop.is_none(),
                    "「{}／{}」不该接拖放",
                    section.title,
                    row.label
                );
            }
        }
    }

    /// 行尾按钮一一对号：断开 / 卸载 / 移除各归各位，推不动的盘不摆推出按钮。
    #[test]
    fn trailing_buttons_match_their_area() {
        let s = Sources {
            locations: vec![loc("桌面", "/home/me/Desktop")],
            connections: vec![conn(3)],
            shares: vec![share("NAS", "/mnt/nas")],
            drives: vec![
                drive("内置", "/", false),
                drive("U 盘", "/Volumes/usb", true),
            ],
            bookmarks: vec![PathBuf::from("/home/me/proj")],
            ..Default::default()
        };
        let got = sections(&s, None, false);
        assert!(got[0].rows[0].trailing.is_none(), "快捷访问没有行尾按钮");
        assert!(got[0].rows[1].trailing.is_none(), "回收站没有行尾按钮");
        assert!(matches!(
            got[1].rows[0].trailing,
            Some(Trailing::Power(PowerAction::Disconnect(3)))
        ));
        assert!(matches!(
            got[2].rows[0].trailing,
            Some(Trailing::Power(PowerAction::Unmount(_)))
        ));
        assert!(
            got[3].rows[0].trailing.is_none(),
            "内置硬盘（ejectable=false）不该有推出按钮"
        );
        assert!(matches!(
            got[3].rows[1].trailing,
            Some(Trailing::Power(PowerAction::Eject(_)))
        ));
        assert!(matches!(
            got[4].rows[0].trailing,
            Some(Trailing::RemoveBookmark(_))
        ));
    }

    /// 只有书签可能存的是远程路径，所以只有它带「交回当前后端」的分流标记。
    #[test]
    fn only_bookmarks_fall_back_to_the_current_backend() {
        let s = Sources {
            locations: vec![loc("桌面", "/home/me/Desktop")],
            shares: vec![share("NAS", "/mnt/nas")],
            drives: vec![drive("U 盘", "/Volumes/usb", true)],
            bookmarks: vec![PathBuf::from("/mnt/nas/photos")],
            ..Default::default()
        };
        for section in sections(&s, None, false) {
            for row in &section.rows {
                let want = section.title == "书签";
                match &row.activate {
                    Activate::Open {
                        fallback_to_current_backend,
                        failure,
                        ..
                    } => {
                        assert_eq!(
                            *fallback_to_current_backend, want,
                            "「{}／{}」的分流标记应当是 {want}",
                            section.title, row.label
                        );
                        // 快捷访问与书签一屏好几条，失败要回显路径；其余不必。
                        assert_eq!(
                            *failure == Failure::EchoPath,
                            section.title == "快捷访问" || want,
                            "「{}」的失败措辞选错了",
                            section.title
                        );
                    }
                    other => assert!(
                        section.title == "快捷访问" && matches!(other, Activate::TrashPanel),
                        "「{}／{}」出现了没预料到的点击语义",
                        section.title,
                        row.label
                    ),
                }
            }
        }
    }

    /// 推不动的盘不摆推出按钮，而且**行序与来源严格对齐**：按钮少画一行时，如果 ID 是
    /// 「数按钮出来的」而不是「数行出来的」，后面那几行的元素 ID 会集体错位——点 A 行
    /// 打开的是 B 行。
    #[test]
    fn non_ejectable_drives_get_no_button_and_rows_stay_aligned() {
        let vs = vec![
            drive("内置", "/", false),
            drive("U 盘", "/Volumes/usb", true),
            drive("系统", "C:/", false),
            drive("备份", "/Volumes/bak", true),
        ];
        let rows = drive_rows(&vs, None);
        assert_eq!(rows.len(), vs.len());
        for (ix, (row, v)) in rows.iter().zip(&vs).enumerate() {
            assert_eq!(row.label, v.name, "第 {ix} 行的名字对不上来源");
            assert_eq!(row.id, ElementId::from(format!("sidebar-vol-{ix}")));
            match &row.trailing {
                Some(Trailing::Power(PowerAction::Eject(p))) => {
                    assert!(v.ejectable, "推不动的盘「{}」画出了推出按钮", v.name);
                    assert_eq!(p, &v.path);
                }
                None => assert!(!v.ejectable, "可推出的盘「{}」少了按钮", v.name),
                Some(other) => panic!("位置区出现了没预料到的按钮：{other:?}"),
            }
        }
    }

    /// 每一行都带**自己**的按钮和**自己**的按钮 ID。按钮 ID 写成常量（或从行外借一个）
    /// 时，gpui 把两行的点击都路由给同一行，另一行的按钮点了没反应且一声不吭——这种
    /// bug 只能在「ID 由行 ID 派生」这条规矩上钉断言。
    #[test]
    fn every_row_carries_its_own_button_and_button_id() {
        let ss = vec![share("NAS", "/mnt/nas"), share("备份", "/mnt/bak")];
        let rows = share_rows(&ss, None);
        assert_eq!(rows.len(), 2);
        for (ix, (row, s)) in rows.iter().zip(&ss).enumerate() {
            let Some(Trailing::Power(PowerAction::Unmount(target))) = row.trailing.as_ref() else {
                panic!("网络区第 {ix} 行应当有卸载按钮");
            };
            assert_eq!(target, &s.path, "第 {ix} 行的按钮指向了别的路径");
            let button: ElementId = (row.id.clone(), "power").into();
            assert_eq!(
                button.to_string(),
                format!("sidebar-net-{ix}-power"),
                "按钮选择器与 headless 测试点的那个名字对不上"
            );
        }
    }

    /// 全侧栏元素 ID 不许撞车：两个可点击元素共用一个 ID 时 gpui 把点击路由到其中
    /// 一个，另一个**点了没反应**而且一声不吭。以前六份手写 div 时最容易出这件事，
    /// 所以钉成断言（含标题按钮与每行的行尾按钮）。
    #[test]
    fn element_ids_are_unique_across_the_sidebar() {
        let s = Sources {
            locations: (0..12)
                .map(|i| loc(&format!("位置{i}"), &format!("/home/me/l{i}")))
                .collect(),
            connections: (0..3).map(conn).collect(),
            active_connection: Some(1),
            shares: (0..3)
                .map(|i| share(&format!("NAS{i}"), &format!("/mnt/nas{i}")))
                .collect(),
            drives: (0..3)
                .map(|i| drive(&format!("盘{i}"), &format!("/Volumes/v{i}"), true))
                .collect(),
            bookmarks: (0..3)
                .map(|i| PathBuf::from(format!("/home/me/b{i}")))
                .collect(),
        };
        // 渲染层的编号规则（`render_row` / `render_trailing` 各一处），这里同款。
        let mut seen: Vec<(ElementId, String)> = Vec::new();
        let mut push = |who: String, id: ElementId| {
            if let Some((_, other)) = seen.iter().find(|(x, _)| x == &id) {
                panic!("元素 ID 撞车：{id}（{who} 与 {other}）——其中一个点了不会有任何反应");
            }
            seen.push((id, who));
        };
        push("标题「＋」".to_string(), "sidebar-remote-add".into());
        for section in sections(&s, Some(Path::new("/home/me/l0")), true) {
            for row in &section.rows {
                push(format!("{}／{}", section.title, row.label), row.id.clone());
                if let Some(trailing) = &row.trailing {
                    let name = match trailing {
                        Trailing::Power(_) => "power",
                        Trailing::RemoveBookmark(_) => "del",
                    };
                    push(
                        format!("{} 的行尾按钮", row.label),
                        (row.id.clone(), name).into(),
                    );
                }
            }
        }
        // 1 个标题「＋」+ 12 快捷访问 + 1 回收站 + 12 行各带一个行尾按钮（远程/网络/
        // 位置/书签各 3 行）。数目对不上 = 有某区某行根本没进这张表，查重就是空的。
        assert_eq!(seen.len(), 1 + 12 + 1 + 12 * 2);
    }
}
