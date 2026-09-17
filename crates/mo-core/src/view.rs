use crate::entry::Entry;

/// 排序依据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// 目录在前，再按名称（大小写不敏感、自然序）。
    #[default]
    Name,
    /// 目录在前，再按大小降序。
    Size,
    /// 目录在前，再按修改时间降序。
    Modified,
    /// 目录在前，再按类型（后缀）。
    Kind,
}

/// 目录视图：排序 + 名称过滤后的**可见索引**。
///
/// 大目录优化的关键之一：过滤 / 排序不复制条目，只维护一份 `Vec<usize>`
/// 指向 `Directory::entries` 的下标。这样：
///   * 切换过滤词只是重建索引（几万条也就毫秒级），不克隆 `Entry`；
///   * 虚拟化列表用 `visible.len()` 作总数，用 `entries[visible[i]]` 取行，
///     过滤态下依然只渲染可见区。
#[derive(Debug, Clone, Default)]
pub struct DirectoryView {
    sort: SortKey,
    /// 过滤词（小写后的子串匹配）。`None` 表示不过滤。
    filter: Option<String>,
    /// `entries` 中可见条目的下标，顺序即展示顺序。
    visible: Vec<usize>,
}

impl DirectoryView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sort(&self) -> SortKey {
        self.sort
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// 是否处于过滤状态。
    pub fn is_filtered(&self) -> bool {
        self.filter.is_some()
    }

    /// 可见条目的下标（顺序即展示顺序）。
    pub fn visible_indices(&self) -> &[usize] {
        &self.visible
    }

    /// 可见条目数量（虚拟化列表的 `item_count`）。
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// 第 `i` 个可见条目在 `entries` 中的下标。
    pub fn index_at(&self, i: usize) -> Option<usize> {
        self.visible.get(i).copied()
    }

    /// 设置排序方式并重建索引。
    pub fn set_sort(&mut self, sort: SortKey, entries: &[Entry]) {
        if self.sort != sort {
            self.sort = sort;
        }
        self.rebuild(entries);
    }

    /// 设置过滤词并重建索引。空字符串视为不过滤。
    pub fn set_filter(&mut self, query: Option<String>, entries: &[Entry]) {
        self.filter = query.filter(|q| !q.trim().is_empty()).map(|q| {
            let q = q.trim().to_lowercase();
            q
        });
        self.rebuild(entries);
    }

    /// 重建可见索引：先过滤，再排序。
    ///
    /// 对几十万条目而言这也是毫秒级操作，但排序比较里避免做任何分配
    /// （不建临时 String、不格式化路径），因此放在后台线程执行即可。
    pub fn rebuild(&mut self, entries: &[Entry]) {
        self.visible.clear();
        match self.filter.as_ref() {
            Some(q) => {
                let needle = q.as_str();
                self.visible.extend(
                    entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| contains_fold(&e.name, needle))
                        .map(|(i, _)| i),
                );
            }
            None => self.visible.extend(0..entries.len()),
        }

        let sort = self.sort;
        self.visible.sort_by(|&a, &b| {
            let ea = &entries[a];
            let eb = &entries[b];
            // 目录永远排在前。
            match (ea.kind.is_dir(), eb.kind.is_dir()) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }
            match sort {
                SortKey::Name => natural_cmp(&ea.name, &eb.name),
                SortKey::Size => eb
                    .size_or_zero()
                    .cmp(&ea.size_or_zero())
                    .then_with(|| natural_cmp(&ea.name, &eb.name)),
                SortKey::Modified => eb
                    .modified_or_zero()
                    .cmp(&ea.modified_or_zero())
                    .then_with(|| natural_cmp(&ea.name, &eb.name)),
                SortKey::Kind => ea
                    .extension()
                    .cmp(&eb.extension())
                    .then_with(|| natural_cmp(&ea.name, &eb.name)),
            }
        });
    }
}

/// 自然序比较：`file2` 排在 `file10` 前面，数字段按数值而非字典序。
///
/// 完全基于字节扫描、**零分配**：排序时每比较一次就分配两个 `Vec<char>`
/// 会让一万条目的排序从几毫秒涨到几十毫秒，是典型的热路径陷阱。
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);

    while i < ab.len() && j < bb.len() {
        if ab[i].is_ascii_digit() && bb[j].is_ascii_digit() {
            let si = i;
            let sj = j;
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            // 跳过前导零，让 "01" 与 "1" 数值相等。
            let (mut pi, mut pj) = (si, sj);
            while pi + 1 < i && ab[pi] == b'0' {
                pi += 1;
            }
            while pj + 1 < j && bb[pj] == b'0' {
                pj += 1;
            }
            // 位数不同直接定序（等价于数值比较，且不会溢出）。
            let la = i - pi;
            let lb = j - pj;
            if la != lb {
                return la.cmp(&lb);
            }
            // 位数相同则字典序即数值序，直接按字节比。
            let ord = ab[pi..i].cmp(&bb[pj..j]);
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            // 非数字：按字符比较（ASCII 折叠大小写；非 ASCII 直接比码点）。
            let ca = a[i..].chars().next().unwrap();
            let cb = b[j..].chars().next().unwrap();
            let fa = ca.to_ascii_lowercase();
            let fb = cb.to_ascii_lowercase();
            if fa != fb {
                return fa.cmp(&fb);
            }
            i += ca.len_utf8();
            j += cb.len_utf8();
        }
    }
    ab.len().cmp(&bb.len())
}

/// 不区分大小写的子串匹配。
///
/// 查询串是纯 ASCII 时走零分配的字节路径——过滤一万条目时，
/// 每条都 `to_lowercase()` 会白白分配一万个 String。
fn contains_fold(name: &str, needle_lower: &str) -> bool {
    if needle_lower.is_ascii() {
        let n = needle_lower.as_bytes();
        if n.len() > name.len() {
            return false;
        }
        // UTF-8 具有自同步性：按字节滑窗比较不会跨字符误匹配。
        name.as_bytes()
            .windows(n.len())
            .any(|w| w.eq_ignore_ascii_case(n))
    } else {
        name.to_lowercase().contains(needle_lower)
    }
}
