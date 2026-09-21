//! 自定义快捷键：键组解析、默认键表、用户覆盖与冲突检测。
//!
//! ## 为什么要做成「键表先行」
//!
//! 原先每个快捷键都是 `on_key_down` 里一个 `if platform && key == "t"` 这样的
//! 硬编码分支——想让用户改键，就必须把「按下了什么」和「该做什么」拆开：
//! 这里把按键归一化成 [`KeyCombo`]，查 [`Keymap`] 得到动作 id，再由 UI 层派发。
//! 硬编码分支全部删掉，否则用户改了键、旧键位仍然生效，等于没改。
//!
//! ## 平台主修饰键
//!
//! 配置里统一写 `cmd+...`，含义按平台落地：macOS 是 ⌘，Windows / Linux 是 Ctrl。
//!
//! ⚠️ 这里**不能**直接用 gpui 的 `modifiers.platform`：Windows 后端把它映射成
//! 真正的 Win 键（`VK_LWIN`），Ctrl 在 `modifiers.control` 里。照 `platform` 比
//! 就会让所有「⌘快捷键」在 Windows 上按不出来（Win+T 还被任务栏抢走）。
//! 非 macOS 上 `ctrl` 与 `cmd` 两个写法都解析成同一个位，用户按平台习惯写哪个都行。

use std::collections::HashMap;

use gpui_kit::Keystroke;

/// 一个键组：修饰键组合 + 主键。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyCombo {
    /// 平台主修饰键（macOS ⌘ / 其它平台 Ctrl）。
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// 主键，归一后的小写形式（字母）或 gpui 特殊键名。
    pub key: String,
}

/// 这个平台是否把 ⌘ 当主修饰键。
const fn has_command_key() -> bool {
    cfg!(target_os = "macos")
}

impl KeyCombo {
    /// 从 gpui 按键事件构造。
    pub fn from_keystroke(ks: &Keystroke) -> Self {
        let m = &ks.modifiers;
        Self {
            cmd: if has_command_key() {
                m.platform
            } else {
                m.control
            },
            // 非 macOS 上 Ctrl 就是主修饰键，不再单列一位，否则
            // "ctrl+c" 与 "cmd+c" 会成两个互斥的键组。
            ctrl: if has_command_key() { m.control } else { false },
            alt: m.alt,
            shift: m.shift,
            key: normalize(&ks.key),
        }
    }

    /// 是否带任一主修饰键——用来区分「全局组合键」与「裸键」（回车 / 空格 / 方向键）。
    pub fn is_modified(&self) -> bool {
        self.cmd || self.ctrl || self.alt
    }

    /// 匹配一次按键：四个修饰位都必须严格相等。
    ///
    /// 严格相等而不是「包含」：否则 `cmd+shift+z`（重做）会把 `cmd+z`（撤销）
    /// 也判中，两个动作抢同一次按键。
    pub fn matches(&self, other: &KeyCombo) -> bool {
        self.cmd == other.cmd
            && self.ctrl == other.ctrl
            && self.alt == other.alt
            && self.shift == other.shift
            && self.key == other.key
    }

    /// 解析 `cmd+shift+p` 形式的键串；大小写与顺序都不敏感。
    pub fn parse(spec: &str) -> Option<Self> {
        let mut out = Self {
            cmd: false,
            ctrl: false,
            alt: false,
            shift: false,
            key: String::new(),
        };
        // 符号写法（`⌘⇧P`）先展开成 `+` 分隔，才能和 `cmd+shift+p` 走同一条路径。
        let expanded = spec
            .replace('⌘', "cmd+")
            .replace('⌃', "ctrl+")
            .replace('⌥', "alt+")
            .replace('⇧', "shift+");
        for part in expanded.split(['+', '-']) {
            let p = part.trim().to_lowercase();
            if p.is_empty() {
                continue;
            }
            match p.as_str() {
                "cmd" | "command" | "super" | "meta" | "win" => out.cmd = true,
                // 非 macOS 上 Ctrl 就是主修饰键：`ctrl+c` 与 `cmd+c` 同义，
                // 用户按自己平台的习惯写哪种都能用。
                "ctrl" | "control" => {
                    if has_command_key() {
                        out.ctrl = true;
                    } else {
                        out.cmd = true;
                    }
                }
                "alt" | "option" | "opt" => out.alt = true,
                "shift" => out.shift = true,
                "esc" => out.key = "escape".into(),
                "return" => out.key = "enter".into(),
                "del" | "delete" => out.key = "backspace".into(),
                "pgup" | "prior" => out.key = "pageup".into(),
                "pgdn" | "next" => out.key = "pagedown".into(),
                "space" | "spacebar" | "␣" => out.key = " ".into(),
                _ => out.key = normalize(&p),
            }
        }
        // 主键必须是认识的键名：否则「不是键」这种手打错的值会被当成一个
        // 永远按不出来的键，动作直接失联——这里返回 None 让调用方退回默认。
        (!out.key.is_empty() && known_key(&out.key)).then_some(out)
    }

    /// 展示串：macOS 用符号，其它平台写 `Ctrl+Shift+P`。
    pub fn format(&self) -> String {
        if cfg!(target_os = "macos") {
            let mut s = String::new();
            if self.alt {
                s.push('⌥');
            }
            if self.ctrl {
                s.push('⌃');
            }
            if self.cmd {
                s.push('⌘');
            }
            if self.shift {
                s.push('⇧');
            }
            // 方向键用箭头符号，跟系统菜单一致（`key_label` 会给「Down」）。
            let label = match self.key.as_str() {
                "up" => "↑".to_string(),
                "down" => "↓".to_string(),
                "left" => "←".to_string(),
                "right" => "→".to_string(),
                _ => self.key_label(),
            };
            s.push_str(&label);
            s
        } else {
            let mut parts: Vec<String> = Vec::new();
            if self.ctrl || self.cmd {
                parts.push("Ctrl".into());
            }
            if self.alt {
                parts.push("Alt".into());
            }
            if self.shift {
                parts.push("Shift".into());
            }
            parts.push(self.key_label());
            parts.join("+")
        }
    }

    /// 配置串（写进 config.json 的形式，始终用 `cmd` 表示平台主修饰键）。
    pub fn spec(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.cmd {
            parts.push("cmd".into());
        }
        if self.ctrl {
            parts.push("ctrl".into());
        }
        if self.alt {
            parts.push("alt".into());
        }
        if self.shift {
            parts.push("shift".into());
        }
        // 空格在配置里写 `space`，裸空格串既难读也容易被 trim 掉。
        parts.push(if self.key == " " {
            "space".into()
        } else {
            self.key.clone()
        });
        parts.join("+")
    }

    /// 主键的展示形式。
    ///
    /// ⚠️ 只有**单字符**键才做大写转换：`enter` / `escape` / `up` 这些多字符
    /// 键名若走同一条分支，会被首字母大写成「E」，菜单提示就成了胡话。
    fn key_label(&self) -> String {
        let mut chars = self.key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() => c.to_ascii_uppercase().to_string(),
            _ => match self.key.as_str() {
                " " => "Space".to_string(),
                "escape" => "Esc".to_string(),
                "backspace" => "Delete".to_string(),
                // 其余多字符键名首字母大写展示：enter→Enter、up→Up、f2→F2。
                other => match other.chars().next() {
                    Some(c) => {
                        let mut s = c.to_ascii_uppercase().to_string();
                        s.push_str(&other[c.len_utf8()..]);
                        s
                    }
                    None => String::new(),
                },
            },
        }
    }
}

/// 键名归一：字母转小写，其余保持原样（gpui 已经给的是小写名）。
///
/// ⚠️ 空格是唯一的例外：gpui 全平台把它报成规范名 `"space"`（macOS / Windows /
/// Linux / Web 一致），而本模块内部统一用**单个空格字符** `" "` 表示空格
/// （见 `parse` 的 `"space" => " "`，以及 `spec()` / `format()` / `key_label()`
/// 都以 `" "` 为准）。不在这里折这一步，运行时按键（`key == "space"`）与键表里
/// 的绑定（`key == " "`）就永远对不上——`lookup` 落空，空格相关动作静默失效。
fn normalize(s: &str) -> String {
    if s.eq_ignore_ascii_case("space") {
        return " ".into();
    }
    if s.chars().count() == 1 {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

/// gpui 会报出来的特殊键名（外加单字符键）。
///
/// 用来校验用户手写的配置：不认识的键名一律视为写错，退回默认键位，
/// 而不是绑到一个永远按不出来的「幽灵键」上。
fn known_key(key: &str) -> bool {
    if key.chars().count() == 1 {
        return true;
    }
    matches!(
        key,
        "escape"
            | "enter"
            | "tab"
            | "backspace"
            | "delete"
            | "up"
            | "down"
            | "left"
            | "right"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "f1"
            | "f2"
            | "f3"
            | "f4"
            | "f5"
            | "f6"
            | "f7"
            | "f8"
            | "f9"
            | "f10"
            | "f11"
            | "f12"
    )
}

/// 一个可绑定动作：稳定 id + 中文名 + 默认键串。
pub struct Binding {
    pub id: &'static str,
    pub label: &'static str,
    pub default: &'static str,
}

/// 全部可重映射动作。id 是配置里的键，一旦发布就不要改名。
pub const BINDINGS: [Binding; 28] = [
    Binding {
        id: "app.quit",
        label: "退出应用",
        default: "cmd+q",
    },
    Binding {
        id: "palette.open",
        label: "命令面板",
        default: "cmd+shift+p",
    },
    Binding {
        id: "search.global",
        label: "全局搜索",
        default: "cmd+f",
    },
    Binding {
        id: "select.all",
        label: "全选",
        default: "cmd+a",
    },
    Binding {
        id: "edit.undo",
        label: "撤销",
        default: "cmd+z",
    },
    Binding {
        id: "edit.redo",
        label: "重做",
        default: "cmd+shift+z",
    },
    Binding {
        id: "tab.new",
        label: "新建标签页",
        default: "cmd+t",
    },
    Binding {
        id: "tab.close",
        label: "关闭标签页",
        default: "cmd+w",
    },
    Binding {
        id: "tab.prev",
        label: "上一个标签页",
        default: "cmd+shift+[",
    },
    Binding {
        id: "tab.next",
        label: "下一个标签页",
        default: "cmd+shift+]",
    },
    Binding {
        id: "pane.split",
        label: "分栏开关",
        default: "cmd+shift+d",
    },
    Binding {
        id: "pane.prev",
        label: "上一个窗格",
        default: "cmd+shift+left",
    },
    Binding {
        id: "pane.next",
        label: "下一个窗格",
        default: "cmd+shift+right",
    },
    Binding {
        id: "view.list",
        label: "列表视图",
        default: "cmd+1",
    },
    Binding {
        id: "view.grid",
        label: "网格视图",
        default: "cmd+2",
    },
    Binding {
        id: "view.gallery",
        label: "画廊视图",
        default: "cmd+3",
    },
    Binding {
        id: "view.columns",
        label: "列视图",
        default: "cmd+4",
    },
    Binding {
        id: "file.properties",
        label: "显示简介",
        default: "cmd+i",
    },
    Binding {
        id: "file.duplicate",
        label: "创建副本",
        default: "cmd+d",
    },
    Binding {
        id: "clipboard.copy",
        label: "复制",
        default: "cmd+c",
    },
    Binding {
        id: "clipboard.cut",
        label: "剪切",
        default: "cmd+x",
    },
    Binding {
        id: "clipboard.paste",
        label: "粘贴",
        default: "cmd+v",
    },
    Binding {
        id: "clipboard.copy_path",
        label: "拷贝路径",
        default: "cmd+alt+c",
    },
    Binding {
        id: "nav.parent",
        label: "进入上级目录",
        default: "cmd+up",
    },
    Binding {
        id: "file.trash",
        label: "移到废纸篓",
        default: "delete",
    },
    Binding {
        id: "list.rename",
        label: "重命名",
        default: "f2",
    },
    Binding {
        id: "list.open",
        label: "打开（默认应用 / 进入目录）",
        default: "enter",
    },
    Binding {
        id: "list.preview",
        label: "快速预览",
        default: "space",
    },
];

/// 作用于**下层文件列表**的绑定 id：选中项、剪贴板、文件操作、对当前目录的读取。
///
/// 模态 / 对话框打开时这些动作不派发（判定见 [`touches_the_browser`]，用在
/// `RootView` 的键盘路由里）：那个列表在遮罩后面，用户看不见它，选中项却会在
/// 背后被改掉。
///
/// 最容易被撞见的一例是 ⌘A——在「连接到服务器」的输入框里按 ⌘A，选中的是后面的
/// 文件；⌘X / ⌘V / ⌘Z / Delete 更糟，会真的动文件或剪贴板。反差在于导航 / 窗口 /
/// 视图 / 切换模态这些**不碰选中项**的动作照旧放行（Finder 的 sheet 也是这样）。
///
/// ⚠️ 新增绑定若作用于浏览区，记得加进来——`browser_scoped_ids_all_exist` 守住
/// 拼写（写错的 id 会静默失效，等于没拦）。
pub const BROWSER_SCOPED: [&str; 13] = [
    "select.all",
    "edit.undo",
    "edit.redo",
    "file.properties",
    "file.duplicate",
    "clipboard.copy",
    "clipboard.cut",
    "clipboard.paste",
    "clipboard.copy_path",
    "file.trash",
    "list.rename",
    "list.open",
    "list.preview",
];

/// 这个绑定 id 是否作用于下层文件列表（模态打开时应当被吞掉）。
pub fn touches_the_browser(id: &str) -> bool {
    BROWSER_SCOPED.contains(&id)
}

/// 某个动作在本平台的默认键串。
///
/// ⚠️ macOS 与 Windows / Linux 的两个动作默认键位**故意不同**，这是平台惯例，
/// 不是笔误：
///
/// * `list.open`：Finder 里 Enter 不是「打开」，⌘↓ 才是；
/// * `list.rename`：Finder 里 Enter 就是重命名（Windows 资源管理器用 F2）。
///
/// 键表可重映射，所以这只影响默认值——用户想反过来照样能改。
pub fn default_spec(b: &Binding) -> &'static str {
    if has_command_key() {
        match b.id {
            "list.open" => return "cmd+down",
            "list.rename" => return "enter",
            _ => {}
        }
    }
    b.default
}

/// 某动作在当前平台的默认键位展示串（右键菜单的快捷键提示用它）。
///
/// 提示从键表推导而不是另抄一份字面量：抄的那份会漂——改了绑定忘了改提示，
/// 用户照着提示按键却没反应，比没有提示更糟。
pub fn hint(id: &str) -> String {
    BINDINGS
        .iter()
        .find(|b| b.id == id)
        .and_then(|b| KeyCombo::parse(default_spec(b)))
        .map(|c| c.format())
        .unwrap_or_default()
}

/// 当前键表：`(键组, 动作 id)`，按平台主修饰键分两段查询。
#[derive(Default)]
pub struct Keymap {
    entries: Vec<(KeyCombo, &'static str)>,
    /// 被用户显式解绑的默认键位。
    ///
    /// 解绑不等于「这个键没人管」：`⌘T` 解绑后如果继续往下冒泡，会漏给系统 /
    /// 输入组件触发别的行为。命中这里就整次按键吞掉，才算真的「无操作」。
    unbound: Vec<KeyCombo>,
}

impl Keymap {
    /// 用默认键表 + 用户覆盖构建。
    ///
    /// 覆盖里值为空串表示**解绑**该动作；值为坏键串则忽略该条覆盖（保留默认），
    /// 用户手打错一个字母不该让快捷键整体失效。
    pub fn build(overrides: &HashMap<String, String>) -> Self {
        let mut out = Self::default();
        for b in BINDINGS.iter() {
            let base = default_spec(b);
            let spec = overrides.get(b.id).map(|s| s.as_str()).unwrap_or(base);
            if spec.trim().is_empty() {
                // 解绑：不进表，但记下默认键位，命中时吞键。
                if let Some(default) = KeyCombo::parse(base) {
                    out.unbound.push(default);
                }
                continue;
            }
            if let Some(combo) = KeyCombo::parse(spec) {
                out.entries.push((combo, b.id));
            } else {
                // 覆盖值解析不出来：退回默认，别把动作弄丢。
                tracing::warn!(
                    "快捷键 {} 的配置值「{spec}」不是合法键组，沿用默认 {}",
                    b.id,
                    base
                );
                if let Some(combo) = KeyCombo::parse(base) {
                    out.entries.push((combo, b.id));
                }
            }
        }
        out
    }

    /// 这个键组是否被显式解绑（命中则应吞掉按键）。
    pub fn is_unbound(&self, combo: &KeyCombo) -> bool {
        self.unbound.iter().any(|c| c.matches(combo))
    }

    /// 查一次按键对应的动作。
    pub fn lookup(&self, ks: &KeyCombo) -> Option<&'static str> {
        self.entries
            .iter()
            .find(|(c, _)| c.matches(ks))
            .map(|(_, id)| *id)
    }

    /// 某动作当前绑到的键组（被解绑则 `None`）。
    pub fn combo_of(&self, id: &str) -> Option<&KeyCombo> {
        self.entries
            .iter()
            .find(|(_, other)| *other == id)
            .map(|(c, _)| c)
    }

    /// 该键组是否已被别的动作占用（重绑定时做冲突提示）。
    pub fn conflict(&self, id: &str, combo: &KeyCombo) -> Option<&'static str> {
        self.entries
            .iter()
            .find(|(c, other)| *other != id && c.matches(combo))
            .map(|(_, other)| *other)
    }

    /// 把动作绑到新键组（同 id 的旧条目一并替换）。
    pub fn rebind(&mut self, id: &'static str, combo: KeyCombo) {
        self.entries.retain(|(_, other)| *other != id);
        self.entries.push((combo, id));
    }

    /// 解绑一个动作：从表里摘掉，并把该键位记入「显式解绑」名单（命中时吞键）。
    pub fn unbind(&mut self, id: &str, combo: KeyCombo) {
        self.entries.retain(|(_, other)| *other != id);
        if !self.unbound.iter().any(|c| c.matches(&combo)) {
            self.unbound.push(combo);
        }
    }

    /// 恢复某个动作的默认键位。
    pub fn reset(&mut self, id: &str) {
        self.entries.retain(|(_, other)| *other != id);
        self.unbound.clear();
        if let Some(b) = BINDINGS.iter().find(|b| b.id == id) {
            if let Some(combo) = KeyCombo::parse(default_spec(b)) {
                self.entries.push((combo, b.id));
            }
        }
    }

    /// 全部动作回到默认（清空解绑名单）。
    pub fn reset_all(&mut self) {
        *self = Self::build(&HashMap::new());
    }

    /// 动作 id → 中文名。
    pub fn label(id: &str) -> &str {
        BINDINGS
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.label)
            .unwrap_or(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(spec: &str) -> KeyCombo {
        KeyCombo::parse(spec).expect("测试里的键串必须能解析")
    }

    /// 解析：顺序无关、大小写无关、别名与符号都收。
    #[test]
    fn parse_is_lenient() {
        let a = ks("cmd+shift+p");
        let b = ks("Shift+Cmd+P");
        assert_eq!(a, b);
        assert!(a.cmd && a.shift && !a.ctrl && !a.alt);
        assert_eq!(a.key, "p");
        // 符号形式与别名。
        assert_eq!(KeyCombo::parse("⌘K").unwrap().spec(), "cmd+k");
        assert_eq!(KeyCombo::parse("esc").unwrap().key, "escape");
        assert_eq!(KeyCombo::parse("space").unwrap().key, " ");
        assert!(KeyCombo::parse("").is_none(), "空串不是合法键组");
    }

    /// 回归：空格必须能从**真实按键事件**命中默认绑定。
    ///
    /// 走 `from_keystroke`（gpui 报 `key == "space"`）而不是 `parse`——两者对空格
    /// 的内部表示若不一致，`lookup` 会落空，空格快速预览就会静默失效。
    #[test]
    fn space_from_keystroke_hits_default_binding() {
        let ev = Keystroke {
            key: "space".into(),
            ..Default::default()
        };
        let combo = KeyCombo::from_keystroke(&ev);
        // 必须与配置串路径落到同一个内部名，否则重绑 / 键位展示会漂。
        assert_eq!(combo.key, KeyCombo::parse("space").unwrap().key);
        let map = Keymap::build(&HashMap::new());
        assert_eq!(
            map.lookup(&combo),
            Some("list.preview"),
            "空格应命中快速预览"
        );
    }

    /// 匹配是严格相等：多一个修饰键就不算命中（撤销 / 重做不能互相抢键）。
    #[test]
    fn matching_is_exact_on_modifiers() {
        let undo = ks("cmd+z");
        let redo = ks("cmd+shift+z");
        assert!(!undo.matches(&redo));
        assert!(redo.matches(&ks("cmd+shift+Z")));
    }

    /// 默认键表能整体构建，且没有内部冲突。
    #[test]
    fn default_keymap_has_no_conflicts() {
        let map = Keymap::build(&HashMap::new());
        assert_eq!(map.entries.len(), BINDINGS.len(), "默认表应覆盖全部动作");
        for b in BINDINGS.iter() {
            let combo = map.combo_of(b.id).expect("默认键位应存在");
            assert!(
                map.conflict(b.id, combo).is_none(),
                "默认键表里 {} 与别的动作撞了",
                b.id
            );
        }
    }

    /// 覆盖：改绑、解绑、坏值退回默认。
    #[test]
    fn overrides_apply_unbind_and_recover() {
        let mut o = HashMap::new();
        o.insert("tab.new".to_string(), "cmd+alt+t".to_string());
        o.insert("search.global".to_string(), "".to_string());
        o.insert("view.list".to_string(), "不是键".to_string());
        let map = Keymap::build(&o);

        assert!(map.lookup(&ks("cmd+alt+t")) == Some("tab.new"));
        assert!(map.lookup(&ks("cmd+t")).is_none(), "旧键位应当失效");
        assert!(map.combo_of("search.global").is_none(), "空串 = 解绑");
        assert_eq!(
            map.combo_of("view.list").map(|c| c.spec()),
            Some("cmd+1".to_string()),
            "坏键串应退回默认"
        );
    }

    /// 重绑与复位。
    #[test]
    fn rebind_and_reset() {
        let mut map = Keymap::build(&HashMap::new());
        map.rebind("list.preview", ks("cmd+p"));
        assert!(map.lookup(&ks("space")).is_none(), "旧键位应被换掉");
        assert_eq!(map.lookup(&ks("cmd+p")), Some("list.preview"));
        assert_eq!(map.conflict("list.preview", &ks("cmd+p")), None);
        map.rebind("list.open", ks("cmd+p"));
        assert_eq!(
            map.conflict("list.open", &ks("cmd+p")),
            Some("list.preview"),
            "应当报出占用者"
        );
        map.reset("list.preview");
        assert_eq!(
            map.combo_of("list.preview").map(|c| c.spec()),
            Some("space".to_string())
        );
    }

    /// 平台主修饰键落地：非 macOS 上 Ctrl 就是 `cmd` 位（gpui 的 `platform`
    /// 在 Windows 是 Win 键，直接用它会让所有组合键失灵）。
    #[test]
    fn platform_modifier_maps_to_control_off_macos() {
        use gpui_kit::{Keystroke, Modifiers};
        let m = Modifiers {
            control: true,
            ..Default::default()
        };
        let ks = Keystroke {
            modifiers: m,
            key: "p".to_string(),
            ..Default::default()
        };
        let combo = KeyCombo::from_keystroke(&ks);
        if has_command_key() {
            // macOS：Ctrl 是独立的 `ctrl` 位，主修饰键仍是 ⌘。
            assert!(combo.ctrl && !combo.cmd);
        } else {
            assert!(
                combo.cmd && !combo.ctrl,
                "Windows / Linux 上 Ctrl 即主修饰键"
            );
            assert!(combo.matches(&ks2("cmd+p")), "Ctrl+P 应当命中 cmd+p");
            assert!(combo.matches(&ks2("ctrl+p")), "ctrl+p 写法同义");
            assert!(!combo.matches(&ks2("cmd+shift+p")));
        }

        fn ks2(spec: &str) -> KeyCombo {
            KeyCombo::parse(spec).expect("测试里的键串必须能解析")
        }
    }

    /// 高度相似的键组不能互相误命中（严格比较每一位）。
    #[test]
    fn near_misses_do_not_match() {
        let target = ks("cmd+shift+p");
        assert!(!target.matches(&ks("cmd+p")));
        assert!(!target.matches(&ks("cmd+shift+p+alt")));
        assert!(!target.matches(&ks("cmd+shift+o")));
    }

    /// 展示串按平台分支，配置串始终用 `cmd`。
    #[test]
    fn formatting_is_platform_aware() {
        let c = ks("cmd+shift+p");
        assert_eq!(c.spec(), "cmd+shift+p");
        let shown = c.format();
        if cfg!(target_os = "macos") {
            assert_eq!(shown, "⌘⇧P");
        } else {
            assert_eq!(shown, "Ctrl+Shift+P");
        }
    }

    /// 平台默认键位差异：macOS 上 Enter 是重命名、⌘↓ 才是打开；其它平台
    /// 反过来（Enter 打开、F2 重命名）。空格在两个平台都是预览。
    #[test]
    fn open_and_rename_defaults_are_platform_specific() {
        let open = BINDINGS.iter().find(|b| b.id == "list.open").unwrap();
        let rename = BINDINGS.iter().find(|b| b.id == "list.rename").unwrap();
        let preview = BINDINGS.iter().find(|b| b.id == "list.preview").unwrap();
        if has_command_key() {
            assert_eq!(default_spec(open), "cmd+down", "Finder：⌘↓ 打开");
            assert_eq!(default_spec(rename), "enter", "Finder：Enter 重命名");
        } else {
            assert_eq!(default_spec(open), "enter", "资源管理器：Enter 打开");
            assert_eq!(default_spec(rename), "f2", "资源管理器：F2 重命名");
        }
        assert_eq!(default_spec(preview), "space", "预览两个平台都是空格");

        // 打开与预览必须是**不同**的键：否则「Enter 变成预览」这类回归会重现。
        let map = Keymap::build(&HashMap::new());
        let open_combo = map.combo_of("list.open").unwrap().clone();
        let preview_combo = map.combo_of("list.preview").unwrap().clone();
        assert!(
            !open_combo.matches(&preview_combo),
            "打开与预览撞键：{open_combo:?} vs {preview_combo:?}"
        );
        let rename_combo = map.combo_of("list.rename").unwrap().clone();
        assert!(!rename_combo.matches(&open_combo), "重命名与打开不该撞键");
    }

    /// `BROWSER_SCOPED` 里的 id 必须都是真的绑定——写错的 id 会静默失效，
    /// 等于那一条没拦（模态打开时照样打到下层文件列表）。
    #[test]
    fn browser_scoped_ids_all_exist() {
        for id in BROWSER_SCOPED {
            assert!(
                BINDINGS.iter().any(|b| b.id == id),
                "BROWSER_SCOPED 里的 {id:?} 不是绑定 id（拼错了？）"
            );
            assert!(touches_the_browser(id), "{id:?} 应当被判为作用于浏览区");
        }
        // 反例：窗口级动作不该被拦，否则模态打开时连新建标签页都没了。
        assert!(!touches_the_browser("tab.new"));
        assert!(!touches_the_browser("palette.open"));
    }

    /// 菜单提示由键表推导（抄来的字面量会随重映射漂移）。
    #[test]
    fn hints_come_from_the_keymap() {
        assert!(!hint("list.open").is_empty());
        assert!(!hint("list.preview").is_empty());
        assert!(hint("no.such-action").is_empty(), "未知动作给空提示");
        if has_command_key() {
            assert_eq!(hint("list.open"), "⌘↓");
        } else {
            assert_eq!(hint("list.open"), "Enter");
            assert_eq!(hint("list.rename"), "F2");
        }
    }
}
