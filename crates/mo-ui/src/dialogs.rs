//! dialogs：需要输入的模态（属性 / 批量重命名 / 压缩 / 磁盘用量 / 标签）。
//!
//! 统一套路：`RootView` 只持有**表单状态**（当前字段、文本、选项），
//! 这里负责渲染 + 把按键结果写回。所有提交动作都通过 `AppState` 发命令，
//! UI 不直接碰文件系统。

use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;
use mo_core::plan_batch_rename;
use mo_operations::mode_string;

use crate::{app::modal_card, theme, RootView};

/// 属性面板里的可编辑状态。
#[derive(Clone)]
pub(crate) struct PropEdit {
    pub path: std::path::PathBuf,
    /// 正在编辑的文件名（不含目录部分）。
    pub name: String,
    /// 权限位（低 9 位）。
    pub mode: u32,
    /// 权限位光标（0..9：rwx 三组的 9 个开关）。
    pub bit: usize,
    pub info: String,
}

impl PropEdit {
    /// 光标所指的权限位。
    pub fn bit_mask(&self) -> u32 {
        1 << (8 - self.bit.min(8))
    }
    pub fn toggle_bit(&mut self) {
        self.mode ^= self.bit_mask();
    }
}

/// 属性 / 权限面板。
pub fn properties(view: &RootView, entity: &Entity<RootView>) -> Div {
    let Some(p) = &view.prop else {
        return modal_card("属性", "", div(), "Esc 关闭");
    };
    let mut body = div().flex().flex_col().gap(px(6.0)).p(px(8.0));

    // 1) 文件名（可编辑）
    body = body
        .child(
            div()
                .text_color(theme::muted())
                .child(text!("名称（可编辑，Enter 重命名）".to_string())),
        )
        .child(field_row(
            &p.name,
            view.form_index == 0,
            format!("prop-name-{}", p.name.len()),
        ));

    // 2) 权限位：9 个开关，← → 移动光标，空格切换，Enter 应用
    let mut bits = div().flex().flex_row().items_center().gap(px(2.0));
    for i in 0..9 {
        let mask = 1u32 << (8 - i);
        let on = p.mode & mask != 0;
        let ch = ["r", "w", "x"][i % 3].to_string();
        let active = view.form_index == 1 && p.bit == i;
        let mut b = div()
            .id(format!("prop-bit-{i}"))
            .flex()
            .items_center()
            .justify_center()
            .size(px(22.0))
            .rounded(px(4.0))
            .text_color(if active {
                theme::text()
            } else if on {
                theme::accent()
            } else {
                theme::muted()
            })
            .bg(if active {
                theme::accent()
            } else {
                theme::surface()
            })
            .child(text!(if on { ch } else { "-".to_string() }));
        let click_entity = entity.clone();
        b.interactivity().on_click(move |_, _window, cx| {
            click_entity.update(cx, |v, cx| {
                if let Some(p) = v.prop.as_mut() {
                    p.bit = i;
                    p.toggle_bit();
                }
                cx.notify();
            });
        });
        bits = bits.child(b);
    }
    body = body
        .child(div().text_color(theme::muted()).child(text!(format!(
            "权限 {}（← → 选位，空格切换，Enter 应用）",
            mode_string(p.mode)
        ))))
        .child(bits);

    body = body.child(
        div()
            .text_size(px(12.0))
            .text_color(theme::muted())
            .child(text!(p.info.clone())),
    );

    modal_card(
        "属性与权限",
        "",
        body,
        "↑↓ 切字段 · ← → 选权限位 · 空格切换 · Enter 应用 · Esc 关闭",
    )
}

/// 批量重命名：规则表单 + 实时预览。
pub fn batch_rename(view: &RootView, _entity: &Entity<RootView>) -> Div {
    let spec = &view.rename_spec;
    let names: Vec<String> = view
        .rename_paths
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();
    let preview = plan_batch_rename(&names, spec);

    let mut body = div().flex().flex_col().gap(px(4.0)).p(px(8.0));
    let fields: [(&str, String, usize); 4] = [
        ("查找", spec.find.clone(), 0),
        ("替换为", spec.replace.clone(), 1),
        ("前缀", spec.prefix.clone(), 2),
        ("后缀", spec.suffix.clone(), 3),
    ];
    for (label, value, idx) in fields {
        body = body
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::muted())
                    .child(text!(label.to_string())),
            )
            .child(field_row(
                &value,
                view.form_index == idx,
                format!("rn-{idx}"),
            ));
    }

    // 开关：序号模式 / 保留扩展名
    let toggles = [
        ("用序号命名", spec.use_index, 4),
        ("保留扩展名", spec.keep_extension, 5),
    ];
    for (label, on, idx) in toggles {
        let active = view.form_index == idx;
        body = body.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .bg(if active {
                    theme::accent()
                } else {
                    theme::surface()
                })
                .text_color(theme::text())
                .child(text!(format!("[{}] {}", if on { "x" } else { " " }, label))),
        );
    }

    let mut list = div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .overflow_y_scrollbar()
        .h(px(200.0));
    for (old, new) in names.iter().zip(preview.iter()).take(12) {
        list = list.child(
            div()
                .flex()
                .flex_row()
                .gap(px(8.0))
                .child(div().w(px(220.0)).truncate().child(text!(old.clone())))
                .child(text!("→".to_string()))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(theme::accent())
                        .child(text!(new.clone())),
                ),
        );
    }
    body = body
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child(text!(format!("预览：共 {} 项（Enter 执行）", names.len()))),
        )
        .child(list);

    modal_card(
        "批量重命名",
        "",
        body,
        "↑↓ 切字段 · 空格开关 · Enter 执行 · Esc 取消",
    )
}

/// 压缩：输入目标文件名（后缀决定格式）。
pub fn archive(view: &RootView, _entity: &Entity<RootView>) -> Div {
    let names: Vec<String> = view
        .rename_paths
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();
    let mut body = div().flex().flex_col().gap(px(6.0)).p(px(8.0));
    body = body
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child(text!(format!(
                    "将 {} 项打包为（.zip / .tar / .tar.gz）：",
                    names.len()
                ))),
        )
        .child(field_row(
            &view.archive_name,
            true,
            "archive-name".to_string(),
        ));
    for n in names.iter().take(8) {
        body = body.child(div().text_size(px(12.0)).truncate().child(text!(n.clone())));
    }
    modal_card("压缩", "", body, "输入文件名 · Enter 执行 · Esc 取消")
}

/// 磁盘空间分析：按大小排序的横向条形图。
pub fn disk_usage(view: &RootView, entity: &Entity<RootView>) -> Div {
    let usage = &view.usage;
    let total: u64 = usage.iter().map(|u| u.size).sum::<u64>().max(1);
    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(px(8.0))
        .overflow_y_scrollbar()
        .h(px(380.0));
    if usage.is_empty() {
        body = body.child(text!("（正在统计…）".to_string()));
    }
    for (i, u) in usage.iter().enumerate() {
        let ratio = (u.size as f64 / total as f64).clamp(0.0, 1.0);
        let width = (520.0 * ratio).max(2.0);
        let name = u
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| u.path.display().to_string());
        let active = view.form_index == i;
        let mut row = div()
            .id(format!("usage-{i}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .bg(if active {
                theme::accent()
            } else {
                theme::surface()
            })
            .text_color(theme::text());
        if !active {
            row = row.hover(|s| s.bg(theme::hover_bg()));
        }
        row =
            row.child(div().w(px(180.0)).truncate().child(text!(format!(
                "{} {}",
                icon_for(u),
                name
            ))))
            .child(
                div()
                    .w(px(width as f32))
                    .h(px(12.0))
                    .rounded(px(3.0))
                    .bg(theme::accent()),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::muted())
                    .child(text!(format!("{} · {} 项", human_size(u.size), u.files))),
            );
        let entity_click = entity.clone();
        let path = u.path.clone();
        row.interactivity().on_click(move |ev, _window, cx| {
            if ev.click_count() >= 2 && path.is_dir() {
                entity_click.update(cx, |v, cx| v.open_entry(path.clone(), cx));
            }
        });
        body = body.child(row);
    }
    modal_card(
        "磁盘空间分析",
        &format!("合计 {}", human_size(total)),
        body,
        "↑↓ 选择 · 双击进入 · Esc 关闭",
    )
}

/// 标签：给选中项选一个颜色。
pub fn tags(entity: &Entity<RootView>, view: &RootView) -> Div {
    let mut body = div().flex().flex_col().gap(px(2.0)).p(px(8.0));
    for (i, (key, label)) in mo_app::TAG_COLORS.iter().enumerate() {
        let active = view.form_index == i;
        let color = tag_color(key);
        let mut row = div()
            .id(format!("tag-{key}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .p(px(4.0))
            .rounded(px(4.0))
            .bg(if active {
                theme::accent()
            } else {
                theme::surface()
            })
            .text_color(theme::text());
        if !active {
            row = row.hover(|s| s.bg(theme::hover_bg()));
        }
        row = row
            .child(
                div()
                    .size(px(12.0))
                    .rounded(px(6.0))
                    .bg(color)
                    .child(text!("".to_string())),
            )
            .child(text!(format!("{}（Enter 应用，Del 清除）", label)));
        let entity_click = entity.clone();
        let color_key = key.to_string();
        row.interactivity().on_click(move |_, _window, cx| {
            entity_click.update(cx, |v, cx| {
                v.apply_tag(color_key.clone());
                cx.notify();
            });
        });
        body = body.child(row);
    }
    modal_card(
        "文件标签",
        "",
        body,
        "↑↓ 选择 · Enter 应用 · Delete 清除 · Esc 关闭",
    )
}

/// 一个可编辑的输入行（带光标提示）。
fn field_row(value: &str, active: bool, id: String) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .h(px(28.0))
        .px(px(6.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::separator()
        })
        .bg(theme::surface())
        .child(text!(format!("{}{}", value, if active { "▏" } else { "" })))
}

fn icon_for(u: &mo_app::DirUsage) -> &'static str {
    if u.dirs > 0 {
        "📁"
    } else {
        "📄"
    }
}

fn human_size(size: u64) -> String {
    crate::file_item::format_size(size)
}

/// 颜色名 → 具体的 RGBA（ Finder 风格的七色）。
pub fn tag_color(name: &str) -> Rgba {
    match name {
        "red" => rgb(0xe24b4a),
        "orange" => rgb(0xef9f27),
        "yellow" => rgb(0xf5c451),
        "green" => rgb(0x639922),
        "blue" => rgb(0x378add),
        "purple" => rgb(0x7f77dd),
        _ => rgb(0x888780),
    }
}
