# 示例插件：字幕工具（srt-tools）

这个目录是一个**能跑的 Mo 插件示例**，用来演示插件系统（P1–P4）的整条链路：

```
清单 manifest.json → provider 进程（classify / preview / list）→ 命令进菜单 / 列表源进侧栏与面板
```

它认领 `.srt` / `.vtt` 两种扩展名，做三件事：

| 能力 | 落在哪 | 效果 |
|---|---|---|
| `types` | 「种类」列 | `.srt`/`.vtt` 在种类列显示「字幕」 |
| `provider.classify` | 种类列（覆盖 types 文案） | 进一步区分「SRT 字幕」/「WebVTT 字幕」 |
| `provider.preview` | 预览面板 | 字幕文件回一段文本摘要（前若干行） |
| `provider.list` | 侧栏一行 + 只读面板 | 「最近字幕」列表源（示例给出两个工具入口） |
| `commands` | 命令面板 / 右键菜单 | 「统计字幕条目数」（选中字幕后可用） |

provider 用 **TypeScript（bun 运行，零编译）** 写——插件系统的设计前提就是**进程隔离、
不要求匹配主程序工具链**，所以任意语言都行。本例用 TypeScript 是因为 bun 能直接跑 `.ts`，
你不需要任何构建步骤。协议是 **JSON Lines over stdio**，与设计稿 `devlog/plugin-system.md` §5 一致。

## 文件

```
plugin-srt-tools/
├── manifest.json        # 清单（Mo 加载的全部依据；icon 必填，没有装不进来）
├── icon.png             # 扩展图标（扩展管理器卡片上画的那颗，必填）
├── bin/srt-tools.ts     # provider 进程（有 shebang + 可执行位，bun 直接跑）
├── test-provider.ts     # 协议自测：不依赖 Mo，单独验证 provider 这一腿
└── README.md
```

## 协议长什么样

宿主每帧发一行：

```json
{"id":1,"method":"classify","params":{"path":"/电影/movie.srt","name":"movie.srt","size":1234}}
```

插件回一行（id 原样回显）：

```json
{"id":1,"result":{"label":"SRT 字幕"}}
```

出错就回 error（宿主按「健康地答不了」处理，不计失败、回落内置）：

```json
{"id":1,"error":"unknown method: frobnicate"}
```

生命周期：宿主先发 `initialize{"protocol":1}`，插件回
`{"name":"srt-tools","version":"1.0.0","methods":["classify","preview","list"]}`；之后对每条
文件各发一帧。进程常驻，闲置 30s 回收，崩溃/超时由宿主 kill + 退避停用（面板会亮「已停用」）。

stdout **只能吐协议帧**——任何多余输出都会被宿主当成坏帧丢弃；日志请走 stderr。

## 怎么装

把整个 `plugin-srt-tools/` 目录复制（或软链）到扩展根目录下的 `srt-tools/`：

```
<配置目录>/mo/extensions/srt-tools/
├── manifest.json
├── icon.png
└── bin/srt-tools.ts
```

- 配置目录即 `config.json` 所在目录（见 `mo_config`）。
- 清单里的 `icon` 是**必填项**（相对扩展目录的图片路径，png/jpg/webp/gif 等），且文件
  必须真的在目录里——缺了不允许安装。装好后扩展管理器的卡片上有「刷新」按钮
  （循环箭头）：改完这个目录里的代码点一下即从来源重装，启停状态保留，开发迭代不用
  卸了再装。
- `bin/srt-tools.ts` 需要可执行位（`chmod +x`）；宿主按扩展目录解析 `run[0]`，靠 shebang
  `#!/usr/bin/env bun` 拉起。
- 运行环境要求 **bun** 在 PATH 上（命令面板里的「统计字幕条目数」走的是 `sh`，不依赖 bun；
  但 provider 进程本身需要 bun 来跑 `.ts`）。
- Windows 注意：`.ts` 没有原生 shebang 支持，需要在 `provider.run` 改成
  `["bun", "bin/srt-tools.ts"]` 并确认 Windows 上 `bun` 可被发现（或用 `.cmd` 包装）。
  本示例以 macOS / Linux 为主（Mo 的主力平台）。

装完后重启 Mo，选中一个 `.srt`/`.vtt`：右键菜单与命令面板出现「统计字幕条目数」，种类列
显示「SRT 字幕」/「WebVTT 字幕」，空格预览看到摘要，侧栏出现「最近字幕」行、点开是只读列表面板。

## 怎么验证（不依赖 Mo）

```sh
bun test-provider.ts
```

逐帧发 `initialize` / `classify` / `preview` / `list`，断言回包形状，等价于宿主那一侧会做的
事。全部通过即说明 provider 协议正确。也可以手动喂协议行验证 provider 自己：

```sh
printf '%s\n' \
  '{"id":1,"method":"initialize","params":{"protocol":1}}' \
  '{"id":2,"method":"classify","params":{"path":"/m/movie.srt","name":"movie.srt","size":123}}' \
  | bun bin/srt-tools.ts
```

## 与设计稿的对应关系

- 清单 schema：`devlog/plugin-system.md` §3（实际字段以 `crates/mo-app/src/extensions.rs`
  的 `Manifest` 为准；本文档示例已对齐）。
- provider 协议：`§5`，实现见 `crates/mo-app/src/provider.rs`。
- `classify` 只收 `label`（界面今天只有一个「种类」消费点）；`group`/`icon_key`/`columns`
  协议允许但界面暂不消费，写了也无害。
- `list` 第一版只读：收 `id`/`name`/`path`/`subtitle`；`icon`/`next`(翻页) 不消费。
- 能力边界：`read-contents` 的执行点在宿主派发侧——只有清单声明了它，`classify` 入参才会
  附 `head_b64`（文件头 4KiB）；没授权时连字段都不会出现。
