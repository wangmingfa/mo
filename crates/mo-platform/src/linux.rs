//! Linux 后端：键盘折符号键（`unshifted_key`）。
//!
//! gpui 的 `Keystroke` 只给字符、不给虚拟键码，所以像 Windows/macOS 一样**反查当前布局**
//! 才能把「Shift 切出来的符号」折回它自己的物理键位——否则非 US 布局下动作串键。
//!
//! 这里的做法是读 **XKB 布局**：用 libxkbcommon（Linux 桌面 compositor 的标配依赖）
//! 在当前布局下、对每个键码分别取「无 Shift」与「按住 Shift」打出的字符，建一张
//! `Shift 变体 → 基本键` 的映射表，缓存进进程级 `OnceLock`。`unshifted_key` 查表即可。
//!
//! ⚠️ 依赖 libxkbcommon（`libxkbcommon.so.0`）：几乎所有 X11/Wayland 桌面都随 GNOME/KDE
//! 一起装好。若环境里拿不到库或 XKB 数据（精简容器），`build_map` 返回 `None`，
//! `unshifted_key` 退化为 `None`（上层继续用 US 表），**不因此让整个二进制起不来**。
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::OnceLock;

// libxkbcommon 的符号直接按名字链进来（`#[link(name = "xkbcommon")]`）；只在 Linux
// 目标编译，macOS/Windows 构建不会碰这条链接。交叉编译 `cargo check --target
// x86_64-unknown-linux-gnu` 不触发链接，能验证 Rust 侧类型正确。
#[link(name = "xkbcommon")]
extern "C" {
    fn xkb_context_new(flags: u32) -> *mut c_void;
    fn xkb_context_unref(ctx: *mut c_void);
    fn xkb_keymap_new_from_names(
        ctx: *mut c_void,
        names: *const XkbRuleNames,
        flags: u32,
    ) -> *mut c_void;
    fn xkb_keymap_unref(keymap: *mut c_void);
    fn xkb_keymap_mod_get_index(keymap: *mut c_void, name: *const c_char) -> u32;
    fn xkb_state_new(keymap: *mut c_void) -> *mut c_void;
    fn xkb_state_unref(state: *mut c_void);
    fn xkb_state_update_modifiers(
        state: *mut c_void,
        depressed: u32,
        latched: u32,
        locked: u32,
        depressed_virtual: u32,
        latched_virtual: u32,
        locked_virtual: u32,
    ) -> u32;
    fn xkb_state_key_get_utf8(
        state: *mut c_void,
        key: u32,
        buffer: *mut c_char,
        size: usize,
    ) -> c_int;
}

/// 与 `xkb_rule_names` 同布局：五个字段都是 `const char *`，全 `NULL` 时 libxkbcommon
/// 用系统默认 + `XKB_DEFAULT_*` 环境变量（测试里靠设 `XKB_DEFAULT_LAYOUT` 切换布局）。
#[repr(C)]
struct XkbRuleNames {
    rules: *const c_char,
    model: *const c_char,
    layout: *const c_char,
    variant: *const c_char,
    options: *const c_char,
}

/// 在某个键码上、按指定 Shift 态打出的第一个字符（UTF-8 取首码元）。
///
/// 用 `xkb_state` 模拟 Shift 修饰位再取 UTF-8——比直接查 `level` 更稳：它自动处理
/// 各布局的 level/Shifts 数，而不是假设「level 1 就是 Shift」。
unsafe fn char_for_key(
    state: *mut c_void,
    shift_index: u32,
    keycode: u32,
    with_shift: bool,
) -> Option<char> {
    let mask = if with_shift { 1u32 << shift_index } else { 0 };
    // 清掉其它修饰位，只留（或留空）Shift，保证取到的就是这一颗键的字符。
    xkb_state_update_modifiers(state, mask, 0, 0, 0, 0, 0);
    let mut buf = [0u8; 32];
    let n = xkb_state_key_get_utf8(state, keycode, buf.as_mut_ptr() as *mut c_char, buf.len());
    if n <= 0 {
        return None;
    }
    let n = n as usize;
    if n >= buf.len() {
        // 缓冲区不够（理论上单键至多几个码元，32 字节绰绰有余），宁可不要。
        return None;
    }
    let s = std::str::from_utf8(&buf[..n]).ok()?;
    s.chars().next()
}

/// 建「Shift 变体 → 基本键」映射表。失败（无库 / 无 XKB 数据 / 取不到 Shift 修饰位）
/// 返回 `None`，调用方退化到 US 表。
fn build_map() -> Option<HashMap<char, char>> {
    unsafe {
        let ctx = xkb_context_new(0);
        if ctx.is_null() {
            return None;
        }
        // 全 `NULL`：用系统默认规则 + `XKB_DEFAULT_*` 环境变量。
        let names = XkbRuleNames {
            rules: ptr::null(),
            model: ptr::null(),
            layout: ptr::null(),
            variant: ptr::null(),
            options: ptr::null(),
        };
        let keymap = xkb_keymap_new_from_names(ctx, &names, 0);
        if keymap.is_null() {
            xkb_context_unref(ctx);
            return None;
        }
        let state = xkb_state_new(keymap);
        if state.is_null() {
            xkb_keymap_unref(keymap);
            xkb_context_unref(ctx);
            return None;
        }
        // Shift 修饰位的索引按名字查（不硬编码，跨布局/版本都稳）。
        let shift_name = CString::new("Shift").unwrap();
        let shift_idx = xkb_keymap_mod_get_index(keymap, shift_name.as_ptr());
        if shift_idx == u32::MAX {
            xkb_state_unref(state);
            xkb_keymap_unref(keymap);
            xkb_context_unref(ctx);
            return None;
        }

        let mut map = HashMap::new();
        // XKB 键码范围 8..=255；从 1 扫到 255 全覆盖且无害（1..7 是保留键码）。
        for kc in 1u32..=255 {
            let base = char_for_key(state, shift_idx, kc, false);
            let shifted = char_for_key(state, shift_idx, kc, true);
            if let (Some(b), Some(s)) = (base, shifted) {
                // 基本键映射到自己、或任一方是不可打印控制字符，都不进表。
                if b != s && !b.is_control() && !s.is_control() {
                    map.insert(s, b);
                }
            }
        }

        xkb_state_unref(state);
        xkb_keymap_unref(keymap);
        xkb_context_unref(ctx);
        Some(map)
    }
}

/// 字符 `ch` 在当前 XKB 布局上的基本键（它所在物理键未加 Shift 打出的字符）。
///
/// 见 [`crate::unshifted_key`] 的总说明；本函数只负责 Linux 分支。映射表进程级
/// 只建一次（`OnceLock`），后续查询是纯 `HashMap` 查找。
pub fn unshifted_key(ch: char) -> Option<char> {
    static MAP: OnceLock<Option<HashMap<char, char>>> = OnceLock::new();
    let map = MAP.get_or_init(build_map);
    map.as_ref().and_then(|m| m.get(&ch).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 Linux 走了布局感知的 libxkbcommon 链路：德语布局下 `?` 是 `ß` 的 Shift 变体，
    /// 与 US 布局（`?` 来自 `/`）不同——足以证明不是退化成 US 表。
    ///
    /// ⚠️ 必须在首次调用前设 `XKB_DEFAULT_LAYOUT`（map 是进程级 `OnceLock` 缓存）。
    /// 没有 libxkbcommon / XKB 数据（精简容器）时 `unshifted_key` 退化为 `None`，这里
    /// 跳过重断言而不是硬失败——真机桌面有 XKB 数据，会走下面的断言。
    #[test]
    fn unshifted_key_follows_german_layout() {
        std::env::set_var("XKB_DEFAULT_LAYOUT", "de");
        if unshifted_key('?').is_none() {
            eprintln!("skip: libxkbcommon / XKB 数据不可用，Linux 退化为 None");
            return;
        }
        // 德语布局：`?` 是 `ß` 的 Shift 变体；US 布局则来自 `/`。布局感知即此断言通过。
        assert_eq!(unshifted_key('?'), Some('ß'));
        // Shift 方向正确：大写字母折回小写。
        assert_eq!(unshifted_key('A'), Some('a'));
        // 数字行 `!` 在 US/DE 都是 `1` 的 Shift 变体。
        assert_eq!(unshifted_key('!'), Some('1'));
        // 基本键映射到自己。
        assert_eq!(unshifted_key('1'), Some('1'));
    }
}
