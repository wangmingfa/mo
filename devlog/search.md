# devlog · 全局搜索与索引

## 1. 索引为什么必须落盘，以及「增量」到底怎么做（2026-09-22）

### 现象

⌘F 打开全局搜索，除非先在命令面板里手动跑一次「索引当前目录」，否则什么都搜不到；
重开应用又归零。

### 根因

索引是**内存库**（`FileIndex::open_in_memory()`），而建索引的唯一入口是
`CommandId::IndexCurrent`——用户不主动触发就永远是空的。

### 修法

- **落盘**：`FileIndex::open()` 打开 `~/Library/Caches/mo/search.sqlite`
  （`AppState::index_path`，认 `MO_CACHE_DIR`）。索引是**可重建的缓存**而不是用户
  数据，所以放缓存目录不放配置目录；打不开就退回内存库（搜索退化成本次会话内有效，
  但绝不影响启动）。开了 WAL + `synchronous = NORMAL`。
- **启动自举** `AppState::ensure_index_started`（`mo-ui::run` 里调一次，后台跑）：
  空库 → 爬主目录（限深 6、限 10 万条）；非空 → 只重爬超过 6 小时没刷过的根
  （`indexed_roots` 表记着每个根上次爬完的时刻，重爬是幂等 `upsert`，等于刷新）。
- **进过的目录顺带进索引** `AppState::note_visited`（`load_path` 成功且在本地时调）：
  限深 3、限 2 万条，10 分钟内不重复爬。
- **当前层的 watcher 事件同步索引**：`sync_index_created/removed/renamed`。事件本来
  就在听（watcher 监听当前目录），一条 SQL 的事。

### 为什么不做递归 watcher

`notify` 在 macOS 上是**每目录一个 fd**：监听整棵主目录要几十万个 fd，不现实。所以跨
目录的新鲜度靠「你进过的目录爬一遍」+「启动时补过期的根」兜住，代价与「用户实际去过的
地方」成正比，而不是与磁盘大小成正比。

### ⚠️ 爬取必须带上限

第一版没限：某个集成测试 `open_directory` 到主目录，`note_visited` 一次爬了 **10 万条**
（三层就把 `~/Library`、`node_modules` 那类子树全吃进去了）。所以 `crawl` 多了 `limit`
参数（`0` = 不限，只有用户手动触发的那条命令用），后台自举一律限量。索引是可增量补齐的：
少爬一点下次再补，好过把机器占死几分钟。

### ⚠️ 索引落盘后，测试必须隔离

`MO_CACHE_DIR` 把库钉到临时目录（见 `mo-app/tests/global_index.rs::use_temp_index`）。
不隔离的话，测试会把 `/var/folders/.../T/mo-*` 这些临时路径写进**开发者机器上的真实
索引**，而且它们不会自己消失——更糟的是搜索有 `LIMIT 50`，历史条目会把本次要断言的
结果挤掉（`productivity.rs` 那条就是这样红的）。

### ⚠️ 索引锁不能长时间堵住异步 worker

爬一个大目录期间索引锁被持有几分钟。`sync_index_*` 因此走 `spawn_blocking`（派发即忘）
而不是在 async 上下文里就地 `lock()`——后者会把整条 tokio worker 卡住。
`std::mem::drop(self.spawn_blocking(..))` 是 clippy 的 `let_underscore_future` 要求的
显式丢弃写法。
