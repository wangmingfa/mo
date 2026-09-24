//! 暂存区（Staging Tray）：跨目录收集待处理的文件。
//!
//! 真实场景是「从八个文件夹各挑三个文件，最后一起拷走」——剪贴板一次只装得住
//! 一处的选择，换目录就把上一处覆盖了；而暂存区是**累积**的。
//!
//! 与内部剪贴板（[`crate::Clipboard`]）的区别，一句话：
//!
//! * 剪贴板是「**替换** + 立刻粘贴」：`⌘C` 会清掉上一次的内容，语义是搬运；
//! * 暂存区是「**追加** + 攒够再做」：可以连着在好几个目录里收集，最后统一
//!   复制 / 移动 / 压缩 / 删除，也可以逐条挑掉不要的。
//!
//! 作用域是**进程级**（见 [`staging`]）：一个窗口里开两个窗格、四个标签页，
//! 收集的是同一份清单——这正是它解决「跨目录挑文件」的前提。测试要隔离就传
//! 一份独占的进来（[`crate::AppState::with_staging`]），理由同
//! [`crate::session_registry`]。

use parking_lot::Mutex as PlMutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 暂存区里的一条：一个被收集的路径 + 它是从哪儿收集来的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedEntry {
    pub path: PathBuf,
    /// 收集时所在的目录。抽屉里以灰字显示——十来个文件来自五六个目录时，
    /// 没了它就是一堆同名 `IMG_0042.jpg` 分不清谁是谁。
    pub from: PathBuf,
    pub is_dir: bool,
}

/// 暂存区本体：一条有序清单（收集顺序即显示顺序）。
#[derive(Debug, Default)]
pub struct Staging {
    entries: Vec<StagedEntry>,
}

impl Staging {
    pub fn new() -> Self {
        Self::default()
    }

    /// 全部条目（按收集顺序）。
    pub fn entries(&self) -> &[StagedEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 收集一批路径，返回**真正新增**的条数。
    ///
    /// 同一路径重复收集只留一条：按住 ⌘ 多选时不该因为「选过一次又选了一次」
    /// 就在清单里出现两行（粘贴时就会变成「复制成两份、第二份改名」）。
    pub fn collect(&mut self, from: PathBuf, items: Vec<(PathBuf, bool)>) -> usize {
        let mut added = 0;
        for (path, is_dir) in items {
            if self.entries.iter().any(|e| e.path == path) {
                continue;
            }
            self.entries.push(StagedEntry {
                path,
                from: from.clone(),
                is_dir,
            });
            added += 1;
        }
        added
    }

    /// 移除一条（行尾的 ×）。返回是否真的移掉了。
    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.path != path);
        self.entries.len() != before
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// 进程级的那一份暂存区（所有窗口 / 窗格 / 标签页共享）。
pub fn staging() -> Arc<PlMutex<Staging>> {
    static ST: std::sync::OnceLock<Arc<PlMutex<Staging>>> = std::sync::OnceLock::new();
    ST.get_or_init(|| Arc::new(PlMutex::new(Staging::new())))
        .clone()
}
