//! mo-core：Mo 文件管理器的领域核心。
//!
//! 本 crate **不依赖 GPUI / 任何 UI 框架**，只描述文件系统相关的领域模型：
//! `FileId`、`Entry`、`Directory`、`SelectionModel`、`NavigationState`、
//! `FileCommand`、`AppEvent` / `EventBus` 与统一错误类型。
//!
//! 因为与 UI 解耦，核心逻辑可以单独单元测试，并且在以后替换搜索引擎、
//! 缓存机制甚至文件系统实现时，UI 都不需要改动。

pub mod command;
pub mod directory;
pub mod entry;
pub mod error;
pub mod event;
pub mod file_id;
pub mod metadata;
pub mod navigation;
pub mod rename;
pub mod selection;
pub mod view;

#[cfg(test)]
mod tests;

pub use command::FileCommand;
pub use directory::{Directory, DirectoryError, DirectoryId};
pub use entry::{Entry, EntryKind, LightEntry, MetadataState, ThumbnailState};
pub use error::MoError;
pub use event::{AppEvent, EventBus};
pub use file_id::FileId;
pub use metadata::{FileMetadata, Permissions};
pub use navigation::{Location, NavigationState};
pub use rename::{is_noop, plan_batch_rename, RenameSpec};
pub use selection::SelectionModel;
pub use view::{DirectoryView, SortKey};
