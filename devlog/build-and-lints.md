# 构建与依赖的坑

日期：2026-09-17。工具链：rustc 1.98.1。

---

## 1. `block 0.1.6` 的 future-incompat 告警

* **现象**：`cargo run` 输出 `warning: the following packages contain code that will be rejected by a future version of Rust: block v0.1.6`。
* **根因**：传递依赖链 `gpui-kit → gpui-pre-macos → cocoa 0.26.1 → block 0.1.6`。上游 SSheldon/rust-block 已弃维护，其 `extern static _NSConcreteStackBlock: Class` 用空枚举（uninhabited）类型声明 static——rust-lang/rust#74840 将成硬错误。
* **修法**：源码拷入 `third_party/block`，static 类型改为 `*const Class`（它只被取符号地址、从不读值，语义不变），workspace 挂 `[patch.crates-io] block = { path = "third_party/block" }`。

## 2. path patch 后才暴露的告警（cap-lints 豁免消失）

* **现象**：打上 path patch 后编译反而多出告警。
* **根因**：crates.io 依赖的告警被 cargo 的 cap-lints 机制压制（cap 到 allow）；path 依赖**不受豁免**，上游代码里的裸 `extern`（无显式 ABI，新版 rustc warn-by-default）立即现形。
* **修法**：补丁内所有 `extern` / `extern fn` 补显式 `"C"`。**教训**：打 path patch 前预留一轮「把上游历史告警清零」的工作量。

## 3. workspace lints 继承与本地 lints 段冲突

* **现象**：mo-ui 的 Cargo.toml 已有本地 `[lints.rust]`（上面 objc check-cfg 用的），再写 `[lints] workspace = true` 后 manifest 解析失败。
* **根因**：cargo 不允许同一 crate 同时使用本地 lints 表和 workspace 继承。
* **修法**：mo-ui 手动复制 workspace 策略到本地 `[lints]` 段并注释提醒**同步维护**。改 workspace lints 时记得同步 mo-ui。

## 4. `[workspace.lints]` 的实际效果

* **约定**：`clippy.all = "deny"`（告警即编译失败，防累积）、`rust.unsafe_code = "warn"`。首次全量跑清零了 33 条存量告警（unit let-binding、redundant closure、`&PathBuf` → `&Path` 等，多数可 `cargo clippy --fix` 自动修）。
* **注意**：`--fix` 之后要用 `cargo fmt` 收尾，自动修复的格式经常不齐。

## 5. rust-version 声明

* **要点**：workspace `rust-version` 是**最低**构建版本门槛（当前 1.98.1），不锁 toolchain；若需团队/CI 锁同一版本，另加 `rust-toolchain.toml`。

## 6. Windows 任务栏 / 标题栏图标：gpui 只认 exe 里 ID=1 的图标资源

* **现象**：`cargo run` 跑裸 `mo.exe`，Windows 任务栏 / 标题栏 / 资源管理器里都是通用图标（macOS 那边靠 `icon.rs` 运行时 `setApplicationIconImage` 已解决，Windows 无对应入口）。
* **根因**：gpui 的 Windows 后端注册窗口类时用 `LoadImageW(module, MAKEINTRESOURCE(1), IMAGE_ICON, …)` 取图标（`gpui-pre-windows/src/platform.rs::load_icon`）。exe 里没有**整数 ID 1** 的 ICON 资源就 `unwrap_or_default()` 成空 HICON，于是全链路回退默认图标。运行期没有从 PNG 设图标的公开 API。
* **修法**：加 `crates/mo-ui/build.rs`（仅 `cfg(windows)` 生效）：用 workspace 里的 `image`（已含 `png`+`ico` feature）把 `assets/icon.png` 缩放成 16/24/32/48/64/128/256 多帧写成 `.ico`，生成一句 `1 ICON "…"` 的 `.rc`，再用 `embed-resource` 编译并链接（它产出 `mo-icon.lib` 并 `cargo:rustc-link-arg-bins`）。ID 正好是 1，匹配 `load_icon`。build-deps 放 `[target.'cfg(windows)'.build-dependencies]`，非 Windows 不拉这些依赖、脚本 no-op。
* **注意**：`embed-resource` 靠 `rc.exe`（Windows SDK）编译 `.rc`；找不到会返回 `NotAttempted`——脚本对此只 `cargo:warning` 不阻断构建，`Failed` 才 panic。验证：`[System.Drawing.Icon]::ExtractAssociatedIcon('mo.exe')` 能取到图标即已内嵌。Explorer 的 exe 图标有缓存，可能要重开资源管理器 / 重新固定快捷方式才刷新；运行窗口的任务栏图标重启进程即更新。

