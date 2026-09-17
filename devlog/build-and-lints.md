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
