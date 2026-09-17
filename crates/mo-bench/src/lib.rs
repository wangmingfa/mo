//! mo-bench：Mo 的性能基准。
//!
//! 用 `cargo bench` 运行。这里的数字是「大目录优化」的效果基线：
//! 改动 `mo-fs` / `mo-core` / `mo-cache` / `mo-thumbnails` 后，
//! 对比基准变化即可判断是变快还是变慢。
//!
//! ```bash
//! cargo bench                       # 全部基准
//! cargo bench --bench directory     # 只看目录相关
//! ```
