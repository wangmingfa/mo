use std::ops::Range;
use std::path::PathBuf;

use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_app::AppState;
use mo_core::{SelectionModel, SortKey};
use mo_operations::{HashAlgo, OperationHandle, TrashEntry};
use mo_preview::Preview;
use mo_search::SearchHit;

use crate::{breadcrumb, file_list, progress_panel, sidebar, status_bar, toolbar};

/// 首次同步时抓取的窗口大小。
pub(crate) const INITIAL_WINDOW: usize = 200;

/// 当前打开的模态层（占用中央区；Esc 关闭）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Modal {
    None,
    /// 命令面板。
    CommandPalette,
    /// 全局搜索。
    GlobalSearch,
    /// 快速预览（空格 Quick Look）。
    QuickLook,
    /// 回收站面板。
    Trash,
    /// 文件 / 文件夹比较结果。
    Diff,
    /// 纯文本信息（哈希结果 / 提示）。
    Info(String),
}

/// 命令面板中的可执行命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandId {
    Refresh,
    Back,
    Forward,
    Parent,
    SelectAll,
    ClearSelection,
    DeleteSelection,
    SortName,
    SortSize,
    SortModified,
    SortKind,
    IndexCurrent,
    StopIndexing,
    OpenGlobalSearch,
    QuickLook,
    HashSelection,
    CompareSelection,
    Undo,
    Redo,
    OpenTrash,
}

struct CmdDef {
    id: CommandId,
    title: &'static str,
    category: &'static str,
}

/// 命令目录（命令面板的数据源）。
fn commands() -> Vec<CmdDef> {
    vec![
        CmdDef {
            id: CommandId::OpenGlobalSearch,
            title: "全局搜索…",
            category: "搜索",
        },
        CmdDef {
            id: CommandId::QuickLook,
            title: "快速预览（Quick Look）",
            category: "预览",
        },
        CmdDef {
            id: CommandId::HashSelection,
            title: "计算选中文件哈希（MD5/SHA-1/SHA-256）",
            category: "工具",
        },
        CmdDef {
            id: CommandId::CompareSelection,
            title: "比较选中的两项（文件 / 文件夹，含 diff）",
            category: "工具",
        },
        CmdDef {
            id: CommandId::IndexCurrent,
            title: "索引当前目录（建立全局搜索索引）",
            category: "搜索",
        },
        CmdDef {
            id: CommandId::StopIndexing,
            title: "停止索引",
            category: "搜索",
        },
        CmdDef {
            id: CommandId::Refresh,
            title: "刷新",
            category: "导航",
        },
        CmdDef {
            id: CommandId::Back,
            title: "后退",
            category: "导航",
        },
        CmdDef {
            id: CommandId::Forward,
            title: "前进",
            category: "导航",
        },
        CmdDef {
            id: CommandId::Parent,
            title: "上级目录",
            category: "导航",
        },
        CmdDef {
            id: CommandId::SelectAll,
            title: "全选",
            category: "选择",
        },
        CmdDef {
            id: CommandId::ClearSelection,
            title: "清除选择",
            category: "选择",
        },
        CmdDef {
            id: CommandId::DeleteSelection,
            title: "删除选中",
            category: "操作",
        },
        CmdDef {
            id: CommandId::Undo,
            title: "撤销",
            category: "操作",
        },
        CmdDef {
            id: CommandId::Redo,
            title: "重做",
            category: "操作",
        },
        CmdDef {
            id: CommandId::OpenTrash,
            title: "回收站…",
            category: "操作",
        },
        CmdDef {
            id: CommandId::SortName,
            title: "按名称排序",
            category: "排序",
        },
        CmdDef {
            id: CommandId::SortSize,
            title: "按大小排序",
            category: "排序",
        },
        CmdDef {
            id: CommandId::SortModified,
            title: "按修改时间排序",
            category: "排序",
        },
        CmdDef {
            id: CommandId::SortKind,
            title: "按类型排序",
            category: "排序",
        },
    ]
}

/// 按查询串过滤命令（大小写不敏感子串匹配）。
fn filtered_commands(q: &str) -> Vec<CommandId> {
    let q = q.trim().to_lowercase();
    commands()
        .into_iter()
        .filter(|c| {
            q.is_empty()
                || c.title.to_lowercase().contains(&q)
                || c.category.to_lowercase().contains(&q)
        })
        .map(|c| c.id)
        .collect()
}

/// 根视图：组合 toolbar / (sidebar + file list) / 进度面板 / status bar。
///
/// UI **不持有整份目录**：只持有可见区窗口（[`Self::window`]）与总数，
/// 滚动到哪取哪。这是十万级目录依然流畅的前提——
/// 虚拟化解决「渲染多少 element」，窗口懒加载解决「同步多少数据」。
pub struct RootView {
    pub(crate) app: AppState,
    path: Option<PathBuf>,
    /// 可见条目总数（虚拟化列表的 `item_count`）。
    pub(crate) visible_count: usize,
    /// 窗口快照的起始下标。
    pub(crate) window_start: usize,
    /// 窗口快照：只覆盖可见区 + 少量缓冲。
    pub(crate) window: Vec<mo_core::Entry>,
    /// 已发起但还没回填的窗口请求，避免重复派生任务。
    pub(crate) pending: Option<Range<usize>>,
    pub(crate) selection: SelectionModel,
    /// 后台操作快照（进度面板数据源）。
    ops: Vec<OperationHandle>,
    /// 输入即过滤的当前关键词。
    query: String,
    can_back: bool,
    can_forward: bool,
    /// 键盘焦点：没有它收不到按键事件。
    focus: FocusHandle,
    /// 当前模态层。
    modal: Modal,
    /// 命令面板过滤词。
    cmd_query: String,
    /// 命令面板 / 搜索结果的高亮下标。
    palette_index: usize,
    /// 全局搜索过滤词。
    search_query: String,
    /// 全局搜索结果。
    search_results: Vec<SearchHit>,
    /// 快速预览缓存。
    preview_cache: Option<Preview>,
    /// 已索引文件数（状态栏展示）。
    indexed: usize,
    /// 回收站条目快照（回收站面板数据源）。
    trash_entries: Vec<TrashEntry>,
    /// 比较 / diff 结果缓存（比较模态数据源）。
    diff_cache: Option<mo_diff::Comparison>,
}

impl RootView {
    pub fn new(app: AppState, cx: &mut Context<Self>) -> Self {
        let view = Self {
            app,
            path: None,
            visible_count: 0,
            window_start: 0,
            window: Vec::new(),
            pending: None,
            selection: SelectionModel::new(),
            ops: Vec::new(),
            query: String::new(),
            can_back: false,
            can_forward: false,
            focus: cx.focus_handle(),
            modal: Modal::None,
            cmd_query: String::new(),
            palette_index: 0,
            search_query: String::new(),
            search_results: Vec::new(),
            preview_cache: None,
            indexed: 0,
            trash_entries: Vec::new(),
            diff_cache: None,
        };

        let app2 = view.app.clone();
        // 注意：这里只持有 `WeakEntity`（即闭包拿到的 `weak`）。
        // 这个任务活得比窗口还久（它在等事件总线），若持有强引用
        // `Entity<RootView>`，视图会在窗口关闭后依然无法释放。
        cx.spawn(async move |weak, cx| {
            // 默认打开 Home 目录。
            if let Some(home) = home_dir() {
                let _ = mo_app::DirectoryController::new(app2.clone())
                    .open(&home)
                    .await;
            }
            Self::sync(&app2, &weak, cx).await;

            // 订阅事件总线：目录 / 导航 / 条目变化时刷新快照。
            // 外部程序增删改文件由 watcher 增量更新模型后广播到这里。
            let mut rx = app2.bus().subscribe();
            while rx.recv().await.is_ok() {
                Self::sync(&app2, &weak, cx).await;
            }
        })
        .detach();

        view
    }

    /// 把 `AppState` 的当前状态同步进 UI 快照并触发重绘。
    ///
    /// 注意只同步**元信息**与当前窗口，不克隆整份条目列表。
    /// 收 `WeakEntity`：视图销毁后这里静默退出，不会把它钉在内存里。
    async fn sync(app: &AppState, this: &WeakEntity<RootView>, cx: &mut AsyncApp) {
        let path = app.current_path().await;
        let count = app.visible_count().await;
        let can_back = app.can_go_back().await;
        let can_forward = app.can_go_forward().await;
        let ops = app.operations_snapshot().await;
        let indexed = app.index_count();
        let trash_entries = app.trash_list();
        // app 侧选择是唯一事实来源；⌘A / 键盘移动等改动都从这里回灌 UI。
        let selection_ids = app.selection_ids().await;

        let Ok(range) = this.update(cx, |v, _cx| {
            // 切换目录或改过滤词后，旧窗口的下标已失效，直接作废。
            if v.path != path || v.visible_count != count {
                v.window.clear();
                v.window_start = 0;
                v.pending = None;
            }
            v.path = path;
            v.visible_count = count;
            v.can_back = can_back;
            v.can_forward = can_forward;
            v.ops = ops;
            v.indexed = indexed;
            v.trash_entries = trash_entries;
            v.selection.set_from(&selection_ids);
            let len = if v.window.is_empty() {
                INITIAL_WINDOW
            } else {
                v.window.len()
            };
            v.window_start..v.window_start + len
        }) else {
            // 视图已销毁，停止同步。
            return;
        };

        let (start, entries) = app.visible_window(range).await;
        let for_thumbs = entries.clone();
        let _ = this.update(cx, |v, cx| {
            v.window_start = start;
            v.window = entries;
            v.pending = None;
            cx.notify();
        });
        // 缩略图只为当前窗口生成。
        app.thumbs().request(app.clone(), for_thumbs);
    }

    /// 应用当前过滤词（输入即过滤）。
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.trim().to_string();
        let app = self.app.clone();
        cx.spawn(async move |_weak, _cx| {
            app.set_filter(if query.is_empty() { None } else { Some(query) })
                .await;
        })
        .detach();
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// 当前用户的 Home 目录。
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir())
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let entity_key = entity.clone();

        // 模态打开时，工具栏 / 面包屑 / 状态栏保留，中央区换成模态卡片。
        let body: Div = match &self.modal {
            Modal::None => {
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .child(sidebar::render(&self.app, &self.path))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            // 测试用（release no-op）：tests/layout.rs 断言中央区位置与尺寸。
                            .debug_selector(|| "mo-center".to_string())
                            .child(filter_bar(&self.query))
                            .child(file_list::render(&entity, self.visible_count)),
                    )
            }
            Modal::CommandPalette => self.render_command_palette(&entity),
            Modal::GlobalSearch => self.render_global_search(&entity),
            Modal::QuickLook => self.render_quick_look(),
            Modal::Trash => self.render_trash(),
            Modal::Diff => self.render_diff(),
            Modal::Info(text) => render_info(text),
        };

        let mut root = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(crate::theme::surface())
            .text_color(crate::theme::text())
            .track_focus(&self.focus)
            .child(toolbar::render(&self.app, self.can_back, self.can_forward))
            .child(breadcrumb::render(&self.path))
            .child(body)
            .child(progress_panel::render(&self.ops, &self.app))
            .child(status_bar::render(
                self.visible_count,
                &self.path,
                &self.query,
                self.selection.count(),
                self.indexed,
                self.app.can_undo(),
                self.app.can_redo(),
            ));

        // 键盘路由：全局快捷键 + 模态内导航 + 输入即过滤。
        root.interactivity().on_key_down(move |ev, _window, cx| {
            let key = ev.keystroke.key.as_str();
            let m = &ev.keystroke.modifiers;
            let platform = m.platform;
            let shift = m.shift;
            let plain = !m.control && !m.alt && !m.platform;

            // 全局快捷键（任何状态下都可触发）。
            // ⌘Q：裸二进制没有菜单栏，macOS 收不到系统 terminate，
            // 自己接住退出键（cx.quit 走 gpui 的正常退出流程）。
            if platform && key.eq_ignore_ascii_case("q") {
                cx.quit();
                return;
            }
            if platform && shift && key.eq_ignore_ascii_case("p") {
                entity_key.update(cx, |v, cx| {
                    v.modal = Modal::CommandPalette;
                    v.cmd_query.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
                return;
            }
            if platform && key.eq_ignore_ascii_case("f") {
                entity_key.update(cx, |v, cx| {
                    v.modal = Modal::GlobalSearch;
                    v.search_query.clear();
                    v.search_results.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
                return;
            }
            if platform && key.eq_ignore_ascii_case("a") {
                let app = entity_key.update(cx, |v, _cx| v.app.clone());
                let this = entity_key.clone();
                cx.spawn(async move |cx| {
                    app.select_all_visible().await;
                    // app 侧选择是唯一事实来源：全选后回灌 UI 高亮。
                    pull_selection(&app, &this, cx).await;
                })
                .detach();
                return;
            }
            if platform && key.eq_ignore_ascii_case("z") {
                let app = entity_key.update(cx, |v, _cx| v.app.clone());
                if shift {
                    app.redo();
                } else {
                    app.undo();
                }
                return;
            }

            // 模态内按键。
            if entity_key.update(cx, |v, _cx| v.modal != Modal::None) {
                handle_modal_key(key, plain, &entity_key, cx);
                return;
            }

            // 非模态：键盘导航 + 输入即过滤 + 快速预览 + 删除选中。
            match key {
                "up" | "down" if plain => {
                    let app = entity_key.update(cx, |v, _cx| v.app.clone());
                    let this = entity_key.clone();
                    let step = if key == "up" { -1 } else { 1 };
                    let extend = shift;
                    cx.spawn(async move |cx| {
                        app.move_cursor(step, extend).await;
                        // app 侧选择是唯一事实来源：移动后回灌 UI 高亮。
                        pull_selection(&app, &this, cx).await;
                    })
                    .detach();
                }
                "enter" if plain => {
                    let app = entity_key.update(cx, |v, _cx| v.app.clone());
                    let this = entity_key.clone();
                    cx.spawn(async move |cx| {
                        open_focused(&app, &this, cx).await;
                    })
                    .detach();
                }
                " " if plain => {
                    let app = entity_key.update(cx, |v, _cx| v.app.clone());
                    let this = entity_key.clone();
                    cx.spawn(async move |cx| {
                        open_quick_look(&app, &this, cx).await;
                    })
                    .detach();
                }
                "delete" if plain => {
                    let app = entity_key.update(cx, |v, _cx| v.app.clone());
                    cx.spawn(async move |_cx| {
                        app.delete_selection().await;
                    })
                    .detach();
                }
                "backspace" => {
                    entity_key.update(cx, |v, cx| {
                        if v.query.is_empty() {
                            // 没有过滤词时返回上级目录。
                            let app = v.app.clone();
                            cx.spawn(async move |_weak, _cx| {
                                let _ = app.open_parent().await;
                            })
                            .detach();
                        } else {
                            v.query.pop();
                            v.apply_filter(cx);
                        }
                        cx.notify();
                    });
                }
                "escape" => {
                    entity_key.update(cx, |v, cx| {
                        v.query.clear();
                        v.apply_filter(cx);
                        cx.notify();
                    });
                }
                k if plain && k.chars().count() == 1 => {
                    let ch = k.chars().next().unwrap();
                    entity_key.update(cx, |v, cx| {
                        v.query.push(ch);
                        v.apply_filter(cx);
                        cx.notify();
                    });
                }
                _ => {}
            }
        });

        // 让焦点落在本视图上，否则按键不会派发到这里。
        if !self.focus.is_focused(window) {
            cx.focus_self(window);
        }

        root
    }
}

/// 模态内按键处理（返回是否已被处理）。
fn handle_modal_key(key: &str, plain: bool, entity: &Entity<RootView>, cx: &mut App) {
    let modal = entity.update(cx, |v, _cx| v.modal.clone());
    match modal {
        Modal::CommandPalette => match key {
            "escape" => close_modal(entity, cx),
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                if v.palette_index > 0 {
                    v.palette_index -= 1;
                }
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                let n = filtered_commands(&v.cmd_query).len();
                if n > 0 {
                    v.palette_index = (v.palette_index + 1).min(n - 1);
                }
                cx.notify();
            }),
            "enter" => on_palette_enter(entity, cx),
            k if plain && k.chars().count() == 1 => {
                let ch = k.chars().next().unwrap();
                entity.update(cx, |v, cx| {
                    v.cmd_query.push(ch);
                    v.palette_index = 0;
                    cx.notify();
                });
            }
            "backspace" => entity.update(cx, |v, cx| {
                v.cmd_query.pop();
                v.palette_index = 0;
                cx.notify();
            }),
            _ => {}
        },
        Modal::GlobalSearch => match key {
            "escape" => close_modal(entity, cx),
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                if v.palette_index > 0 {
                    v.palette_index -= 1;
                }
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                let n = v.search_results.len();
                if n > 0 {
                    v.palette_index = (v.palette_index + 1).min(n - 1);
                }
                cx.notify();
            }),
            "enter" => on_search_enter(entity, cx),
            k if plain && k.chars().count() == 1 => {
                let ch = k.chars().next().unwrap();
                let app = entity.update(cx, |v, cx| {
                    v.search_query.push(ch);
                    cx.notify();
                    v.app.clone()
                });
                // 实时搜索（同步、毫秒级）。
                let q = entity.update(cx, |v, _cx| v.search_query.clone());
                let results = app.global_search(&q, 50);
                entity.update(cx, |v, cx| {
                    v.search_results = results;
                    v.palette_index = 0;
                    cx.notify();
                });
            }
            "backspace" => {
                let app = entity.update(cx, |v, cx| {
                    v.search_query.pop();
                    cx.notify();
                    v.app.clone()
                });
                let q = entity.update(cx, |v, _cx| v.search_query.clone());
                let results = app.global_search(&q, 50);
                entity.update(cx, |v, cx| {
                    v.search_results = results;
                    v.palette_index = 0;
                    cx.notify();
                });
            }
            _ => {}
        },
        Modal::Trash => match key {
            "escape" => close_modal(entity, cx),
            "up" | "arrowup" => entity.update(cx, |v, cx| {
                if v.palette_index > 0 {
                    v.palette_index -= 1;
                }
                cx.notify();
            }),
            "down" | "arrowdown" => entity.update(cx, |v, cx| {
                let n = v.trash_entries.len();
                if n > 0 {
                    v.palette_index = (v.palette_index + 1).min(n - 1);
                }
                cx.notify();
            }),
            "enter" => on_trash_restore(entity, cx),
            "delete" => on_trash_purge(entity, cx),
            "e" if plain => on_trash_empty(entity, cx),
            _ => {}
        },
        Modal::QuickLook | Modal::Diff | Modal::Info(_) => match key {
            "escape" | " " => close_modal(entity, cx),
            _ => {}
        },
        Modal::None => {}
    }
}

/// 回收站：还原选中条目。
fn on_trash_restore(entity: &Entity<RootView>, cx: &mut App) {
    let (entry, app) = entity.update(cx, |v, cx| {
        let e = v.trash_entries.get(v.palette_index).cloned();
        v.palette_index = 0;
        cx.notify();
        (e, v.app.clone())
    });
    if let Some(e) = entry {
        cx.spawn(async move |_cx| {
            app.restore_trash_entry(e.original).await;
        })
        .detach();
    }
}

/// 回收站：永久删除选中条目。
fn on_trash_purge(entity: &Entity<RootView>, cx: &mut App) {
    let (entry, app) = entity.update(cx, |v, cx| {
        let e = v.trash_entries.get(v.palette_index).cloned();
        v.palette_index = 0;
        cx.notify();
        (e, v.app.clone())
    });
    if let Some(e) = entry {
        app.purge_trash_entry(e);
    }
}

/// 回收站：清空。
fn on_trash_empty(entity: &Entity<RootView>, cx: &mut App) {
    let app = entity.update(cx, |v, cx| {
        v.palette_index = 0;
        cx.notify();
        v.app.clone()
    });
    app.empty_trash();
}

fn close_modal(entity: &Entity<RootView>, cx: &mut App) {
    entity.update(cx, |v, cx| {
        v.modal = Modal::None;
        v.cmd_query.clear();
        v.search_query.clear();
        v.search_results.clear();
        v.preview_cache = None;
        v.diff_cache = None;
        v.palette_index = 0;
        cx.notify();
    });
}

/// 命令面板回车：执行选中的命令。
fn on_palette_enter(entity: &Entity<RootView>, cx: &mut App) {
    let (id, app) = entity.update(cx, |v, _cx| {
        let id = filtered_commands(&v.cmd_query)
            .get(v.palette_index)
            .copied();
        (id, v.app.clone())
    });
    match id {
        Some(CommandId::OpenGlobalSearch) => {
            entity.update(cx, |v, cx| {
                v.modal = Modal::GlobalSearch;
                v.search_query.clear();
                v.search_results.clear();
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(CommandId::QuickLook) => {
            let this = entity.clone();
            cx.spawn(async move |cx| {
                open_quick_look(&app, &this, cx).await;
            })
            .detach();
        }
        Some(CommandId::HashSelection) => {
            let this = entity.clone();
            cx.spawn(async move |cx| {
                compute_hash(&app, &this, cx).await;
            })
            .detach();
        }
        Some(CommandId::CompareSelection) => {
            let this = entity.clone();
            cx.spawn(async move |cx| {
                run_compare(&app, &this, cx).await;
            })
            .detach();
        }
        Some(CommandId::OpenTrash) => {
            entity.update(cx, |v, cx| {
                v.trash_entries = v.app.trash_list();
                v.modal = Modal::Trash;
                v.palette_index = 0;
                cx.notify();
            });
        }
        Some(other) => {
            let this = entity.clone();
            cx.spawn(async move |cx| {
                run_command(other, &app).await;
                // SelectAll / ClearSelection / Delete 等命令改了 app 侧选择，回灌 UI。
                pull_selection(&app, &this, cx).await;
                let _ = this.update(cx, |v, cx| {
                    v.modal = Modal::None;
                    v.cmd_query.clear();
                    v.palette_index = 0;
                    cx.notify();
                });
            })
            .detach();
        }
        None => {}
    }
}

/// 全局搜索回车：打开选中的结果（目录则进入，文件则预览）。
fn on_search_enter(entity: &Entity<RootView>, cx: &mut App) {
    let (hit, app) = entity.update(cx, |v, _cx| {
        let hit = v.search_results.get(v.palette_index).cloned();
        (hit, v.app.clone())
    });
    if let Some(h) = hit {
        let this = entity.clone();
        cx.spawn(async move |cx| {
            if h.kind.is_dir() {
                let _ = app.open_directory(&h.path).await;
                let _ = this.update(cx, |v, cx| {
                    v.modal = Modal::None;
                    v.search_query.clear();
                    v.search_results.clear();
                    cx.notify();
                });
            } else {
                match app.preview(&h.path) {
                    Ok(pv) => {
                        let _ = this.update(cx, |v, cx| {
                            v.preview_cache = Some(pv);
                            v.modal = Modal::QuickLook;
                            cx.notify();
                        });
                    }
                    Err(e) => {
                        let _ = this.update(cx, |v, cx| {
                            v.modal = Modal::Info(format!("无法预览：{e}"));
                            cx.notify();
                        });
                    }
                }
            }
        })
        .detach();
    }
}

/// 执行一个「运行即关闭」类命令。
async fn run_command(id: CommandId, app: &AppState) {
    match id {
        CommandId::Refresh => {
            let _ = app.refresh().await;
        }
        CommandId::Back => {
            let _ = app.go_back().await;
        }
        CommandId::Forward => {
            let _ = app.go_forward().await;
        }
        CommandId::Parent => {
            let _ = app.open_parent().await;
        }
        CommandId::SelectAll => app.select_all_visible().await,
        CommandId::ClearSelection => app.clear_selection().await,
        CommandId::DeleteSelection => {
            app.delete_selection().await;
        }
        CommandId::SortName => app.set_sort(SortKey::Name).await,
        CommandId::SortSize => app.set_sort(SortKey::Size).await,
        CommandId::SortModified => app.set_sort(SortKey::Modified).await,
        CommandId::SortKind => app.set_sort(SortKey::Kind).await,
        CommandId::IndexCurrent => {
            if let Some(p) = app.current_path().await {
                app.index_root(p, 0);
            }
        }
        CommandId::StopIndexing => app.stop_indexing(),
        CommandId::Undo => app.undo(),
        CommandId::Redo => app.redo(),
        // 这几个由面板特殊处理，不会走到这里。
        CommandId::OpenGlobalSearch
        | CommandId::QuickLook
        | CommandId::HashSelection
        | CommandId::CompareSelection
        | CommandId::OpenTrash => {}
    }
}

/// 打开聚焦 / 选中项的快速预览。
async fn open_quick_look(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    let paths = app.selection_paths().await;
    let Some(p) = paths.into_iter().next() else {
        let _ = this.update(cx, |v, cx| {
            v.modal = Modal::Info("没有选中文件".to_string());
            cx.notify();
        });
        return;
    };
    match app.preview(&p) {
        Ok(pv) => {
            let _ = this.update(cx, |v, cx| {
                v.preview_cache = Some(pv);
                v.modal = Modal::QuickLook;
                cx.notify();
            });
        }
        Err(e) => {
            let _ = this.update(cx, |v, cx| {
                v.modal = Modal::Info(format!("无法预览 {p:?}：{e}"));
                cx.notify();
            });
        }
    }
}

/// 计算选中文件的哈希并展示。
async fn compute_hash(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    let paths = app.selection_paths().await;
    if paths.is_empty() {
        let _ = this.update(cx, |v, cx| {
            v.modal = Modal::Info("没有选中文件".to_string());
            cx.notify();
        });
        return;
    }
    let mut out = String::from("校验和（MD5 / SHA-1 / SHA-256）：\n\n");
    for (i, p) in paths.iter().enumerate().take(5) {
        match mo_operations::compute_hashes(p, &[HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256]) {
            Ok(h) => {
                out.push_str(&format!("📄 {}\n", p.display()));
                out.push_str(&format!("  MD5      : {}\n", h[&HashAlgo::Md5]));
                out.push_str(&format!("  SHA-1   : {}\n", h[&HashAlgo::Sha1]));
                out.push_str(&format!("  SHA-256 : {}\n\n", h[&HashAlgo::Sha256]));
            }
            Err(e) => out.push_str(&format!("📄 {} 计算失败：{e}\n\n", p.display())),
        }
        if i == 4 && paths.len() > 5 {
            out.push_str(&format!("…共 {} 个文件，仅显示前 5 个", paths.len()));
        }
    }
    let _ = this.update(cx, |v, cx| {
        v.modal = Modal::Info(out);
        cx.notify();
    });
}

/// 比较选中的两项：恰好 2 个条目才执行，结果落入 diff 模态。
async fn run_compare(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    let paths = app.selection_paths().await;
    if paths.len() != 2 {
        let _ = this.update(cx, |v, cx| {
            v.modal = Modal::Info(format!(
                "比较需要恰好选中 2 个条目，当前选中 {} 个。\n\n提示：按住 Shift 或 ⌘A 选择后，在命令面板（⌘⇧P）执行「比较选中的两项」。",
                paths.len()
            ));
            cx.notify();
        });
        return;
    }
    let (a, b) = (paths[0].clone(), paths[1].clone());
    match app.compare_paths(a, b).await {
        Ok(c) => {
            let _ = this.update(cx, |v, cx| {
                v.diff_cache = Some(c);
                v.modal = Modal::Diff;
                cx.notify();
            });
        }
        Err(e) => {
            let _ = this.update(cx, |v, cx| {
                v.modal = Modal::Info(format!("比较失败：{e}"));
                cx.notify();
            });
        }
    }
}

/// 把 app 侧选择快照回灌到 UI 本地缓存（app 侧是唯一事实来源）。
async fn pull_selection(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    let ids = app.selection_ids().await;
    let _ = this.update(cx, |v, cx| {
        v.selection.set_from(&ids);
        cx.notify();
    });
}

/// Enter：打开聚焦 / 选中项——目录进入，文件快速预览。
async fn open_focused(app: &AppState, this: &Entity<RootView>, cx: &mut AsyncApp) {
    // `selection_paths` 无选中时回退聚焦项，正好是键盘光标语义。
    let Some(p) = app.selection_paths().await.into_iter().next() else {
        return; // 空目录：没有聚焦项，静默。
    };
    if p.is_dir() {
        let _ = app.open_directory(&p).await;
    } else if let Ok(pv) = app.preview(&p) {
        let _ = this.update(cx, |v, cx| {
            v.preview_cache = Some(pv);
            v.modal = Modal::QuickLook;
            cx.notify();
        });
    }
}

// ---------- 模态卡片渲染 ----------

impl RootView {
    fn render_command_palette(&self, _entity: &Entity<RootView>) -> Div {
        let list = filtered_commands(&self.cmd_query);
        let idx = self.palette_index;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for (i, id) in list.iter().enumerate() {
            let def = commands().into_iter().find(|c| c.id == *id).unwrap();
            let selected = i == idx;
            let row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .p(px(6.0))
                .bg(if selected {
                    gpui_kit::blue()
                } else {
                    gpui_kit::white()
                })
                .text_color(if selected {
                    gpui_kit::white()
                } else {
                    gpui_kit::black()
                })
                .child(text!(def.category.to_string()))
                .child(text!(def.title.to_string()));
            body = body.child(row);
        }
        if list.is_empty() {
            body = body.child(text!("无匹配命令".to_string()));
        }
        modal_card(
            "命令面板",
            &format!("🔍 {}", self.cmd_query),
            body,
            "↑↓ 选择 · Enter 执行 · Esc 关闭（⌘⇧P 打开）",
        )
    }

    fn render_global_search(&self, _entity: &Entity<RootView>) -> Div {
        let idx = self.palette_index;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for (i, hit) in self.search_results.iter().enumerate() {
            let selected = i == idx;
            let row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .p(px(6.0))
                .bg(if selected {
                    gpui_kit::blue()
                } else {
                    gpui_kit::white()
                })
                .text_color(if selected {
                    gpui_kit::white()
                } else {
                    gpui_kit::black()
                })
                .child(text!(hit.name.clone()))
                .child(text!(format!("{}", hit.path.display())));
            body = body.child(row);
        }
        if self.search_results.is_empty() {
            body = body.child(text!("输入关键词搜索整个文件系统（⌘F）".to_string()));
        }
        modal_card(
            &format!("全局搜索（已索引 {} 项）", self.indexed),
            &format!("🔍 {}", self.search_query),
            body,
            "↑↓ 选择 · Enter 打开 · Esc 关闭",
        )
    }

    fn render_quick_look(&self) -> Div {
        let pv = self.preview_cache.clone();
        let body: Div = match pv {
            Some(p) => {
                let text = p.text.clone().unwrap_or_default();
                match p.kind {
                    mo_preview::PreviewKind::Image => {
                        // 图片：直接加载原图路径。
                        if let Some(path) = &p.image {
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(text!(format!("🖼 {}（{} 字节）", p.title, p.size)))
                                .child(img(path.clone()))
                        } else {
                            div().child(text!(text))
                        }
                    }
                    _ => div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(text!(format!("{} · {} 字节", p.title, p.size)))
                        .child(div().overflow_y_scrollbar().h(px(320.0)).child(text!(text))),
                }
            }
            None => div().child(text!("（无预览）".to_string())),
        };
        modal_card("快速预览", "", body, "Space / Esc 关闭")
    }

    fn render_trash(&self) -> Div {
        let idx = self.palette_index;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for (i, e) in self.trash_entries.iter().enumerate() {
            let selected = i == idx;
            let kind = if e.is_dir { "📁" } else { "📄" };
            let row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .p(px(6.0))
                .bg(if selected {
                    gpui_kit::blue()
                } else {
                    gpui_kit::white()
                })
                .text_color(if selected {
                    gpui_kit::white()
                } else {
                    gpui_kit::black()
                })
                .child(text!(kind.to_string()))
                .child(text!(e.original.to_string_lossy().to_string()))
                .child(text!(human_ago(e.at, now)));
            body = body.child(row);
        }
        if self.trash_entries.is_empty() {
            body = body.child(text!("回收站是空的".to_string()));
        }
        modal_card(
            &format!("回收站（{} 项）", self.trash_entries.len()),
            "",
            body,
            "↑↓ 选择 · Enter 还原 · Delete 永久删除 · E 清空 · Esc 关闭",
        )
    }
    /// 比较 / diff 模态：文件 → 行级 diff；文件夹 → 树比较清单。
    fn render_diff(&self) -> Div {
        let Some(c) = &self.diff_cache else {
            return modal_card("比较", "", div().child(text!("（无结果）".to_string())), "Esc 关闭");
        };
        match c {
            mo_diff::Comparison::Files(f) => self.render_file_diff(f),
            mo_diff::Comparison::Trees(t) => self.render_tree_diff(t),
        }
    }

    /// 文件 diff：逐行渲染区块序列（红删绿增，右侧行号留空表示该侧无此行）。
    fn render_file_diff(&self, f: &mo_diff::FileComparison) -> Div {
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(text!(format!(
                "左：{}（{} 字节）",
                f.a.display(),
                f.a_size
            )))
            .child(text!(format!(
                "右：{}（{} 字节）",
                f.b.display(),
                f.b_size
            )));

        match &f.status {
            mo_diff::FileStatus::Identical => {
                body = body.child(
                    div()
                        .text_color(diff_same_fg())
                        .child(text!("✓ 两个文件完全相同".to_string())),
                );
            }
            mo_diff::FileStatus::BinaryDiff => {
                body = body.child(
                    div()
                        .text_color(diff_del_fg())
                        .child(text!("≠ 二进制文件不同".to_string())),
                );
            }
            mo_diff::FileStatus::TextDiff(td) => {
                // 渲染上限：超大 diff 截断展示，避免一次性铺几万行。
                const MAX_ROWS: usize = 2000;
                let mut shown = 0usize;
                let mut lines = div()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .overflow_y_scrollbar()
                    .h(px(380.0));
                'outer: for op in &td.ops {
                    match op {
                        mo_diff::DiffOp::Equal { old, new, count } => {
                            for i in 0..*count {
                                if shown >= MAX_ROWS {
                                    break 'outer;
                                }
                                lines = lines.child(diff_row(
                                    " ",
                                    Some(old + i),
                                    Some(new + i),
                                    &td.a_lines[old + i],
                                    diff_eq_bg(),
                                    crate::theme::text(),
                                ));
                                shown += 1;
                            }
                        }
                        mo_diff::DiffOp::Delete { old, count } => {
                            for i in 0..*count {
                                if shown >= MAX_ROWS {
                                    break 'outer;
                                }
                                lines = lines.child(diff_row(
                                    "−",
                                    Some(old + i),
                                    None,
                                    &td.a_lines[old + i],
                                    diff_del_bg(),
                                    diff_del_fg(),
                                ));
                                shown += 1;
                            }
                        }
                        mo_diff::DiffOp::Insert { new, count } => {
                            for i in 0..*count {
                                if shown >= MAX_ROWS {
                                    break 'outer;
                                }
                                lines = lines.child(diff_row(
                                    "+",
                                    None,
                                    Some(new + i),
                                    &td.b_lines[new + i],
                                    diff_add_bg(),
                                    diff_add_fg(),
                                ));
                                shown += 1;
                            }
                        }
                    }
                }
                body = body.child(lines);
                if shown >= MAX_ROWS {
                    body = body.child(
                        div()
                            .text_color(crate::theme::muted())
                            .child(text!(format!("（仅显示前 {MAX_ROWS} 行）"))),
                    );
                }
            }
        }
        modal_card("文件比较", "", body, "Esc / Space 关闭")
    }

    /// 文件夹 diff：统计摘要 + 按相对路径排序的条目清单。
    fn render_tree_diff(&self, t: &mo_diff::TreeComparison) -> Div {
        let summary = if t.is_identical() {
            "✓ 两棵目录树完全一致".to_string()
        } else {
            format!(
                "相同文件 {} · 相同目录 {}（未列出）· 不同 {} · 仅左 {} · 仅右 {}",
                t.identical_files, t.identical_dirs, t.different, t.left_only, t.right_only
            )
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(text!(format!("左：{}", t.left.display())))
            .child(text!(format!("右：{}", t.right.display())))
            .child(
                div()
                    .text_color(crate::theme::muted())
                    .child(text!(summary)),
            );

        let mut rows = div()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .overflow_y_scrollbar()
            .h(px(360.0));
        for e in &t.entries {
            let (label, fg) = match e.status {
                mo_diff::TreeStatus::Identical => ("＝", crate::theme::muted()),
                mo_diff::TreeStatus::Different => ("≠", diff_del_fg()),
                mo_diff::TreeStatus::LeftOnly => ("◀", crate::theme::accent()),
                mo_diff::TreeStatus::RightOnly => ("▶", diff_add_fg()),
            };
            let name = format!(
                "{}{}",
                e.rel.display(),
                if e.is_dir { "/" } else { "" }
            );
            rows = rows.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(6.0))
                    .text_color(fg)
                    .child(div().w(px(20.0)).child(text!(label.to_string())))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .truncate()
                            .child(text!(name)),
                    ),
            );
        }
        if t.entries.is_empty() {
            rows = rows.child(text!("（无条目）".to_string()));
        }
        body = body.child(rows);
        modal_card("文件夹比较", "", body, "Esc / Space 关闭")
    }
}

/// 粗略的相对时间（依赖零；用于回收站条目展示）。
fn human_ago(at: u64, now: u64) -> String {
    let d = now.saturating_sub(at);
    if d < 60 {
        "刚刚".to_string()
    } else if d < 3600 {
        format!("{} 分钟前", d / 60)
    } else if d < 86_400 {
        format!("{} 小时前", d / 3600)
    } else {
        format!("{} 天前", d / 86_400)
    }
}

/// 一个居中的模态卡片（标题 + 输入行 + 内容 + 底部提示）。
fn modal_card(title: &str, input: &str, body: impl IntoElement, hint: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .child(
            div()
                .flex()
                .flex_col()
                .w(px(640.0))
                .bg(gpui_kit::rgb(0xf7f7f7))
                .rounded(px(8.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .p(px(10.0))
                        .bg(gpui_kit::rgb(0xe8e8e8))
                        .child(text!(title.to_string()))
                        .child(text!(hint.to_string())),
                )
                .child(div().p(px(8.0)).child(text!(input.to_string())))
                .child(div().p(px(8.0)).child(body)),
        )
}

/// 纯文本信息卡片（哈希结果等）。
fn render_info(text: &str) -> Div {
    let body = div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .overflow_y_scrollbar()
        .h(px(320.0))
        .child(text!(text.to_string()));
    modal_card("信息", "", body, "Esc 关闭")
}

// ---------- diff 模态辅助 ----------

/// diff 行组件：标记 + 左右行号 + 内容（固定列宽保证纵向对齐）。
fn diff_row(
    marker: &str,
    old_no: Option<usize>,
    new_no: Option<usize>,
    content: &str,
    bg: gpui_kit::Rgba,
    fg: gpui_kit::Rgba,
) -> Div {
    let num = |n: Option<usize>| n.map(|v| (v + 1).to_string()).unwrap_or_default();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(6.0))
        .bg(bg)
        .text_color(fg)
        .child(div().w(px(16.0)).child(text!(marker.to_string())))
        .child(
            div()
                .w(px(44.0))
                .text_color(crate::theme::muted())
                .child(text!(num(old_no))),
        )
        .child(
            div()
                .w(px(44.0))
                .text_color(crate::theme::muted())
                .child(text!(num(new_no))),
        )
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .truncate()
                .child(text!(content.to_string())),
        )
}

/// 相同行底色（白）。
fn diff_eq_bg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0xffffff)
}

/// 删除行底色（浅红）。
fn diff_del_bg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0xfdecec)
}

/// 删除行前景（深红）。
fn diff_del_fg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0x8f1f1f)
}

/// 新增行底色（浅绿）。
fn diff_add_bg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0xe9f6e9)
}

/// 新增行前景（深绿）。
fn diff_add_fg() -> gpui_kit::Rgba {
    gpui_kit::rgb(0x1f6b2a)
}

/// 一致提示的前景色（绿）。
fn diff_same_fg() -> gpui_kit::Rgba {
    diff_add_fg()
}

/// 过滤条：显示当前关键词与提示。
fn filter_bar(query: &str) -> impl IntoElement {
    if query.is_empty() {
        return div().h(px(0.0));
    }
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(24.0))
        .px(px(8.0))
        .bg(crate::theme::hover_bg())
        .border_b_1()
        .border_color(crate::theme::separator())
        .text_color(crate::theme::accent())
        .child(text!(format!("🔍 {}", query)))
        .child(
            div()
                .text_color(crate::theme::muted())
                .child(text!("（Esc 清除）".to_string())),
        )
}
