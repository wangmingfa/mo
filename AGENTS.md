# AGENTS.md — Mo 项目协作约定

## UI 实现：优先使用 gpui-kit 组件

- **优先使用 gpui-kit（`gpui-kit = "0.6"`）提供的组件来实现 UI**，不要手搓等价物。
  滚动用 `overflow_y_scrollbar()` / `uniform_list`，按钮、文本（`text!`）等先找 kit
  里的现成组件；只有 kit 确实没有的能力才自己拼（自拼时在 devlog 记一笔）。
- kit 0.6.x 的已知坑位（空格键名折 `" "`、`Div` 三参 `on_click`、嵌套点击
  `stop_propagation`、headless 测试 `.test_support()` + `debug_selector` 等）
  见 `devlog/*.md`，写 UI 前先查，别凭记忆重踩。

## 质量门禁

改动必须过一遍（一键：`./scripts/run-ci.sh`）：

1. `cargo fmt --check`
2. `cargo check --all-targets --all-features`
3. `cargo clippy -- -D warnings`
4. `cargo test --all-features`

分层方向 `mo-core`→`mo-fs`→`mo-operations`→`mo-cache`/`mo-config`→`mo-app`→`mo-ui`：
UI 只经 `AppState` 发命令、`EventBus` 订阅，绝不直接碰文件系统。

## 提交约定

- commit message 用中文，按 intent 拆分（feat / fix / chore 各自独立），
  中间提交必须能独立编译。
- 只提交本次改动的文件，不顺手夹带工作区里其他未提交改动。
