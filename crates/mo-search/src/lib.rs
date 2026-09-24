//! mo-search：文件索引与全局搜索。
//!
//! 把「全局搜索」和「目录过滤」区分开：
//!
//! * 目录内的「输入即过滤」不需要索引，直接对当前目录条目做内存子串匹配
//!   （见 `mo-core::DirectoryView`）；
//! * **全局搜索**跨整个文件系统，需要一份本地索引——这就是本 crate 的职责。
//!
//! 索引用 SQLite 存储（path / name_lower / size / modified / is_dir），
//! 搜索走子串匹配 + 相关度排序。索引可由 [`crawl`] 从某个根目录递归建立。

mod content;
mod crawl;
mod index;

pub use content::{
    search_content, ContentQuery, ContentReport, FileHit, LineHit, DEFAULT_MAX_FILE_BYTES,
};
pub use crawl::crawl;
pub use index::{FileIndex, SearchError, SearchHit};
