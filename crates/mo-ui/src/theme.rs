//! 主题系统：内置浅色 / 深色 / 跟随系统 + 配置文件里的自定义主题。
//!
//! ## 为什么要做成「全局当前调色板 + 同名自由函数」
//!
//! 全项目有约 200 处 `theme::text()` / `theme::surface()` 这类调用，它们大多在
//! `div()` 链式构造中间，拿不到 `Window` / `App`。把主题改成「从上下文取」要动
//! 所有调用点，收益为零；所以这里保留同样的函数签名，内部读一份进程级当前
//! 调色板。GPUI 的渲染与按键回调都在应用线程上跑，切换主题时 `set` + `notify`
//! 即可让下一帧取到新色。
//!
//! ## 深色基底
//!
//! 数值参照 macOS dark aqua 的层级关系：内容区最深、面板抬一档、分隔线靠
//! 亮度而非透明度区分（gpui 没有 alpha 合成层，用透明度叠出来的分隔线在深色
//! 底上会发灰）。选中蓝比浅色档提亮一档，保证在深底上的对比度。
//!
//! ## 自定义主题
//!
//! 写在 `config.json` 的 `custom_themes` 里（见 `mo_config::ThemeColors`）：
//! 指定基底（`dark: true/false`）+ 逐角色覆盖 `#RRGGBB`，未覆盖的角色继承基底。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use gpui_kit::{Hsla, Rgba};
use mo_app::ThemeColors;

/// 一套完整调色板：所有语义角色都取到具体颜色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// 主文字色。
    pub text: Rgba,
    /// 次要文字（分隔标签、提示、未激活项）。
    pub muted: Rgba,
    /// 常规面板底色（工具栏 / 侧边栏 / 状态栏）。
    pub container: Rgba,
    /// 内容区底色（文件列表）。
    pub surface: Rgba,
    /// 边框 / 分隔线。
    pub separator: Rgba,
    /// 选中行底色。
    pub selected_bg: Rgba,
    /// 选中行上的文字色。
    pub selected_text: Rgba,
    /// 悬停行 / 按钮底色。
    pub hover_bg: Rgba,
    /// 列表斑马纹（奇数行底色）。
    pub zebra: Rgba,
    /// 列表表头的列分隔线。
    pub divider: Rgba,
    /// 强调色（激活项 / 焦点边框 / 强调文字）。
    pub accent: Rgba,
    /// 基底是否深色（组件层与自绘细节要按它分支）。
    pub dark: bool,
}

/// 角色键名（配置文件里覆盖用的键）与中文名，主题选择器 / 校验共用一份表。
pub const ROLES: [(&str, &str); 11] = [
    ("text", "主文字"),
    ("muted", "次要文字"),
    ("container", "面板底色"),
    ("surface", "内容底色"),
    ("separator", "边框"),
    ("selected_bg", "选中底色"),
    ("selected_text", "选中文字"),
    ("hover_bg", "悬停底色"),
    ("zebra", "斑马纹"),
    ("divider", "列分隔线"),
    ("accent", "强调色"),
];

impl Palette {
    /// 内置浅色主题（数值取自 gpui 的 `Colors::light()` 体系）。
    pub const fn light() -> Self {
        Self {
            text: rgba(0x1d, 0x1d, 0x1f),
            muted: rgba(0x86, 0x86, 0x8b),
            container: rgba(0xf6, 0xf6, 0xf7),
            surface: rgba(0xff, 0xff, 0xff),
            separator: rgba(0xe8, 0xe8, 0xea),
            selected_bg: rgba(0x29, 0x63, 0xd9),
            selected_text: rgba(0xff, 0xff, 0xff),
            hover_bg: rgba(0xf0, 0xf0, 0xf1),
            zebra: rgba(0xf7, 0xf7, 0xf8),
            divider: rgba(0xd6, 0xd6, 0xda),
            accent: rgba(0xcd, 0xcd, 0xcd),
            dark: false,
        }
    }

    /// 内置深色主题。
    pub const fn dark() -> Self {
        Self {
            text: rgba(0xf2, 0xf2, 0xf4),
            muted: rgba(0x9a, 0x9a, 0xa0),
            container: rgba(0x1f, 0x1f, 0x22),
            surface: rgba(0x17, 0x17, 0x1a),
            separator: rgba(0x2c, 0x2c, 0x31),
            selected_bg: rgba(0x3b, 0x7b, 0xe8),
            selected_text: rgba(0xff, 0xff, 0xff),
            hover_bg: rgba(0x24, 0x24, 0x28),
            zebra: rgba(0x1b, 0x1b, 0x1e),
            divider: rgba(0x3a, 0x3a, 0x40),
            accent: rgba(0x3a, 0x3a, 0x3f),
            dark: true,
        }
    }

    /// 按角色键取色（主题编辑器 / 覆盖合并用）。
    pub fn role(&self, key: &str) -> Option<Rgba> {
        match key {
            "text" => Some(self.text),
            "muted" => Some(self.muted),
            "container" => Some(self.container),
            "surface" => Some(self.surface),
            "separator" => Some(self.separator),
            "selected_bg" => Some(self.selected_bg),
            "selected_text" => Some(self.selected_text),
            "hover_bg" => Some(self.hover_bg),
            "zebra" => Some(self.zebra),
            "divider" => Some(self.divider),
            "accent" => Some(self.accent),
            _ => None,
        }
    }

    /// 写入一个角色覆盖（键不认识则返回 false）。
    pub fn set_role(&mut self, key: &str, v: Rgba) -> bool {
        match key {
            "text" => self.text = v,
            "muted" => self.muted = v,
            "container" => self.container = v,
            "surface" => self.surface = v,
            "separator" => self.separator = v,
            "selected_bg" => self.selected_bg = v,
            "selected_text" => self.selected_text = v,
            "hover_bg" => self.hover_bg = v,
            "zebra" => self.zebra = v,
            "divider" => self.divider = v,
            "accent" => self.accent = v,
            _ => return false,
        }
        true
    }
}

/// `rgb()` 不是 const fn，这里自己拼一个 const 版本。
const fn rgba(r: u8, g: u8, b: u8) -> Rgba {
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

/// 内置主题名（"system" 不是调色板，要按系统外观解析）。
pub const BUILTINS: [&str; 3] = ["light", "dark", "system"];

/// 主题名的中文名（选择器展示用）。
pub fn label(name: &str) -> String {
    match name {
        "light" => "浅色".to_string(),
        "dark" => "深色".to_string(),
        "system" => "跟随系统".to_string(),
        other => other.to_string(),
    }
}

/// 当前调色板槽位。
///
/// 用 `OnceLock<Mutex<..>>` 而不是 `static Mutex<Palette>`：`Palette::light()`
/// 虽然本身是 const，但 `Mutex::new` 里塞 const 求值结果在旧稳定版上仍受限，
/// 这里直接懒初始化最省心。
fn slot() -> &'static Mutex<Palette> {
    static CURRENT: OnceLock<Mutex<Palette>> = OnceLock::new();
    CURRENT.get_or_init(|| Mutex::new(Palette::light()))
}

/// 当前调色板。
pub fn current() -> Palette {
    slot()
        .lock()
        .map(|g| *g)
        .unwrap_or_else(|_| Palette::light())
}

/// 切换当前调色板（调用方随后要 `notify` 让界面重画）。
pub fn set(p: Palette) {
    if let Ok(mut g) = slot().lock() {
        *g = p;
    }
}

/// 解析一个 `#RRGGBB` / `RRGGBB` 颜色串。
pub fn parse_hex(s: &str) -> Option<Rgba> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok();
    Some(rgba(byte(0)?, byte(1)?, byte(2)?))
}

/// 主题名 → 调色板。
///
/// * `system` 按 `appearance_dark`（系统外观）落到 light / dark；
/// * 内置名直接取；
/// * 其余查 `customs`：基底按 `dark` 标志选 light/dark，再逐角色覆盖；
/// * 都不认识则退回浅色（配置写错也不该白屏）。
pub fn resolve(
    name: &str,
    customs: &HashMap<String, ThemeColors>,
    appearance_dark: bool,
) -> Palette {
    match name {
        "light" => return Palette::light(),
        "dark" => return Palette::dark(),
        "system" => {
            return if appearance_dark {
                Palette::dark()
            } else {
                Palette::light()
            }
        }
        _ => {}
    }
    let Some(c) = customs.get(name) else {
        return if appearance_dark {
            Palette::dark()
        } else {
            Palette::light()
        };
    };
    let mut p = if c.dark {
        Palette::dark()
    } else {
        Palette::light()
    };
    for (role, hex) in &c.overrides {
        if let Some(v) = parse_hex(hex) {
            p.set_role(role, v);
        }
    }
    p
}

/// 可选主题列表：内置三档 + 配置里的自定义主题。
pub fn choices(customs: &HashMap<String, ThemeColors>) -> Vec<String> {
    let mut out: Vec<String> = BUILTINS.iter().map(|s| s.to_string()).collect();
    let mut names: Vec<&String> = customs.keys().collect();
    names.sort();
    out.extend(names.into_iter().cloned());
    out
}

// ---- 语义取色（保持旧签名：调用点遍布 div 链，拿不到上下文）----

/// 主文字色。
pub fn text() -> Rgba {
    current().text
}

/// 次要文字（分隔标签、提示、未激活项）。
pub fn muted() -> Rgba {
    current().muted
}

/// 常规面板底色（工具栏 / 侧边栏 / 状态栏）——比内容区抬一档。
pub fn container() -> Rgba {
    current().container
}

/// 内容区底色（文件列表）。
pub fn surface() -> Rgba {
    current().surface
}

/// 边框 / 分隔线。
pub fn separator() -> Rgba {
    current().separator
}

/// 选中行底色（配合 [`selected_text`] 保证对比度）。
pub fn selected_bg() -> Rgba {
    current().selected_bg
}

/// 选中行上的文字色。
pub fn selected_text() -> Rgba {
    current().selected_text
}

/// 悬停行 / 按钮底色（中性，不带色相）。
pub fn hover_bg() -> Rgba {
    current().hover_bg
}

/// 列表斑马纹（奇数行底色）。
pub fn zebra() -> Rgba {
    current().zebra
}

/// 列表表头的列分隔线。
pub fn divider() -> Rgba {
    current().divider
}

/// 强调色（激活项 / 焦点边框 / 强调文字）。
pub fn accent() -> Rgba {
    current().accent
}

/// 当前是否深色基底。
pub fn is_dark() -> bool {
    current().dark
}

/// 把 gpui-component 的全局 `Theme` 对齐到当前调色板。
///
/// 地址栏 / 输入框用的是框架组件，它的**选区底色、光标色、前景色**全部取自
/// `Theme` 这个全局——不覆盖的话，那块会在我们的调色板里突然冒出一套 shadcn
/// 默认色。只覆盖与文本输入有关、且肉眼可见的几项。
///
/// 切主题后必须再调一次：它是全局状态，不会跟着 `Palette` 自动变。
///
/// ⚠️ 全局 `Theme` 只有在框架初始化过之后才存在（布局测试里只 `init` 了 gpui，
/// 没有组件主题全局），`global_mut` 对不存在的全局直接 panic——所以先探测。
pub fn apply_component(cx: &mut gpui_kit::App) {
    use gpui_kit::component::Theme;
    if cx.try_global::<Theme>().is_none() {
        return;
    }
    let p = current();
    let t = Theme::global_mut(cx);
    // 选区用选中底色压到 30% 透明，免得盖住字形。
    let mut sel = Hsla::from(p.selected_bg);
    sel.a = 0.3;
    t.selection = sel;
    t.caret = Hsla::from(p.selected_bg);
    t.foreground = Hsla::from(p.text);
    t.muted_foreground = Hsla::from(p.muted);
    t.border = Hsla::from(p.divider);
    t.background = Hsla::from(p.surface);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置两套主题的每个角色都必须成对存在，且深色基底标志正确。
    #[test]
    fn builtins_cover_all_roles() {
        for p in [Palette::light(), Palette::dark()] {
            for (key, _) in ROLES {
                assert!(p.role(key).is_some(), "角色 {key} 缺失");
            }
        }
        assert!(!Palette::light().dark);
        assert!(Palette::dark().dark);
    }

    /// `#RRGGBB` 解析：带 / 不带井号、大小写都收，坏值返回 None。
    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("#1d1d1f"), Some(rgba(0x1d, 0x1d, 0x1f)));
        assert_eq!(parse_hex("FFFFFF"), Some(rgba(0xff, 0xff, 0xff)));
        assert_eq!(parse_hex(" f0f0f1 "), Some(rgba(0xf0, 0xf0, 0xf1)));
        assert_eq!(parse_hex("12345"), None);
        assert_eq!(parse_hex("gggggg"), None);
    }

    /// 自定义主题 = 基底 + 覆盖；未知角色被忽略，未知主题名回落到内置。
    #[test]
    fn resolve_merges_overrides_onto_base() {
        let mut customs = HashMap::new();
        let mut overrides = HashMap::new();
        overrides.insert("surface".to_string(), "#101010".to_string());
        overrides.insert("nonsense".to_string(), "#ffffff".to_string());
        customs.insert(
            "mine".to_string(),
            ThemeColors {
                dark: true,
                overrides,
            },
        );

        let p = resolve("mine", &customs, false);
        assert_eq!(p.surface, rgba(0x10, 0x10, 0x10));
        // 未覆盖的角色继承深色基底。
        assert_eq!(p.text, Palette::dark().text);
        assert!(p.dark);

        assert_eq!(resolve("light", &customs, true), Palette::light());
        assert_eq!(resolve("nope", &customs, true), Palette::dark());
        assert_eq!(resolve("system", &customs, true), Palette::dark());
        assert_eq!(resolve("system", &customs, false), Palette::light());
    }

    /// 主题切换是全局的：`set` 之后语义函数要立刻反映新色。
    #[test]
    fn set_switches_active_palette() {
        let before = current();
        set(Palette::dark());
        assert_eq!(surface(), Palette::dark().surface);
        assert!(is_dark());
        set(before);
        assert_eq!(current(), before);
    }
}
