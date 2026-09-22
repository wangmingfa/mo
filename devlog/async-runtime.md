# devlog · 异步 runtime 与后台任务

后台任务的并发模型踩坑。核心教训一句话：**UI 的响应路径（窗口取回）和后台批量任务共享同一个 runtime，后者一旦洪泛，前者就会被饿死，用户看到的就是闪烁。**

## 1. 逐条 spawn 的元数据回填会淹死小 runtime

**现象**：打开 2.7 万项的目录后快速滚动，列表整屏变成 `…` 占位符约 1 秒，与正常内容交替闪烁（用户录屏 + 逐帧亮度量化确认）。

**根因**：`MetadataScheduler::load` 为每个条目 spawn 一个 tokio 任务（信号量限 8 并发），且：

* `LocalFileSystem::metadata` 在 async 任务里直接调 `std::fs::metadata`（阻塞 stat）；
* `WriteBehind` 的 `put_many`（SQLite + fsync）也同步跑在 async 任务里；
* `runtime()` 只有 `worker_threads(2)`。

窗口取回（`visible_window`）的新任务排进 2.7 万个任务的积压后面，延迟从毫秒级恶化到秒级。

**修法**：批量。64 条一批 → blocking 线程整批 stat + 一条 SQLite 事务 + **一次状态写锁批量写回**（`AppState::update_metadata_batch`）。写锁次数、spawn 次数、fsync 次数全部从「每条一次」降到「每批一次」。

**原则**：给小 runtime 供数的后台任务，API 必须按「批」设计；阻塞调用（stat、SQLite、图片解码）只能出现在 `spawn_blocking` 里。

## 2. 刷新泵 sync 与补窗任务的竞态

**现象**：滚动时重复派发补窗任务，窗口被拉回旧范围，加剧上面的积压。

**根因**：`RootView::sync`（每 120ms 一轮）无条件按「当前窗口范围」重取快照并 `pending = None`，而渲染闭包刚为「新可见范围」设置了 `pending` 并派发了补窗任务——sync 把它吞了，下一帧渲染只好再派一次。

**修法**（所有权要单一）：

* `pending` 只归渲染闭包的补窗任务管：sync 在 `pending` 非空时整轮跳过，落地时也绝不碰它；
* 补窗任务完成时，只清除「与自己请求范围匹配」的 `pending`（期间用户可能又滚动了，新请求不能被吞）；
* sync 只在空闲时刷新窗口（元数据回填晚 120ms 无感）。

## 3. 跨目录的取回快照覆盖

**现象（潜在）**：补窗/刷新任务在途时切换目录，旧目录的快照落地后覆盖新目录的窗口，且覆盖后「已覆盖」判断为真，错误内容会一直停留。

**修法**：`visible_window` 返回 `(读取时的目录路径, start, entries)`，落地前校验 `v.path` 一致才应用；校验失败时匹配的 `pending` 一并清除（否则新目录同范围请求会被残留的 pending 吞掉）。

**原则**：任何「异步取数 → 回写共享状态」的路径，回写时都要校验取数时的前提（这里是目录）仍然成立。

## 附：量化定位手法

录屏用 ffmpeg 按 10fps 抽帧，PIL 算列表区域暗像素占比：空白帧 ~0.0005、正常帧 ~0.0135，双峰分布一目了然，比肉眼逐帧看快得多。

## 4. 进目录时「明显卡一下」：渲染路径上有两处同步重活（2026-09-22）

**现象**：进入内容比较多的目录，界面明显卡一下。

**先量化**（一次性探针 `cargo run --example`，跑完即删；dev 档，本机）：

| 活 | 单价 | 一屏（约 30 行） |
|---|---|---|
| `mo_platform::file_icon`＝`iconForFile:` + 重绘 40px + PNG 编码 | 1.5ms（普通文件）～12ms（`.app`） | **50–400ms** |
| 其中进程内**第一张**（类加载 / 图标服务连接预热） | 60–220ms | 一次性 |
| 缩略图冷生成（5120×2880 JPEG → 解码 + 缩放 + 128px 编码） | 86ms | 见 (2) |

两条都对得上，都是**在渲染线程上做重活**：

### (1) 系统图标在渲染闭包里同步取

`file_list` 的行渲染直接 `app.file_icon(&entry.path)`，而这个函数当年是「查不到就
当场问系统」——AppKit + 位图重绘 + PNG 编码 + **写盘**全压在渲染线程上。进一个新
目录 = 一屏全冷 → 一帧卡 50–400ms。这条正是本仓自己立的红线（渲染路径上不许出现
AppKit 调用 + 位图编码 + 写盘）被绕过的一次。

修法：拆成「查表」与「取图」两半。

* 渲染路径的 `AppState::file_icon` **只查表**：没命中就记一笔、返回 `None`
  （那一行退回内置 SVG），零 IO。
* 新增**图标泵**（`AppState::spawn_icon_pump`，50ms 一拍）：在 blocking 池里问系统、
  写盘、落缓存，然后置 `dirty` —— 交给已有的 120ms 刷新泵合并成一次重绘。图标
  于是「晚一两帧浮现」，而不是「当场把界面冻住」。
* 缓存键分两种：普通文件按**扩展名**（三百个 `.txt` 只该问一次系统），目录 / 包 /
  无扩展名的文件按**路径**（它们的图标各不相同）。除 `is_dir` 外还要用后缀表兜住
  **符号链接形态的包**，否则一堆 `.app` 快捷方式会共用同一张图标（测试
  `path_keys_never_cross_serve` 就是被这条咬出来的）。
* 后台任务的门禁是新加的 `mo_platform::appkit_usable()`：`is_main_thread()` 对后台
  线程恒为假，光判它会把该做的事也一并跳过。应用在 `mo_ui::run()` 里
  `mark_main_loop_ready()` 置一个**单向闩**；测试进程永远不置位 → 后台一律保守跳过，
  不会撞上「主队列无人 drain」的 `dispatch_sync` 死锁。
* 顺带省一半：有缩略图的行不再白要系统图标——那些行画的是缩略图。

### (2) 缩略图按「整个窗口」派发，而窗口 = 可见区 ± `BUFFER`(=100)

`ensure_window` 与 `sync_panel` 在窗口落地后都 `request(整个窗口)`；进目录时那个窗口
是 `INITIAL_WINDOW` = 200 条 —— 图片目录里就是**两百个 86ms 的解码任务**排队（信号量
4），四核被占住几秒。而且**目录越大白解得越多**，正好对上「内容多的目录才卡」。

修法：派发点挪到**渲染那一帧**，只给这一帧真的画出来的行排队（`file_list` / `grid`
各自的 `want_thumbs`）。请求本身幂等，滚动时新露出来的行也能立刻排上队——这也正是
调度器自己声明的语义「只为当前可见的条目生成缩略图」，此前实现与文档是背离的。

⚠️ 幂等有个前提得补上：`ThumbnailState::Loading` **从来没有被置位过**（条目从 `Idle`
直接到 `Loaded`/`Failed`），而 UI 手里那份窗口快照要等下一轮同步才更新 —— 只判
`Idle` 的话，每帧都会给同一张图排一个新任务。所以调度器新增 `inflight` 集合给「已
排队的 id」销账。

### 同类但不是事故（量过，先留着）

`AppState::network_shares()` 与 `volumes()` 每帧被侧边栏问到，按 5s TTL 缓存；到期
那一次会在渲染线程里起 `mount` **子进程**（10 次共 30ms，约 3ms/次）加一次 AppKit
卷宗查询。3ms 不足一帧，暂不动；要动就是同一套「后台刷新 + 缓存只读」。

### 还能再快的一刀（未做）

图片目录里缩略图单价仍由**整图解码**决定（86ms，`image` 只给 `decode()`，不给 DCT
缩放）。直接依赖 `zune-jpeg` 用 `set_max_scale(1/8)` 解码，单价能掉到十几 ms 量级 ——
但那要自己处理色彩空间 / 方向，属于独立的一刀，先记在这里。

## 5. 上一节修完「还是卡一下」：`dispatch_sync` 的活还是主线程在干（2026-09-22）

第 4 节把图标从**渲染闭包**挪到了后台图标泵，用户复测：切到「下载」（27 512 条）
**还是卡一下**。这一节是把它彻底按住的记录。

### 先说结论：挪走「调用」不等于挪走「活」

`mo_platform::file_icon` 内部是 `on_main_thread`，也就是
`dispatch_sync(main_queue)` —— **不管谁调它，那段活都是主线程在跑**。图标泵虽然
跑在 blocking 池里，但泵一次抓 40 张（`ICON_BATCH`），主线程就要连着忙
40 × 单价。所以上一节只是把「一帧里冻 50–400ms」换成了「一拍里冻几十毫秒」，
用户当然还觉得卡。

### 量化（一次性 example 探针，跑完即删；dev 档）

`~/Downloads` 前 24 个文件，逐张把 `file_icon` 拆段（主线程，单位 ms）：

| 段 | 单价 | 占比 |
|---|---|---|
| `iconForFile:` 问系统 | 0.03 | 3% |
| 重绘到 40px（`lockFocus`/`drawInRect:`/`unlockFocus`） | 0.07 | 8% |
| `TIFFRepresentation` + `NSBitmapImageRep` | 0.16 | 18% |
| **`representationUsingType:` 编 PNG** | **0.62** | **70%** |
| 拷字节 | ~0.00 | ~0 |
| **合计** | **~0.9** | |

一拍 40 张 ≈ **36ms 主线程**（`.app` 密集的目录更长；冷启动第一张还要额外几十毫秒
连图标服务）。

### 同一批顺手量掉的：进目录的真实耗时（一次性 example 探针）

`open_local(~/Downloads)` 端到端 **312ms**（热 280ms）——但**全在 blocking 池**，
主线程一点没堵：同一探针里挂的 10ms 心跳循环（跑在主线程前台执行器上）全程没有
一次缺口 > 30ms。

| 段 | 耗时 | 在哪 |
|---|---|---|
| `read_dir_blocking`（27 512 条） | 131ms | blocking |
| `set_entries`（建索引 + 排序） | 43ms | blocking |
| `entries.clone()`（给后台校验的副本） | 2.6ms | blocking |
| `prime_from_cache`（SQLite 批量读 27 512 条） | 133ms | blocking |
| `FileSystemWatcher::watch` | 1.4ms | **主线程**（`load_path` 里直接调） |
| `MetadataScheduler::load` 的同步段（排序 + 分批 spawn） | 8.6ms | **主线程** |
| `volumes()` + `network_shares()`（5s TTL 到点那一次） | 13ms | **主线程** |
| 渲染期 `tag_of()` × 30 行 | 2.1ms | **主线程**（每帧，见下） |

结论：**切换本身 300ms 是「慢」，不是「卡」**——界面是在转的；卡的是图标那 36ms。

### 修法：按时间配额滴灌

`ICON_BUDGET_MS = 3`：泵每拍**一条条**取（新增 `IconCache::pop_next`），花满 3ms
就收工，剩下的留在队列里等下一拍（`ICON_BATCH` 退化成条数兜底）。

* 3ms 在一帧（16.6ms）里绰绰有余 → 不再掉帧；
* 代价是图标按 ~60–80 张/秒浮现：一屏（30 行）约 0.4–0.5s 填满；
* ⚠️ 取出来就等于「问过了」（`asked` 账本），所以**配额用完时没取走的必须还在
  队列里** —— 不能像以前那样 `take(40)` 再丢掉剩下的，否则那些行永远停在内置 SVG。

### 还能再快的一刀（未做，但方向明确）

PNG 编码占了 70%。把它挪下主线程（主线程只留 `iconForFile:` + 重绘 + 取像素，
编码交给 `image` 在后台做，或把 `NSBitmapImageRep` retain 后交给后台调
`representationUsingType:`），主线程单价能从 ~0.9ms 掉到 ~0.26ms —— 同样 3ms 配额
下图标就该「瞬时到位」而不是滴灌。要做的话注意位图格式（预乘 alpha）与
`retain`/`release` 的所有权。

### 顺手记下、还没动的小账

`file_list` 行渲染里每行都调 `AppState::tag_of(path)` → `config()` → **每次读一遍
JSON 配置**（实测 0.07ms/次，30 行 ≈ 2ms/帧）。不致命，但按「渲染路径只查表」的
原则，标签表该在 UI 侧缓存一份。等哪次动标签相关再收。

## 6. 再把「进目录卡一下」按住：编码挪出主线程 + 预填只填首屏（2026-09-22）

第 5 节把图标泵改成「按 3ms 配额滴灌」之后仍留了两笔账：主线程单价还是 ~0.9ms/张，
而「打开大目录」本身要 300ms。这一节把两笔都清掉。

### (1) PNG 编码（占 70%）挪出主线程

第 5 节量过：`file_icon` 每张 0.9ms 里，**PNG 编码 0.62ms（70%）**。这段活没有理由
留在主线程——它只是「从像素到字节」的纯计算。

改成两段式：平台层只交出**像素**，编码归调用方在后台做。

```rust
// mo-platform：只做必须主线程的事
pub struct IconRaster { width: u32, height: u32, rgba: Vec<u8> }  // RGBA8、预乘 alpha
pub fn file_icon_raster(path: &Path) -> Option<IconRaster>

// mo-thumbnails：后台段
pub fn unpremultiply_rgba(rgba: &mut [u8])                  // 预乘 → 直通
pub fn encode_rgba_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>>
```

⚠️ **取像素这件事有两个坑，都踩了一遍**（靠一次性 example 探针把格式打出来才发现）：

1. 自己拼 `initWithBitmapDataPlanes:...`（想指定 8 位/通道、RGBA）→ AppKit 直接
   `Inconsistent set of values to create NSBitmapImageRep` + `Bad colorspace name …`，
   rep 是 nil。十个参数的组合被拒，原因不明（参数顺序、类型都核对过）。
2. 退回「新建 40pt NSImage → `lockFocus`/`drawInRect:` → `TIFFRepresentation` →
   `imageRepWithData:` → 读 `bitmapData`」——这条路**能拿到 rep，但读出来是错的**：
   探针打印格式发现 **Retina 上 40pt = 80px**、而且解出来是 **16 位/通道**
   （实测 `spp=4 bps=16 bpr=640`）。按 8 位读就是错位数据：半透明像素数、中心色全错。

最后走 **CoreGraphics**，位深与字节序都由我们指定，还顺手设了插值质量：

```rust
let cg = [image CGImageForProposedRect:&rect context:nil hints:nil];   // AppKit 到此为止
CGBitmapContextCreate(buf, 40, 40, 8, 160, deviceRGB,
                      kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
CGContextSetInterpolationQuality(ctx, kCGInterpolationHigh);  // 512→40 是 12 倍下采样
CGContextDrawImage(ctx, rect, cg);
```

**实测（dev 档，`~/Downloads` 16 个文件）：**

| 段 | 改前 | 改后 |
|---|---|---|
| 主线程（问系统 + 重绘 + 拷像素） | 0.28ms | **0.47ms** |
| 后台（预乘还原 + PNG 编码） | 0（原来也在主线程） | 0.67ms |
| **主线程合计** | **0.90ms** | **0.47ms** |

主线程掉了一半，剩下的是 `CGImageForProposedRect:`（触发了图标首次解码）——那是
固有成本。按 3ms 配额：每拍从 ~3 张变成 ~6 张。

### (2) 打开目录：缓存预填从「全部」收到「首屏」

原实现 `prime_from_cache(cache, &mut dir.entries)` 拿**整份** entries 去
`cache.get_many()`——27 512 条目录就是一次查 2.7 万个 id，实测 **133ms**（第 4 节
的表里就有这一行，当时只记了「在 blocking 池」）。

改成 `prime_visible_from_cache(cache, &mut dir, upto)`：只查**视图序**前
`FIRST_SCREEN_ROWS`(=200) 条。首屏之外那两万多条用户此刻一行都看不到，而它们本来
就要被后台 `stat` 校验（`scheduler.load`）走一遍——预填只是让 UI **先有数据**，
省不掉任何 stat。

顺带修了一处语义错位：`scheduler.load(..., Some(0..first_screen))` 的「首屏优先」
是按下标判的，而传进去的是 `read_dir` **原始序**的副本——排一次序之后
`0..200` 就只是「目录里的前 200 个」。现在副本按**视图序**排好再传。

再省一笔：预填只改元数据、不动名称，所以**只有按大小 / 时间排时才需要重排视图**
（`SortKey::Size | Modified`）；默认的按名称 / 类型排与元数据无关，那一趟
（大目录实测 43ms）直接省掉。

**实测（dev 档，`~/Downloads` = 27 512 条）：**

| 轮次 | 改前 | 改后 |
|---|---|---|
| 冷（首次） | 821ms | 738ms |
| **热（第 2 次起）** | **312ms** | **152ms** |

首屏 20 条的元数据仍然全部就位；主线程心跳（5ms 一拍、跑在 current_thread
runtime 上，即 main 线程）全程没有 >30ms 的缺口。

### (3) 那 152ms 里界面还是「没反应」——补上立即反馈

读目录期间 `directory` 还是**上一处**的内容，UI 因此一动不动。新增：

* `AppState::opening_path()`：正在读取的目标（`None` = 空闲）；
* `AppEvent::OpeningChanged { path: Option<PathBuf> }`：开始 / 结束各发一条；
* 用 **RAII guard**（`OpeningGuard`）置位与收尾——`load_path` 里有七八处 `?` 与
  `return Err`，漏一处就是一条永远不消失的「正在读取 …」；
* UI：侧栏高亮**先跟过去**（`Panel.opening` 优先于 `path`）+ 中央顶部一行
  「正在读取 X…」。

与 `directory` 分开存是刻意的：读失败时把它清掉就行，界面自然回到原来的位置，
而不是「先切过去、再弹一条错误」。

### 小结：三笔账的归宿

| 现象 | 根因 | 落在哪 |
|---|---|---|
| 进目录一帧冻 50–400ms | 图标在渲染闭包里同步取 | 查表 + 图标泵（§4） |
| 还是卡一下 | 泵一拍 40 张，主线程连着忙 | 3ms 时间配额（§5） |
| 仍有一半 | 编码占 70% 却在主线程 | 像素交给后台编码（本节 1） |
| 切换本身 300ms | 缓存预填查了全量 id | 只填首屏（本节 2） |
| 那 152ms 观感是「死机」 | 界面停在上一处 | opening 立即反馈（本节 3） |
