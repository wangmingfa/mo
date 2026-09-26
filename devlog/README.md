# devlog —— 开发踩坑日志

记录开发 Mo 过程中**实际踩过、并已定位根因**的坑。每条记录包含：现象 → 根因 → 修法，附涉及的源码位置。

与逐日流水账不同，这里按**主题**组织，目的是：再次遇到同类问题时，能直接查到结论，而不是从头调试。

## 目录

| 文件 | 内容 |
|---|---|
| [gpui-layout-and-interaction.md](gpui-layout-and-interaction.md) | GPUI 布局（flex / uniform_list / 定宽槽位）与交互（click / hover / 双击 / 快捷键）的坑 |
| [macos-platform.md](macos-platform.md) | macOS 平台层：红绿灯定位、⌘Q 退出、Dock 图标、objc FFI |
| [windows-port.md](windows-port.md) | Windows 移植：cfg 门控、`explorer /select`、`IFileOperation` 回收站与 `$I` 反查、卷宗/推出的 `Unsupported` vs `Failed` 契约、shell 图标解 alpha、WinRT PDF 渲染 |
| [build-and-lints.md](build-and-lints.md) | 构建与依赖：future-incompat 补丁、workspace lints、cfg 检查 |
| [engine-testing.md](engine-testing.md) | 引擎逻辑与测试方法：headless 布局测试、通用算法陷阱 |
| [async-runtime.md](async-runtime.md) | 异步 runtime：后台任务洪泛饿死 UI、刷新竞态、跨目录快照覆盖 |
| [customization.md](customization.md) | 自定义系统：配置文件 BOM、主题取色的全局槽位、深色档语义色 |
| [search.md](search.md) | 全局搜索与索引：落盘、启动自举、增量维护、爬取上限 |
| [preview.md](preview.md) | 缩略图与快速预览：降采样编码选择、先开窗后到图、入场动画 |
| [remote-transfer.md](remote-transfer.md) | 跨端点传输：`Endpoint` 判据、`FileSystem::read_file/is_dir`、`TransferOperation` |
| [selection.md](selection.md) | 选择交互：反选 / 框选 / type-ahead、软链 FileId |
| [transfer-badge.md](transfer-badge.md) | 传输指示与任务面板：小块 + 浮层、估速、暂停恢复、回收站面板 |

## 约定

* 修好一个新坑后，追加到对应主题文件，格式：`### 现象（简短）` + 根因 + 修法。
* 如果一个坑横跨多个主题（例如 cfg 告警涉及 objc 宏），放在最主要的那篇，其余位置加一行链接。
* 只记**已验证**的结论；猜测和未修的问题写在条目末尾的「待办」里。
