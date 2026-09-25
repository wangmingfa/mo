// Windows 默认把 exe 打成「控制台子系统」，双击时先弹一个 conhost 黑框装
// 日志；GUI 应用应为 windows 子系统（与 macOS 的 .app 行为对齐：无终端宿主）。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use mo_ui::run;

fn main() {
    run();
}
