use std::path::PathBuf;

/// 一个导航位置（当前阶段用路径表示，后续可由 `FileId` 加强）。
pub type Location = PathBuf;

/// 导航状态：当前位置、前进/后退栈、书签、最近访问。
///
/// 与具体 UI Layout 解耦——以后加 Tabs / Split View / Pinned Locations 都不会影响这里。
#[derive(Debug, Clone, Default)]
pub struct NavigationState {
    pub current: Option<Location>,
    pub back_stack: Vec<Location>,
    pub forward_stack: Vec<Location>,
    pub bookmarks: Vec<Location>,
    pub recent: Vec<Location>,
}

impl NavigationState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 访问一个新位置，维护前进/后退栈。
    pub fn visit(&mut self, loc: Location) {
        if let Some(cur) = &self.current {
            if cur != &loc {
                self.back_stack.push(cur.clone());
            }
        }
        self.forward_stack.clear();
        self.push_recent(&loc);
        self.current = Some(loc);
    }

    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    pub fn go_back(&mut self) -> Option<Location> {
        let prev = self.back_stack.pop()?;
        if let Some(cur) = self.current.take() {
            self.forward_stack.push(cur);
        }
        self.current = Some(prev.clone());
        Some(prev)
    }

    pub fn go_forward(&mut self) -> Option<Location> {
        let next = self.forward_stack.pop()?;
        if let Some(cur) = self.current.take() {
            self.back_stack.push(cur);
        }
        self.current = Some(next.clone());
        Some(next)
    }

    pub fn add_bookmark(&mut self, loc: Location) {
        if !self.bookmarks.contains(&loc) {
            self.bookmarks.push(loc);
        }
    }

    fn push_recent(&mut self, loc: &Location) {
        self.recent.retain(|r| r != loc);
        self.recent.insert(0, loc.clone());
        self.recent.truncate(20);
    }
}
