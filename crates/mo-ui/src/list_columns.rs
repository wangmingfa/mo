//! 列表视图的**列模型**：顺序 + 宽度 + 排序键映射。
//!
//! 与 `columns.rs`（Miller 列视图）无关，只服务于 `ViewMode::List` 的四列表格。
//!
//! 设计要点：
//! * 列的**顺序**存在 `Vec<ColId>` 里，表头与数据行都按它渲染，因此拖动排序
//!   不需要动任何渲染逻辑，只改这一个 `Vec`；
//! * 列**宽度**存在定长数组里（按 `ColId as usize` 索引），`Name` 列是弹性的
//!   （宽度 0 表示「吃掉剩余空间」），其余列固定宽、可拖拽调整；
//! * 本模块不依赖 GPUI，宽度钳制 / 顺序调整都能直接单测。

use mo_core::SortKey;

/// 列表视图的一列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColId {
    Name,
    Date,
    Size,
    Kind,
}

impl ColId {
    /// 默认列顺序（也是拖动前的初始顺序）。
    pub const ALL: [ColId; 4] = [ColId::Name, ColId::Date, ColId::Size, ColId::Kind];

    /// 表头文案，同时用于测试里的定位。
    pub fn title(self) -> &'static str {
        match self {
            ColId::Name => "名称",
            ColId::Date => "修改日期",
            ColId::Size => "大小",
            ColId::Kind => "种类",
        }
    }

    /// 点击这一列表头时对应的排序键。
    pub fn sort_key(self) -> SortKey {
        match self {
            ColId::Name => SortKey::Name,
            ColId::Date => SortKey::Modified,
            ColId::Size => SortKey::Size,
            ColId::Kind => SortKey::Kind,
        }
    }

    /// 默认宽度（`Name` 为 0：弹性，不吃固定宽）。
    pub fn default_width(self) -> f32 {
        match self {
            ColId::Name => 0.0,
            ColId::Date => 150.0,
            ColId::Size => 80.0,
            ColId::Kind => 100.0,
        }
    }

    /// 是否弹性列（弹性列不可拖拽调宽，否则会把别的列挤没）。
    pub fn is_flex(self) -> bool {
        matches!(self, ColId::Name)
    }

    /// 是否右对齐（数值 / 日期 / 种类列右对齐，名称左对齐）。
    pub fn is_right_aligned(self) -> bool {
        !matches!(self, ColId::Name)
    }

    pub fn index(self) -> usize {
        match self {
            ColId::Name => 0,
            ColId::Date => 1,
            ColId::Size => 2,
            ColId::Kind => 3,
        }
    }
}

/// 列宽下限 / 上限：太窄表头文字会被压没，太宽会把其它列挤出行外。
pub const MIN_COL_W: f32 = 60.0;
pub const MAX_COL_W: f32 = 420.0;

/// 列表视图的列布局：顺序 + 宽度。
#[derive(Debug, Clone)]
pub struct ColumnLayout {
    /// 列顺序（即渲染顺序）。
    pub order: Vec<ColId>,
    widths: [f32; 4],
}

impl Default for ColumnLayout {
    fn default() -> Self {
        Self {
            order: ColId::ALL.to_vec(),
            widths: [
                ColId::Name.default_width(),
                ColId::Date.default_width(),
                ColId::Size.default_width(),
                ColId::Kind.default_width(),
            ],
        }
    }
}

impl ColumnLayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// 某一列的当前宽度（弹性列为 0）。
    pub fn width(&self, col: ColId) -> f32 {
        self.widths[col.index()]
    }

    /// 设置列宽（自动钳制到位）。
    pub fn set_width(&mut self, col: ColId, w: f32) {
        if col.is_flex() {
            return;
        }
        self.widths[col.index()] = w.clamp(MIN_COL_W, MAX_COL_W);
    }

    /// 列在顺序里的下标。
    pub fn index_of(&self, col: ColId) -> Option<usize> {
        self.order.iter().position(|c| *c == col)
    }

    /// 把 `from` 位置的列移到 `to` 位置。
    ///
    /// 下标越界或原地移动都是 no-op（拖动落点算出来恰好是原位时很常见）。
    pub fn move_col(&mut self, from: usize, to: usize) -> bool {
        if from >= self.order.len() || to >= self.order.len() || from == to {
            return false;
        }
        let col = self.order.remove(from);
        self.order.insert(to, col);
        true
    }

    /// 一次性写入多列宽度（拖分隔线时两侧一起改，见 [`divider_resize`]）。
    pub fn set_widths(&mut self, widths: &[(ColId, f32)]) {
        for (col, w) in widths {
            self.set_width(*col, *w);
        }
    }

    /// 分隔线画在某一列的**左缘**，`col` 是它**右侧**的那一列。
    ///
    /// 返回 `None` 表示 `col` 排在第一位——它左边没有分隔线（也就没有把手）。
    /// 弹性列不参与固定宽调整，所以它所在的那一侧是 `None`：分隔线两边
    /// 至少有一侧是固定宽的列，拖动才有意义。
    pub fn divider_anchor(&self, col: ColId) -> Option<DividerAnchor> {
        let i = self.index_of(col)?;
        let left = *self.order.get(i.checked_sub(1)?)?;
        Some(DividerAnchor {
            left: (!left.is_flex()).then(|| (left, self.width(left))),
            right: (!col.is_flex()).then(|| (col, self.width(col))),
        })
    }
}

/// 一条分隔线两侧的**起始**宽度快照（鼠标按下时取一次）。
///
/// 拖动过程中按「相对按下点的总位移」重算两侧宽度，而不是每次移动都做增量——
/// 增量叠加钳制会把线拖偏（触到下限后继续拖，松回来时线回不到鼠标下）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DividerAnchor {
    /// 分隔线**左侧**的列及其起始宽度（该列是弹性列时为 `None`）。
    pub left: Option<(ColId, f32)>,
    /// 分隔线**右侧**的列及其起始宽度（该列是弹性列时为 `None`）。
    pub right: Option<(ColId, f32)>,
}

/// 拖动分隔线：按总位移 `dx` 算出两侧列的新宽度。
///
/// 规则是「**左列变宽 `dx`、右列变窄 `dx`**」：两侧同时动，被拖的那条线才会
/// 严格跟着鼠标走。只动一侧是不够的——布局里恒有一个弹性列吃掉剩余空间，
/// 任何固定宽列的**远侧**边缘都被容器边缘钉住，单边改宽度只会让它不动的
/// 那条边挪位，分隔线反而不会跟着鼠标。
///
/// 两侧必须共用**同一个钳制后的位移**：各自独立钳制的话，一侧先触到上下限时
/// 另一侧还在动，线就会悄悄偏离鼠标。
///
/// 弹性列不设固定宽，它那一侧交给邻居独自承担（此时线依然跟着鼠标走：
/// 弹性列吃掉/吐出剩余空间，边界正好由邻居的宽度决定）。
pub fn divider_resize(anchor: DividerAnchor, dx: f32) -> Vec<(ColId, f32)> {
    // 先把位移钳到「两侧都还合法」的区间。
    let mut lo = f32::NEG_INFINITY;
    let mut hi = f32::INFINITY;
    if let Some((_, w)) = anchor.left {
        lo = lo.max(MIN_COL_W - w);
        hi = hi.min(MAX_COL_W - w);
    }
    if let Some((_, w)) = anchor.right {
        // 右侧列新宽度 = w - dx，同样要落在 [MIN, MAX] 内。
        lo = lo.max(w - MAX_COL_W);
        hi = hi.min(w - MIN_COL_W);
    }
    let d = dx.clamp(lo, hi);

    let mut out = Vec::new();
    if let Some((col, w)) = anchor.left {
        out.push((col, w + d));
    }
    if let Some((col, w)) = anchor.right {
        out.push((col, w - d));
    }
    out
}

/// 拖动换位的落点：指针落在某一列的中线之前 → 插到那一列前面。
///
/// `centers` 是各列**中点**的 x 坐标（顺序即当前列序）。抽成纯函数是为了
/// 能直接单测——真实渲染里这些中点来自 GPUI prepaint 回写的 bounds。
pub fn drop_index(centers: &[f32], x: f32) -> Option<usize> {
    if centers.is_empty() {
        return None;
    }
    for (i, mid) in centers.iter().enumerate() {
        if x < *mid {
            return Some(i);
        }
    }
    Some(centers.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_order_and_widths() {
        let l = ColumnLayout::new();
        assert_eq!(l.order, ColId::ALL.to_vec());
        assert_eq!(l.width(ColId::Name), 0.0, "名称列应为弹性列");
        assert_eq!(l.width(ColId::Date), 150.0);
        assert_eq!(l.width(ColId::Size), 80.0);
        assert_eq!(l.width(ColId::Kind), 100.0);
    }

    #[test]
    fn width_is_clamped() {
        let mut l = ColumnLayout::new();
        l.set_width(ColId::Size, 1.0);
        assert_eq!(l.width(ColId::Size), MIN_COL_W, "过窄应钳到下限");
        l.set_width(ColId::Size, 9999.0);
        assert_eq!(l.width(ColId::Size), MAX_COL_W, "过宽应钳到上限");
        // 弹性列忽略宽度设置。
        l.set_width(ColId::Name, 300.0);
        assert_eq!(l.width(ColId::Name), 0.0);
    }

    #[test]
    fn move_col_reorders_and_ignores_noop() {
        let mut l = ColumnLayout::new();
        // 把「种类」拖到最前。
        assert!(l.move_col(3, 0));
        assert_eq!(
            l.order,
            vec![ColId::Kind, ColId::Name, ColId::Date, ColId::Size]
        );
        // 原地 / 越界都是 no-op。
        assert!(!l.move_col(0, 0));
        assert!(!l.move_col(9, 0));
        assert!(!l.move_col(0, 9));
    }

    #[test]
    fn sort_keys_are_mapped() {
        assert_eq!(ColId::Name.sort_key(), SortKey::Name);
        assert_eq!(ColId::Date.sort_key(), SortKey::Modified);
        assert_eq!(ColId::Size.sort_key(), SortKey::Size);
        assert_eq!(ColId::Kind.sort_key(), SortKey::Kind);
    }

    /// 落点判定：中点左侧插到该列前，越过后插到最后，空布局返回 None。
    #[test]
    fn drop_index_uses_column_midpoints() {
        let centers = [100.0, 300.0, 500.0, 700.0];
        assert_eq!(drop_index(&centers, 0.0), Some(0), "最左 → 插到第一列前");
        assert_eq!(drop_index(&centers, 250.0), Some(1), "越过第一列中点");
        assert_eq!(drop_index(&centers, 501.0), Some(3), "越过第三列中点");
        assert_eq!(drop_index(&centers, 9999.0), Some(3), "最右 → 落到最后");
        assert_eq!(drop_index(&[], 10.0), None, "没有列时不判定");
    }

    /// 分隔线画在列的**左缘**：第一列没有，其余列都有；弹性列那一侧为 `None`。
    #[test]
    fn divider_anchor_skips_the_first_column_and_the_flex_one() {
        let l = ColumnLayout::new();
        assert_eq!(l.divider_anchor(ColId::Name), None, "第一列左侧没有分隔线");

        // 名称列是弹性列：它和日期之间的那条线只能调日期自己。
        let a = l.divider_anchor(ColId::Date).expect("日期列左侧应有分隔线");
        assert_eq!(a.left, None, "左侧是弹性列，不参与固定宽调整");
        assert_eq!(a.right, Some((ColId::Date, 150.0)));

        // 其余两条线两侧都是固定列。
        let a = l.divider_anchor(ColId::Size).expect("大小列左侧应有分隔线");
        assert_eq!(a.left, Some((ColId::Date, 150.0)));
        assert_eq!(a.right, Some((ColId::Size, 80.0)));

        // 把名称列拖到最后一行：分隔线跟着列序走。
        let mut l = ColumnLayout::new();
        assert!(l.move_col(0, 3));
        assert_eq!(
            l.order,
            vec![ColId::Date, ColId::Size, ColId::Kind, ColId::Name]
        );
        assert_eq!(
            l.divider_anchor(ColId::Date),
            None,
            "此时日期列排第一，没有左缘分隔线"
        );
        let a = l.divider_anchor(ColId::Name).expect("名称列左侧应有分隔线");
        assert_eq!(
            a.left,
            Some((ColId::Kind, 100.0)),
            "弹性列在右侧时调左边的列"
        );
        assert_eq!(a.right, None);
    }

    /// 拖分隔线：左列变宽 dx、右列变窄 dx（对称改宽度，线才跟着鼠标走）。
    #[test]
    fn divider_resize_moves_both_neighbours() {
        let anchor = DividerAnchor {
            left: Some((ColId::Date, 150.0)),
            right: Some((ColId::Size, 80.0)),
        };
        assert_eq!(
            divider_resize(anchor, 20.0),
            vec![(ColId::Date, 170.0), (ColId::Size, 60.0)],
            "右拖：左列 +20、右列 -20"
        );
        assert_eq!(
            divider_resize(anchor, -30.0),
            vec![(ColId::Date, 120.0), (ColId::Size, 110.0)],
            "左拖：左列 -30、右列 +30"
        );
    }

    /// 只有一侧是固定宽列时（另一侧是弹性列），由它独自承担位移。
    #[test]
    fn divider_resize_falls_back_to_the_single_fixed_side() {
        let anchor = DividerAnchor {
            left: None,
            right: Some((ColId::Date, 150.0)),
        };
        assert_eq!(
            divider_resize(anchor, 30.0),
            vec![(ColId::Date, 120.0)],
            "左侧是弹性列：右拖只能让右列变窄"
        );

        let anchor = DividerAnchor {
            left: Some((ColId::Kind, 100.0)),
            right: None,
        };
        assert_eq!(
            divider_resize(anchor, 30.0),
            vec![(ColId::Kind, 130.0)],
            "右侧是弹性列：右拖由左列独自变宽"
        );
    }

    /// 两侧共用同一个钳制后的位移：一侧触限时另一侧同步停住，线不会偏。
    #[test]
    fn divider_resize_clamps_both_sides_together() {
        // 右列已经在下限：往哪边拖，右列都不能再窄。
        let anchor = DividerAnchor {
            left: Some((ColId::Date, 150.0)),
            right: Some((ColId::Size, MIN_COL_W)),
        };
        assert_eq!(
            divider_resize(anchor, 50.0),
            vec![(ColId::Date, 150.0), (ColId::Size, MIN_COL_W)],
            "右列已到下限：右拖不再改变任何宽度"
        );
        assert_eq!(
            divider_resize(anchor, -25.0),
            vec![(ColId::Date, 125.0), (ColId::Size, 85.0)],
            "左拖不受影响"
        );

        // 左列已经在上限：右拖同样无效。
        let anchor = DividerAnchor {
            left: Some((ColId::Date, MAX_COL_W)),
            right: Some((ColId::Size, 80.0)),
        };
        assert_eq!(
            divider_resize(anchor, 40.0),
            vec![(ColId::Date, MAX_COL_W), (ColId::Size, 80.0)],
            "左列已到上限：右拖不再改变任何宽度"
        );
    }
}
