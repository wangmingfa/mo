# 引擎逻辑与测试方法的坑

日期：2026-09-17。

---

## 1. 两个有序 Map 拼 keys 后 `dedup_by` 失效（mo-diff 文件夹比较计数翻倍）

* **现象**：文件夹比较时同名条目被处理两次，identical 计数翻倍。
* **根因**：`ls.keys().chain(rs.keys())` 得到的序列里，两侧行的重复项**不相邻**（左侧行先全出来，右侧行再全出来），`dedup_by` 只去相邻重复，等于没去。
* **修法**：并集老实用 `BTreeSet`：`let names: BTreeSet<_> = ls.keys().chain(rs.keys()).collect();`
* **守卫**：`mo-diff` 的 `identical_trees` 等测试锁计数。

## 2. headless 布局测试方法（不依赖 GPU / 真实窗口）

* **通道**：`gpui-kit` 开 `test-support` feature → `TestAppContext::single()` + `cx.open_window(...)` + `VisualTestContext::from_window` + `window.render_frame(cx)`。
* **探针**：元素挂 `.debug_selector(|| "name".into())`（release no-op），测试里 `cx.debug_bounds("name")` 拿**真实布局矩形**。
* **可断言的东西**：区域几何关系（上下左右贴合、高度恒等、右缘对齐）、元素存在性。
* **量不到的东西**：AppKit 画的内容（如红绿灯）——它们不参与 GPUI 布局，只能靠常量推导 + 实机验收。
* **推荐实践**：布局回归（如「地址栏必须在工具栏内」「大小列贴行右缘」）都写成探针测试；改样式前先跑一遍留下基线。

## 3. 像素级验收（截图测量）的坑

* **教训**：在沙箱/自动化环境里靠 `screencapture` + Pillow 测窗口位置不可靠：后台起的 GUI 进程可能静默死、窗口可能在另一块屏或另一个 Space、截屏色彩配置文件会让颜色阈值偏移。
* **结论**：**优先读框架源码拿公式**（如红绿灯定位公式），实测只作为最后的验证手段，且要在用户前台会话里做。

## 4. 零依赖引擎的分层收益

* **实践**：mo-diff / mo-core 等纯逻辑 crate 不依赖 GPUI / tokio，测试秒级跑完且可在 CI 任意平台跑。UI 相关的坑全部隔离在 mo-ui 的探针测试里。
