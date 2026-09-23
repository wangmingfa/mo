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

/// 排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

impl SortDir {
    pub fn flipped(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }

    /// 每个排序键的「自然」方向：首次点击该列表头时用这个。
    ///
    /// 名称 / 种类从小到大，大小 / 修改时间从大到小——与访达 / 资源管理器的
    /// 默认观感一致（点击同一个表头再来一次就翻转）。
    pub fn natural_for(key: SortKey) -> Self {
        match key {
            SortKey::Name | SortKey::Kind => SortDir::Asc,
            SortKey::Size | SortKey::Modified => SortDir::Desc,
        }
    }
}

/// 列表视图的分组方式（任务⑤：按类型 / 按日期）。
///
/// 分组**只作用于列表视图**：网格 / 画廊 / 列视图的几何与虚拟化都以
/// 「条目 = 一行」为前提，插分组头会把行流变成「头 + 条目」两种行，
/// 那三种视图不消费它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grouping {
    #[default]
    None,
    /// 按类型：文件夹 / 图片 / 文稿 / 影音 / 压缩包 / 其他。
    Kind,
    /// 按修改时间：今天 / 最近 7 天 / 更早。
    Date,
}

impl Grouping {
    /// 配置文件里用的稳定键名（同 `ViewMode::key` 的约定）。
    pub fn key(self) -> &'static str {
        match self {
            Grouping::None => "none",
            Grouping::Kind => "kind",
            Grouping::Date => "date",
        }
    }

    /// 键名 → 分组方式；不认识返回 `None`（调用方回落默认）。
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "none" => Some(Grouping::None),
            "kind" => Some(Grouping::Kind),
            "date" => Some(Grouping::Date),
            _ => None,
        }
    }

    /// 循环切换的下一档（无 → 类型 → 日期 → 无）。
    pub fn next(self) -> Self {
        match self {
            Grouping::None => Grouping::Kind,
            Grouping::Kind => Grouping::Date,
            Grouping::Date => Grouping::None,
        }
    }
}

/// 分组键。分组顺序固定（见 [`DirectoryView::rebuild`]），条目 → 键的映射
/// 在这里收口；键的**中文标题**由 UI 层格式化（mo-core 不放文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKey {
    // —— 按类型（Kind）——
    Folder,
    Image,
    Document,
    Media,
    Archive,
    Other,
    // —— 按日期（Date）——
    Today,
    Week,
    Earlier,
}

/// 列表视图的一行：分组头或条目。
///
/// `Entry` 存的是 **visible 下标**（展示顺序位），不是 `entries` 下标——
/// 这样 `row_entry(i)` 的返回值可以直接喂给 `index_at` / 选择 / 预览等
/// 一切以 visible 位为参数的既有路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Header(GroupKey),
    Entry(usize),
}

/// 类型分组的后缀 → 键。顺序即匹配表；`Folder` 在调用处按 `kind` 判，不走这里。
fn kind_group_of(ext: &str) -> GroupKey {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff" | "heic" | "svg" => {
            GroupKey::Image
        }
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "csv" | "rtf"
        | "pages" | "numbers" | "key" | "odt" | "ods" => GroupKey::Document,
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" => {
            GroupKey::Media
        }
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "dmg" | "iso" => {
            GroupKey::Archive
        }
        _ => GroupKey::Other,
    }
}

/// 日期分组的桶：24 小时内 = 今天，7 天内 = 最近 7 天，其余 = 更早。
/// 元数据未加载（`Loading` / `None`）时落「更早」——回填后下一次 rebuild 自然归位。
fn date_group_of(e: &Entry, now: std::time::SystemTime) -> GroupKey {
    let modified = match &e.metadata {
        crate::entry::MetadataState::Loaded(m) => m.modified,
        _ => None,
    };
    let Some(t) = modified else {
        return GroupKey::Earlier;
    };
    let age = now.duration_since(t).unwrap_or(std::time::Duration::ZERO);
    if age <= std::time::Duration::from_secs(24 * 3600) {
        GroupKey::Today
    } else if age <= std::time::Duration::from_secs(7 * 24 * 3600) {
        GroupKey::Week
    } else {
        GroupKey::Earlier
    }
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
    sort_dir: SortDir,
    /// 过滤词（小写后的子串匹配）。`None` 表示不过滤。
    filter: Option<String>,
    /// `entries` 中可见条目的下标，顺序即展示顺序。
    visible: Vec<usize>,
    /// 列表分组方式。
    grouping: Grouping,
    /// 分组后的行流（`Header` + `Entry` 交错）。**只在 `grouping != None` 时非空**；
    /// 空表示「无分组」，行流就等于 `visible` 本身。组内顺序 = 现有排序序。
    rows: Vec<Row>,
}

impl DirectoryView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sort(&self) -> SortKey {
        self.sort
    }

    pub fn sort_dir(&self) -> SortDir {
        self.sort_dir
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// 是否处于过滤状态。
    pub fn is_filtered(&self) -> bool {
        self.filter.is_some()
    }

    /// 当前分组方式。
    pub fn grouping(&self) -> Grouping {
        self.grouping
    }

    /// 设置分组方式并重建（同排序 / 过滤一样走一次 O(n log n) 重建）。
    pub fn set_grouping(&mut self, grouping: Grouping, entries: &[Entry]) {
        self.grouping = grouping;
        self.rebuild(entries);
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

    /// 列表视图的**行数**：无分组 = 条目数；有分组 = 条目数 + 分组头数。
    pub fn row_count(&self) -> usize {
        if self.rows.is_empty() {
            self.visible.len()
        } else {
            self.rows.len()
        }
    }

    /// 第 `i` 行的条目 visible 位；分组头行返回 `None`。
    pub fn row_entry(&self, i: usize) -> Option<usize> {
        match self.rows.get(i) {
            Some(Row::Entry(pos)) => Some(*pos),
            Some(Row::Header(_)) => None,
            // 无分组（rows 空）：行即条目。
            None => self.visible.get(i).map(|_| i),
        }
    }

    /// 第 `i` 行的分组头键；条目行返回 `None`。
    pub fn row_header(&self, i: usize) -> Option<GroupKey> {
        match self.rows.get(i) {
            Some(Row::Header(k)) => Some(*k),
            _ => None,
        }
    }

    /// 条目 visible 位 → 行下标（供键盘移动 / type-ahead 的滚动跟随）。
    /// 无分组时两者相等；有分组时线性扫（键盘导航本身已是 O(n)，同量级）。
    pub fn pos_to_row(&self, pos: usize) -> usize {
        if self.rows.is_empty() {
            return pos;
        }
        self.rows
            .iter()
            .position(|r| matches!(r, Row::Entry(p) if *p == pos))
            .unwrap_or(pos)
    }

    /// 设置排序方式（键 + 方向）并重建索引。
    pub fn set_sort(&mut self, sort: SortKey, dir: SortDir, entries: &[Entry]) {
        self.sort = sort;
        self.sort_dir = dir;
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
        let dir = self.sort_dir;
        self.visible.sort_by(|&a, &b| {
            let ea = &entries[a];
            let eb = &entries[b];
            // 目录永远排在前（不受排序方向影响，与访达一致）。
            match (ea.kind.is_dir(), eb.kind.is_dir()) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }
            // 主键按「升序」比较，再由方向翻转；次序键（名称）恒升序，
            // 这样降序时同值条目仍是自然顺序，而不是被一起倒过来。
            let primary = match sort {
                SortKey::Name => natural_cmp(&ea.name, &eb.name),
                SortKey::Size => ea.size_or_zero().cmp(&eb.size_or_zero()),
                SortKey::Modified => ea.modified_or_zero().cmp(&eb.modified_or_zero()),
                SortKey::Kind => ea.extension().cmp(&eb.extension()),
            };
            let primary = match dir {
                SortDir::Asc => primary,
                SortDir::Desc => primary.reverse(),
            };
            primary.then_with(|| natural_cmp(&ea.name, &eb.name))
        });
        self.rebuild_rows(entries);
    }

    /// 从已排好的 `visible` 构建（或清掉）分组行流。
    ///
    /// 组**顺序固定**（类型：文件夹 → 图片 → 文稿 → 影音 → 压缩包 → 其他；
    /// 日期：今天 → 最近 7 天 → 更早），组**内**保持现有排序序（稳定分桶）——
    /// 这样分组与排序是叠加关系而不是替换：切分组不重排组内条目。
    /// 空组不出头（没有内容的组不占行）。
    fn rebuild_rows(&mut self, entries: &[Entry]) {
        self.rows.clear();
        if self.grouping == Grouping::None {
            return;
        }
        const KIND_ORDER: [GroupKey; 6] = [
            GroupKey::Folder,
            GroupKey::Image,
            GroupKey::Document,
            GroupKey::Media,
            GroupKey::Archive,
            GroupKey::Other,
        ];
        const DATE_ORDER: [GroupKey; 3] = [GroupKey::Today, GroupKey::Week, GroupKey::Earlier];
        let order: &[GroupKey] = match self.grouping {
            Grouping::Kind => &KIND_ORDER,
            Grouping::Date => &DATE_ORDER,
            Grouping::None => unreachable!("上面已 return"),
        };
        let now = std::time::SystemTime::now();
        // 单趟分桶：每个桶收集属于该组的 visible 位（桶内顺序 = 现有排序序）。
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
        for (pos, &vi) in self.visible.iter().enumerate() {
            let e = &entries[vi];
            let key = match self.grouping {
                Grouping::Kind => {
                    if e.kind.is_dir() {
                        GroupKey::Folder
                    } else {
                        let ext = e.extension();
                        kind_group_of(&ext)
                    }
                }
                Grouping::Date => date_group_of(e, now),
                Grouping::None => unreachable!("上面已 return"),
            };
            let slot = order
                .iter()
                .position(|&k| k == key)
                .unwrap_or(order.len() - 1);
            buckets[slot].push(pos);
        }
        for (slot, bucket) in buckets.iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            self.rows.push(Row::Header(order[slot]));
            self.rows.extend(bucket.iter().map(|&pos| Row::Entry(pos)));
        }
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

#[cfg(test)]
mod group_tests {
    use super::*;
    use crate::entry::{Entry, EntryKind, MetadataState};
    use crate::file_id::FileId;
    use crate::metadata::{FileMetadata, Permissions};
    use std::time::{Duration, SystemTime};

    fn e(name: &str, dir: bool) -> Entry {
        Entry::new(
            FileId::new(1, name.len() as u128),
            name.to_string(),
            if dir {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            std::path::PathBuf::from(name),
        )
    }

    /// 带修改时间（age 秒前）的条目。
    fn aged(name: &str, age_secs: u64) -> Entry {
        let mut e = e(name, false);
        e.metadata = MetadataState::Loaded(FileMetadata {
            size: 1,
            modified: Some(SystemTime::now() - Duration::from_secs(age_secs)),
            created: None,
            permissions: Permissions::default(),
        });
        e
    }

    /// 类型分组：组顺序固定、空组不出头、组内保持排序序。
    #[test]
    fn kind_grouping_inserts_headers_in_fixed_order() {
        // 排序后：sub（目录）在最前，其余按名升序。
        let entries = vec![
            e("sub", true),
            e("a.png", false),
            e("b.txt", false),
            e("c.zip", false),
            e("d.xyz", false),
        ];
        let mut v = DirectoryView::new();
        v.set_grouping(Grouping::Kind, &entries);

        assert_eq!(v.row_count(), 5 + 5, "5 条目 + 5 个非空组头");
        // 逐行核对：头行 row_entry = None，条目行给出正确的 visible 位。
        let expect_rows: Vec<(Option<GroupKey>, Option<usize>)> = vec![
            (Some(GroupKey::Folder), None),
            (None, Some(0)),
            (Some(GroupKey::Image), None),
            (None, Some(1)),
            (Some(GroupKey::Document), None),
            (None, Some(2)),
            (Some(GroupKey::Archive), None),
            (None, Some(3)),
            (Some(GroupKey::Other), None),
            (None, Some(4)),
        ];
        for (i, (hk, en)) in expect_rows.iter().enumerate() {
            assert_eq!(v.row_header(i), *hk, "第 {i} 行的组头不对");
            assert_eq!(v.row_entry(i), *en, "第 {i} 行的条目位不对");
        }
        // pos → row 映射（条目位 0..=4 → 1,3,5,7,9）。
        for (pos, row) in [(0usize, 1usize), (1, 3), (2, 5), (3, 7), (4, 9)] {
            assert_eq!(v.pos_to_row(pos), row, "条目位 {pos} 应在第 {row} 行");
        }
    }

    /// 日期分组：24h / 7d / 更早三桶；元数据未加载落「更早」。
    #[test]
    fn date_grouping_buckets_by_age() {
        let entries = vec![
            aged("fresh.txt", 3600),         // 1h → 今天
            aged("week.txt", 3 * 24 * 3600), // 3d → 最近 7 天
            aged("old.txt", 30 * 24 * 3600), // 30d → 更早
            e("loading.txt", false),         // 无元数据 → 更早
        ];
        let mut v = DirectoryView::new();
        v.set_grouping(Grouping::Date, &entries);

        // 名字排序后：fresh / loading / old / week（visible 位 0..3）。
        // 分组只改行流不改桶内顺序：Today 只含 fresh(0)，
        // Week 只含 week.txt(3)，Earlier 含 old(2) 与 loading(1)。
        let expect_rows: Vec<(Option<GroupKey>, Option<usize>)> = vec![
            (Some(GroupKey::Today), None),
            (None, Some(0)),
            (Some(GroupKey::Week), None),
            (None, Some(3)),
            (Some(GroupKey::Earlier), None),
            (None, Some(1)),
            (None, Some(2)),
        ];
        for (i, (hk, en)) in expect_rows.iter().enumerate() {
            assert_eq!(v.row_header(i), *hk, "第 {i} 行的组头不对");
            assert_eq!(v.row_entry(i), *en, "第 {i} 行的条目位不对");
        }
    }

    /// 无分组时行 API 与 visible 恒等——既有路径不能因为接了这套接口而变。
    #[test]
    fn ungrouped_rows_are_identity() {
        let entries = vec![e("a", false), e("b", false)];
        let mut v = DirectoryView::new();
        v.rebuild(&entries);
        assert_eq!(v.row_count(), 2);
        for i in 0..2 {
            assert_eq!(v.row_entry(i), Some(i));
            assert_eq!(v.row_header(i), None);
            assert_eq!(v.pos_to_row(i), i);
        }
    }

    /// 过滤 + 分组叠加：分组吃的是过滤后的可见集。
    #[test]
    fn grouping_composes_with_filter() {
        let entries = vec![e("sub", true), e("a.png", false), e("b.txt", false)];
        let mut v = DirectoryView::new();
        v.set_filter(Some("t".to_string()), &entries); // 命中 txt（t 在 txt）与 sub？
        v.set_grouping(Grouping::Kind, &entries);
        let total_entries: usize = (0..v.row_count()).filter_map(|i| v.row_entry(i)).count();
        assert_eq!(total_entries, v.len(), "行流里的条目数 = 可见条目数");
    }
}
